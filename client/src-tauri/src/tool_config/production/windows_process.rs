const WINDOWS_PROCESS_IMAGE_BUFFER_CHARS: usize = 32_768;

struct WindowsOwnedHandle(windows_sys::Win32::Foundation::HANDLE);

impl WindowsOwnedHandle {
    fn raw(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.0
    }
}

impl Drop for WindowsOwnedHandle {
    fn drop(&mut self) {
        // SAFETY: every instance owns one successful Win32 handle acquisition and is dropped
        // exactly once. CloseHandle accepts both process and Tool Help snapshot handles.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[derive(Clone, Debug)]
struct WindowsProcessEntry {
    pid: u32,
    parent_pid: u32,
    name: String,
}

#[derive(Debug)]
struct WindowsProcessProbe {
    identity: ToolProcessIdentity,
    package_family_name: Option<String>,
    command_line: Option<String>,
}

fn normalized_windows_process_path(path: &Path) -> String {
    let mut normalized = path
        .display()
        .to_string()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase();
    if let Some(without_prefix) = normalized.strip_prefix(r"\\?\") {
        normalized = without_prefix.to_string();
    }
    normalized
}

fn windows_path_is_within_root(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|remainder| remainder.starts_with('\\'))
}

fn normalized_windows_command_line(value: &str) -> String {
    value.replace('/', "\\").to_ascii_lowercase()
}

fn windows_command_line_matches_owner(command_line: &str, owner: &str) -> bool {
    !owner.is_empty() && normalized_windows_command_line(command_line).contains(owner)
}

fn windows_descendant_process_ids(
    roots: &BTreeSet<u32>,
    entries: &[WindowsProcessEntry],
) -> BTreeSet<u32> {
    let mut process_ids = roots.clone();
    loop {
        let before = process_ids.len();
        for entry in entries {
            if process_ids.contains(&entry.parent_pid) {
                process_ids.insert(entry.pid);
            }
        }
        if process_ids.len() == before {
            return process_ids;
        }
    }
}

impl ToolProcessScope {
    fn matches_windows_probe(&self, probe: &WindowsProcessProbe) -> bool {
        if probe
            .package_family_name
            .as_ref()
            .map(|family| family.to_ascii_lowercase())
            .is_some_and(|family| self.package_family_names.contains(&family))
        {
            return true;
        }

        let path = normalized_windows_process_path(Path::new(&probe.identity.executable_path));
        if self.exact_executable_paths.contains(&path)
            || self
                .install_roots
                .iter()
                .any(|root| windows_path_is_within_root(&path, root))
            || probe.command_line.as_ref().is_some_and(|command_line| {
                self.command_line_markers
                    .iter()
                    .any(|owner| windows_command_line_matches_owner(command_line, owner))
            })
        {
            return true;
        }

        self.allow_name_fallback
            && Path::new(&probe.identity.executable_path)
                .file_name()
                .and_then(|value| value.to_str())
                .map(normalized_process_name)
                .is_some_and(|name| self.executable_names.contains(&name))
    }

}

fn windows_process_snapshot(scope: &ToolProcessScope) -> Result<ToolProcessSnapshot> {
    if scope.executable_names.is_empty() && !scope.has_concrete_owner() {
        return Ok(ToolProcessSnapshot::scoped(
            Vec::new(),
            BTreeMap::new(),
            scope.clone(),
        ));
    }

    let deadline = Instant::now() + TOOL_PROCESS_SNAPSHOT_TIMEOUT;
    let entries = windows_process_entries()?;
    ensure_windows_process_snapshot_deadline(deadline)?;
    let parent_process_ids = entries
        .iter()
        .map(|entry| (entry.pid, entry.parent_pid))
        .collect::<BTreeMap<_, _>>();
    let entry_by_pid = entries
        .iter()
        .map(|entry| (entry.pid, entry))
        .collect::<HashMap<_, _>>();

    let mut probes = BTreeMap::<u32, WindowsProcessProbe>::new();
    let mut root_process_ids = BTreeSet::new();
    for entry in entries.iter().filter(|entry| {
        scope
            .executable_names
            .contains(&normalized_process_name(&entry.name))
    }) {
        ensure_windows_process_snapshot_deadline(deadline)?;
        let Some(probe) = windows_process_probe_by_pid(entry.pid).with_context(|| {
            format!(
                "TOOL_CONFIG_PROCESS_PROBE_FAILED: read native identity for PID {}",
                entry.pid
            )
        })? else {
            continue;
        };
        ensure_windows_process_snapshot_deadline(deadline)?;

        // A PID can be reused between the Tool Help snapshot and OpenProcess. Accept only the
        // executable name read from the live process handle, not the possibly stale snapshot row.
        let current_name = Path::new(&probe.identity.executable_path)
            .file_name()
            .and_then(|value| value.to_str())
            .map(normalized_process_name);
        if !current_name
            .as_ref()
            .is_some_and(|name| scope.executable_names.contains(name))
        {
            continue;
        }
        if scope.matches_windows_probe(&probe) {
            root_process_ids.insert(entry.pid);
            probes.insert(entry.pid, probe);
        }
    }

    if !root_process_ids.is_empty() {
        // The selected executable/package establishes the root ownership. Once a root is known,
        // its descendants belong to the same runtime tree even when a helper is hosted by a
        // generic executable outside the installation directory.
        for pid in windows_descendant_process_ids(&root_process_ids, &entries) {
            if probes.contains_key(&pid) {
                continue;
            }
            let Some(entry) = entry_by_pid.get(&pid) else {
                continue;
            };
            ensure_windows_process_snapshot_deadline(deadline)?;
            let Some(probe) = windows_process_probe_by_pid(entry.pid).with_context(|| {
                format!(
                    "TOOL_CONFIG_PROCESS_PROBE_FAILED: read descendant identity for PID {}",
                    entry.pid
                )
            })? else {
                continue;
            };
            probes.insert(entry.pid, probe);
        }
    }

    if scope.has_concrete_path_owner() {
        // An installation may host helpers with different executable names or helpers detached
        // from the original parent. Query the remaining accessible processes and keep only those
        // inside the exact installation/package scope. Failures for unrelated processes are
        // ignored; matching-name roots above remain fail-closed.
        for entry in &entries {
            if probes.contains_key(&entry.pid) {
                continue;
            }
            ensure_windows_process_snapshot_deadline(deadline)?;
            let Ok(Some(probe)) = windows_process_probe_by_pid(entry.pid) else {
                continue;
            };
            if scope.matches_windows_probe(&probe) {
                probes.insert(entry.pid, probe);
            }
        }
    }

    Ok(ToolProcessSnapshot::scoped(
        probes
            .into_values()
            .map(|probe| probe.identity)
            .collect(),
        parent_process_ids,
        scope.clone(),
    ))
}

fn windows_process_snapshot_by_names(
    match_names: &HashSet<String>,
) -> Result<Vec<ToolProcessIdentity>> {
    let scope = ToolProcessScope {
        executable_names: match_names.iter().cloned().collect(),
        allow_name_fallback: true,
        ..Default::default()
    };
    Ok(windows_process_snapshot(&scope)?.processes)
}

fn windows_process_entries() -> Result<Vec<WindowsProcessEntry>> {
    use windows_sys::Win32::{
        Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
    };

    // SAFETY: TH32CS_SNAPPROCESS ignores the PID argument and returns an owned snapshot handle.
    let raw_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw_snapshot == INVALID_HANDLE_VALUE {
        return Err(anyhow!(
            "TOOL_CONFIG_PROCESS_PROBE_FAILED: CreateToolhelp32Snapshot: {}",
            std::io::Error::last_os_error()
        ));
    }
    let snapshot = WindowsOwnedHandle(raw_snapshot);
    let mut entry = PROCESSENTRY32W::default();
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

    // SAFETY: entry has the required dwSize and remains valid for the duration of enumeration.
    if unsafe { Process32FirstW(snapshot.raw(), &mut entry) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
            return Ok(Vec::new());
        }
        return Err(anyhow!(
            "TOOL_CONFIG_PROCESS_PROBE_FAILED: Process32FirstW: {error}"
        ));
    }

    let mut entries = Vec::new();
    loop {
        let name_len = entry
            .szExeFile
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(entry.szExeFile.len());
        if name_len > 0 {
            entries.push(WindowsProcessEntry {
                pid: entry.th32ProcessID,
                parent_pid: entry.th32ParentProcessID,
                name: String::from_utf16_lossy(&entry.szExeFile[..name_len]),
            });
        }

        // SAFETY: the snapshot and initialized entry remain valid until enumeration completes.
        if unsafe { Process32NextW(snapshot.raw(), &mut entry) } != 0 {
            continue;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
            break;
        }
        return Err(anyhow!(
            "TOOL_CONFIG_PROCESS_PROBE_FAILED: Process32NextW: {error}"
        ));
    }
    Ok(entries)
}

fn ensure_windows_process_snapshot_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(anyhow!(
            "TOOL_CONFIG_PROCESS_PROBE_TIMEOUT: native Windows process snapshot exceeded {} seconds",
            TOOL_PROCESS_SNAPSHOT_TIMEOUT.as_secs()
        ));
    }
    Ok(())
}

fn windows_open_process(pid: u32, access: u32) -> Result<Option<WindowsOwnedHandle>> {
    use windows_sys::Win32::{
        Foundation::ERROR_INVALID_PARAMETER,
        System::Threading::OpenProcess,
    };

    // SAFETY: OpenProcess receives a concrete PID and returns an owned handle on success.
    let raw_process = unsafe { OpenProcess(access, 0, pid) };
    if raw_process.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(None);
        }
        return Err(anyhow!(
            "TOOL_CONFIG_PROCESS_IDENTITY_UNAVAILABLE: OpenProcess PID {pid}: {error}"
        ));
    }
    Ok(Some(WindowsOwnedHandle(raw_process)))
}

fn windows_process_probe_by_pid(pid: u32) -> Result<Option<WindowsProcessProbe>> {
    use windows_sys::Win32::System::Threading::{
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    let Some(process) = windows_open_process(
        pid,
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
    )? else {
        return Ok(None);
    };
    windows_process_probe_from_handle(process.raw(), pid)
}

fn windows_process_probe_from_handle(
    process: windows_sys::Win32::Foundation::HANDLE,
    pid: u32,
) -> Result<Option<WindowsProcessProbe>> {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetProcessTimes, QueryFullProcessImageNameW},
    };

    if windows_process_handle_exited(process)? {
        return Ok(None);
    }

    let mut path = vec![0_u16; WINDOWS_PROCESS_IMAGE_BUFFER_CHARS];
    let mut path_len = path.len() as u32;
    // SAFETY: path is a writable UTF-16 buffer and path_len contains its capacity in characters.
    if unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut path_len) } == 0 {
        let error = std::io::Error::last_os_error();
        windows_process_query_failure_or_exit(
            process,
            pid,
            "QueryFullProcessImageNameW",
            error.to_string(),
        )?;
        return Ok(None);
    }
    path.truncate(path_len as usize);
    let executable_path = String::from_utf16(&path).map_err(|error| {
        anyhow!(
            "TOOL_CONFIG_PROCESS_IDENTITY_UNAVAILABLE: PID {pid} executable path is not valid UTF-16: {error}"
        )
    })?;
    if executable_path.trim().is_empty() {
        windows_process_query_failure_or_exit(
            process,
            pid,
            "QueryFullProcessImageNameW",
            "returned an empty path".to_string(),
        )?;
        return Ok(None);
    }

    let mut creation_time = FILETIME::default();
    let mut exit_time = FILETIME::default();
    let mut kernel_time = FILETIME::default();
    let mut user_time = FILETIME::default();
    // SAFETY: all FILETIME pointers are valid writable values and the process handle was opened
    // with PROCESS_QUERY_LIMITED_INFORMATION.
    if unsafe {
        GetProcessTimes(
            process,
            &mut creation_time,
            &mut exit_time,
            &mut kernel_time,
            &mut user_time,
        )
    } == 0
    {
        let error = std::io::Error::last_os_error();
        windows_process_query_failure_or_exit(
            process,
            pid,
            "GetProcessTimes",
            error.to_string(),
        )?;
        return Ok(None);
    }

    let package_family_name = match windows_process_package_family_name(process) {
        Ok(value) => value,
        Err(error) => {
            windows_process_query_failure_or_exit(
                process,
                pid,
                "GetPackageFamilyName",
                error.to_string(),
            )?;
            return Ok(None);
        }
    };
    // Command-line ownership is an additional positive signal for hosted CLIs. Some protected
    // unrelated processes deny this optional query; their executable/package identity remains
    // usable and they must not make every tool operation fail.
    let command_line = windows_process_command_line(process).ok().flatten();
    if windows_process_handle_exited(process)? {
        return Ok(None);
    }
    Ok(Some(WindowsProcessProbe {
        identity: ToolProcessIdentity {
            pid,
            executable_path,
            start_identity: windows_filetime_identity(creation_time),
        },
        package_family_name,
        command_line,
    }))
}

fn windows_process_command_line(
    process: windows_sys::Win32::Foundation::HANDLE,
) -> Result<Option<String>> {
    use windows_sys::{
        Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation},
        Win32::Foundation::{STATUS_INFO_LENGTH_MISMATCH, UNICODE_STRING},
    };

    let mut required = 0_u32;
    // SAFETY: the first call is the documented size query. The process handle is live and the
    // output length pointer is writable.
    let status = unsafe {
        NtQueryInformationProcess(
            process,
            ProcessCommandLineInformation,
            std::ptr::null_mut(),
            0,
            &mut required,
        )
    };
    if status != STATUS_INFO_LENGTH_MISMATCH && status < 0 {
        return Err(anyhow!("NTSTATUS 0x{:08x}", status as u32));
    }
    if required < std::mem::size_of::<UNICODE_STRING>() as u32 {
        return Ok(None);
    }

    let word_size = std::mem::size_of::<usize>();
    let buffer_byte_len = (required as usize).div_ceil(word_size) * word_size;
    let mut buffer = vec![0_usize; buffer_byte_len / word_size];
    // SAFETY: buffer is writable for `required` bytes and the return-length pointer is valid.
    let status = unsafe {
        NtQueryInformationProcess(
            process,
            ProcessCommandLineInformation,
            buffer.as_mut_ptr().cast(),
            buffer_byte_len as u32,
            &mut required,
        )
    };
    if status < 0 {
        return Err(anyhow!("NTSTATUS 0x{:08x}", status as u32));
    }

    // SAFETY: the successful query wrote at least one UNICODE_STRING header into `buffer`.
    let command = unsafe { &*(buffer.as_ptr().cast::<UNICODE_STRING>()) };
    if command.Length == 0 || command.Buffer.is_null() {
        return Ok(None);
    }
    let byte_len = command.Length as usize;
    if byte_len % std::mem::size_of::<u16>() != 0 {
        return Err(anyhow!("Windows returned an odd UTF-16 command-line byte length"));
    }
    let buffer_start = buffer.as_ptr() as usize;
    let buffer_end = buffer_start.saturating_add(buffer_byte_len);
    let string_start = command.Buffer as usize;
    let string_end = string_start.saturating_add(byte_len);
    if string_start < buffer_start || string_end > buffer_end || string_end < string_start {
        return Err(anyhow!("Windows returned an out-of-range command-line buffer"));
    }
    // SAFETY: the pointer range was validated to lie inside `buffer`, is UTF-16 aligned by the
    // UNICODE_STRING contract, and contains Length / 2 code units.
    let utf16 = unsafe {
        std::slice::from_raw_parts(command.Buffer, byte_len / std::mem::size_of::<u16>())
    };
    let value = String::from_utf16(utf16)?;
    Ok((!value.trim().is_empty()).then_some(value))
}

fn windows_process_package_family_name(
    process: windows_sys::Win32::Foundation::HANDLE,
) -> Result<Option<String>> {
    use windows_sys::Win32::{
        Foundation::{APPMODEL_ERROR_NO_PACKAGE, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS},
        Storage::Packaging::Appx::GetPackageFamilyName,
    };

    let mut length = 0_u32;
    // SAFETY: a null buffer with a zero length is the documented size-query form.
    let status = unsafe { GetPackageFamilyName(process, &mut length, std::ptr::null_mut()) };
    if status == APPMODEL_ERROR_NO_PACKAGE {
        return Ok(None);
    }
    if status != ERROR_INSUFFICIENT_BUFFER || length == 0 {
        return Err(anyhow!("Windows error {status}"));
    }
    let mut buffer = vec![0_u16; length as usize];
    // SAFETY: buffer is writable for length UTF-16 code units.
    let status = unsafe { GetPackageFamilyName(process, &mut length, buffer.as_mut_ptr()) };
    if status == APPMODEL_ERROR_NO_PACKAGE {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(anyhow!("Windows error {status}"));
    }
    let value_len = buffer
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(buffer.len());
    let family_name = String::from_utf16(&buffer[..value_len])?;
    Ok((!family_name.trim().is_empty()).then_some(family_name))
}

fn windows_process_handle_exited(
    process: windows_sys::Win32::Foundation::HANDLE,
) -> Result<bool> {
    use windows_sys::Win32::{
        Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::WaitForSingleObject,
    };

    // SAFETY: process is an owned live process handle with PROCESS_SYNCHRONIZE access.
    match unsafe { WaitForSingleObject(process, 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        WAIT_FAILED => Err(anyhow!(
            "TOOL_CONFIG_PROCESS_IDENTITY_UNAVAILABLE: WaitForSingleObject: {}",
            std::io::Error::last_os_error()
        )),
        status => Err(anyhow!(
            "TOOL_CONFIG_PROCESS_IDENTITY_UNAVAILABLE: WaitForSingleObject returned unexpected status {status}"
        )),
    }
}

fn windows_process_query_failure_or_exit(
    process: windows_sys::Win32::Foundation::HANDLE,
    pid: u32,
    purpose: &str,
    detail: String,
) -> Result<()> {
    match windows_process_handle_exited(process) {
        Ok(true) => Ok(()),
        Ok(false) => Err(anyhow!(
            "TOOL_CONFIG_PROCESS_IDENTITY_UNAVAILABLE: {purpose} PID {pid}: {detail}"
        )),
        Err(exit_error) => Err(anyhow!(
            "TOOL_CONFIG_PROCESS_IDENTITY_UNAVAILABLE: {purpose} PID {pid}: {detail}; could not verify process exit: {exit_error:#}"
        )),
    }
}

fn windows_filetime_identity(filetime: windows_sys::Win32::Foundation::FILETIME) -> String {
    let raw = ((filetime.dwHighDateTime as u64) << 32) | filetime.dwLowDateTime as u64;
    format!("win-filetime:{raw:016x}")
}

fn windows_process_identity_matches(
    expected: &ToolProcessIdentity,
    current: &ToolProcessIdentity,
) -> bool {
    expected.pid == current.pid
        && expected.start_identity == current.start_identity
        && expected
            .executable_path
            .eq_ignore_ascii_case(&current.executable_path)
}

fn ensure_windows_process_revalidation_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(anyhow!(
            "TOOL_CONFIG_PROCESS_REVALIDATION_TIMEOUT: authorized process validation exceeded {} seconds before force kill",
            TOOL_PROCESS_SNAPSHOT_TIMEOUT.as_secs()
        ));
    }
    Ok(())
}

fn windows_process_is_descendant_of_authorized(
    pid: u32,
    authorized_process_ids: &BTreeSet<u32>,
    parent_process_ids: &BTreeMap<u32, u32>,
) -> bool {
    let mut current = pid;
    let mut visited = BTreeSet::new();
    while let Some(parent) = parent_process_ids.get(&current).copied() {
        if authorized_process_ids.contains(&parent) {
            return true;
        }
        if parent == 0 || !visited.insert(parent) {
            return false;
        }
        current = parent;
    }
    false
}

fn windows_process_depth(pid: u32, parent_process_ids: &BTreeMap<u32, u32>) -> usize {
    let mut current = pid;
    let mut visited = BTreeSet::new();
    let mut depth = 0;
    while let Some(parent) = parent_process_ids.get(&current).copied() {
        if parent == 0 || !visited.insert(parent) {
            break;
        }
        depth += 1;
        current = parent;
    }
    depth
}

fn terminate_authorized_process_trees(
    tool: &str,
    authorized: &ToolProcessSnapshot,
) -> Result<()> {
    if authorized.processes.is_empty() {
        return Ok(());
    }

    let validation_deadline = Instant::now() + TOOL_PROCESS_SNAPSHOT_TIMEOUT;
    let mut expected = authorized.processes.clone();
    let mut parent_process_ids = authorized.parent_process_ids.clone();
    if !authorized.scope.executable_names.is_empty() {
        let current = windows_tool_process_snapshot_for_scope(tool, &authorized.scope)
            .with_context(|| {
                format!(
                    "TOOL_CONFIG_PROCESS_PROBE_FAILED: refresh authorized {} process tree",
                    tool_runtime_display_name(tool)
                )
            })?;
        let authorized_process_ids = authorized.process_ids();
        expected.extend(
            current
                .processes
                .iter()
                .filter(|process| {
                    !authorized_process_ids.contains(&process.pid)
                        && windows_process_is_descendant_of_authorized(
                            process.pid,
                            &authorized_process_ids,
                            &current.parent_process_ids,
                        )
                })
                .cloned(),
        );
        parent_process_ids.extend(current.parent_process_ids);
    }
    expected.sort();
    expected.dedup();
    // Stop roots first so they cannot create replacement helpers while the captured descendants
    // are being terminated through their already-opened process handles.
    expected.sort_by_key(|process| windows_process_depth(process.pid, &parent_process_ids));

    use windows_sys::Win32::System::Threading::{
        TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE,
    };

    let mut targets = Vec::new();
    for expected_identity in expected {
        ensure_windows_process_revalidation_deadline(validation_deadline)?;
        let Some(process) = windows_open_process(
            expected_identity.pid,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
        )
        .with_context(|| {
            format!(
                "TOOL_CONFIG_PROCESS_PROBE_FAILED: open authorized PID {}",
                expected_identity.pid
            )
        })? else {
            continue;
        };
        let Some(current) = windows_process_probe_from_handle(process.raw(), expected_identity.pid)
            .with_context(|| {
                format!(
                    "TOOL_CONFIG_PROCESS_PROBE_FAILED: revalidate authorized PID {}",
                    expected_identity.pid
                )
            })?
        else {
            continue;
        };
        if !windows_process_identity_matches(&expected_identity, &current.identity) {
            return Err(process_replacement_error(
                tool,
                &format!(
                    "authorized PID {} changed identity: expected {expected_identity:?}, found {:?}",
                    expected_identity.pid, current.identity
                ),
            ));
        }
        targets.push((expected_identity, process));
    }
    ensure_windows_process_revalidation_deadline(validation_deadline)?;

    // Every handle above refers to the exact process object that was confirmed. TerminateProcess
    // therefore cannot follow a reused PID or expand into an unconfirmed child tree.
    for (identity, process) in &targets {
        if windows_process_handle_exited(process.raw())? {
            continue;
        }
        // SAFETY: the handle was opened with PROCESS_TERMINATE and still owns the confirmed
        // process object even if its numeric PID is later reused.
        if unsafe { TerminateProcess(process.raw(), 1) } == 0
            && !windows_process_handle_exited(process.raw())?
        {
            return Err(anyhow!(
                "TOOL_CONFIG_PROCESS_FORCE_KILL_FAILED: TerminateProcess PID {}: {}",
                identity.pid,
                std::io::Error::last_os_error()
            ));
        }
    }

    let exit_deadline = Instant::now() + TOOL_PROCESS_CLOSE_TIMEOUT;
    loop {
        let mut still_running = Vec::new();
        for (identity, process) in &targets {
            if !windows_process_handle_exited(process.raw())? {
                still_running.push(identity.pid);
            }
        }
        if still_running.is_empty() {
            return Ok(());
        }
        if Instant::now() >= exit_deadline {
            return Err(anyhow!(
                "TOOL_CONFIG_PROCESS_FORCE_KILL_FAILED: authorized PIDs {still_running:?} remained after {} seconds",
                TOOL_PROCESS_CLOSE_TIMEOUT.as_secs()
            ));
        }
        thread::sleep(std::cmp::min(
            TOOL_PROCESS_POLL_INTERVAL,
            exit_deadline.saturating_duration_since(Instant::now()),
        ));
    }
}
