#[cfg(test)]
fn close_running_tool_before_config(tool: &str) -> Result<bool> {
    close_running_tool_before_config_inner(tool, None)
}

#[cfg(test)]
fn close_running_tool_before_config_inner(
    tool: &str,
    operation: Option<&ToolConfigOperationControl>,
) -> Result<bool> {
    if let Some(operation) = operation {
        ensure_tool_config_operation_active(operation, "process close check")?;
    }
    let started = Instant::now();
    if !tool_program_running(tool)
        .with_context(|| format!("检查 {} 运行状态", tool_runtime_display_name(tool)))?
    {
        return Ok(false);
    }
    remember_running_tool_program_path(tool)?;
    #[cfg(test)]
    run_test_before_authorized_close_hook();
    close_tool_program_instances(tool)
        .with_context(|| format!("关闭 {}", tool_runtime_display_name(tool)))?;
    wait_for_tool_program_state_until_inner(
        tool,
        false,
        tool_process_close_deadline(tool, started),
        "关闭",
        operation,
    )?;
    if let Some(operation) = operation {
        ensure_tool_config_operation_active(operation, "process close completion")?;
    }
    Ok(true)
}

fn close_authorized_tool_processes(
    tool: &str,
    authorized: &ToolProcessSnapshot,
    operation: &ToolConfigOperationControl,
) -> Result<bool> {
    if !authorized.running() {
        return Ok(false);
    }
    ensure_tool_config_operation_active(operation, "authorized process close")?;
    #[cfg(test)]
    let started = Instant::now();
    remember_running_tool_program_path(tool)?;
    #[cfg(test)]
    run_test_before_authorized_close_hook();

    #[cfg(test)]
    if test_tool_runtime_snapshot(tool).is_some() {
        if !ensure_authorized_process_snapshot_current(tool, authorized)? {
            return Ok(true);
        }
        test_tool_runtime_close(tool)
            .ok_or_else(|| anyhow!("TOOL_CONFIG_PROCESS_CLOSE_FAILED: missing test runtime"))?;
        let wait_result = wait_for_tool_program_state_until_inner(
            tool,
            false,
            tool_process_close_deadline(tool, started),
            "关闭",
            Some(operation),
        );
        if let Err(err) = wait_result {
            if tool_config_operation_was_interrupted(&err) {
                return Err(err);
            }
            return Ok(false);
        }
        ensure_tool_config_operation_active(operation, "authorized process close completion")?;
        return Ok(true);
    }

    // Windows revalidates all confirmed identities through native process handles, then sends
    // exact native termination calls. No unconfirmed descendant is reached by process name.
    #[cfg(target_os = "windows")]
    terminate_authorized_process_trees(tool, authorized)?;

    // Unix likewise completes identity/tree validation and all KILL signals
    // before starting a fresh five-second exit-confirmation window.
    #[cfg(not(target_os = "windows"))]
    {
        let validation_deadline = Instant::now() + TOOL_PROCESS_SNAPSHOT_TIMEOUT;
        terminate_authorized_process_trees(&authorized.processes, validation_deadline)?;
        let exit_deadline = tool_process_close_deadline(tool, Instant::now());
        for identity in &authorized.processes {
            ensure_tool_config_operation_active(operation, "authorized process identity recheck")?;
            if !wait_for_observed_process_exit(
                tool,
                identity,
                exit_deadline,
                operation,
            )? {
                return Ok(false);
            }
        }
    }

    wait_for_tool_processes_closed(tool, authorized, operation)
}

fn remaining_tool_process_snapshot(
    tool: &str,
    authorized: &ToolProcessSnapshot,
) -> Result<ToolProcessSnapshot> {
    #[cfg(target_os = "windows")]
    let remaining = if authorized.scope.executable_names.is_empty()
        && !authorized.scope.has_concrete_owner()
    {
        tool_process_snapshot(tool)
    } else {
        windows_tool_process_snapshot_for_scope(tool, &authorized.scope)
    };
    #[cfg(not(target_os = "windows"))]
    let remaining = {
        let _ = authorized;
        tool_process_snapshot(tool)
    };
    remaining.with_context(|| {
        format!(
            "TOOL_CONFIG_PROCESS_PROBE_FAILED: verify {} remained closed",
            tool_runtime_display_name(tool)
        )
    })
}

fn wait_for_tool_processes_closed(
    tool: &str,
    authorized: &ToolProcessSnapshot,
    operation: &ToolConfigOperationControl,
) -> Result<bool> {
    ensure_tool_config_operation_active(operation, "remaining process check")?;
    let remaining = remaining_tool_process_snapshot(tool, authorized)?;
    if !remaining.running() {
        return Ok(true);
    }

    // Late helpers can be shutting down after their parent exits. Give only these
    // observed identities a bounded passive wait; never terminate a new instance.
    // Poll PIDs, not the entire system, and use one deadline for the whole set.
    let deadline = Instant::now() + TOOL_PROCESS_CLOSE_TIMEOUT;
    for identity in &remaining.processes {
        if !wait_for_observed_process_exit(tool, identity, deadline, operation)? {
            eprintln!(
                "[tool-config] {tool} still running after close: {:?}",
                remaining.processes
            );
            return Ok(false);
        }
    }
    ensure_tool_config_operation_active(operation, "remaining process completion")?;
    // A launcher may have replaced a helper during the wait. A new snapshot must
    // still be empty before writing; no second kill or unbounded respawn loop.
    let remaining = remaining_tool_process_snapshot(tool, authorized)?;
    if remaining.running() {
        eprintln!(
            "[tool-config] {tool} restarted during close: {:?}",
            remaining.processes
        );
    }
    Ok(!remaining.running())
}

fn tool_config_operation_was_interrupted(error: &anyhow::Error) -> bool {
    error.to_string().contains("TOOL_CONFIG_OPERATION_CANCELLED")
}

#[cfg(test)]
fn ensure_authorized_process_snapshot_current(
    tool: &str,
    authorized: &ToolProcessSnapshot,
) -> Result<bool> {
    let current = tool_process_snapshot(tool).with_context(|| {
        format!(
            "TOOL_CONFIG_PROCESS_PROBE_FAILED: revalidate {} confirmation snapshot",
            tool_runtime_display_name(tool)
        )
    })?;
    if !current.running() {
        return Ok(false);
    }
    if &current != authorized {
        return Err(process_replacement_error(
            tool,
            &format!("expected {authorized:?}, found {current:?}"),
        ));
    }
    Ok(true)
}

#[cfg(test)]
fn authorized_process_identity_matches_snapshot(
    tool: &str,
    current: &ToolProcessSnapshot,
    authorized: &ToolProcessIdentity,
) -> Result<bool> {
    let Some(current_identity) = current
        .processes
        .iter()
        .find(|identity| identity.pid == authorized.pid)
    else {
        return Ok(false);
    };
    if current_identity == authorized {
        return Ok(true);
    }
    Err(process_replacement_error(
        tool,
        &format!(
            "authorized PID {} changed identity: expected {authorized:?}, found {current_identity:?}",
            authorized.pid
        ),
    ))
}

fn process_replacement_error(tool: &str, detail: &str) -> anyhow::Error {
    anyhow!(
        "TOOL_CONFIG_CONFIRMATION_PROCESS_REPLACED: {} confirmation no longer matches the process set; {detail}",
        tool_runtime_display_name(tool)
    )
}

#[cfg(not(target_os = "windows"))]
fn process_identity_by_pid_with_timeout(
    pid: u32,
    timeout: Duration,
) -> Result<Option<ToolProcessIdentity>> {
    unix_process_identity_by_pid_with_timeout(pid, timeout)
}

#[cfg(target_os = "windows")]
fn process_identity_by_pid_with_timeout(
    pid: u32,
    _timeout: Duration,
) -> Result<Option<ToolProcessIdentity>> {
    Ok(windows_process_probe_by_pid(pid)?.map(|probe| probe.identity))
}

fn wait_for_observed_process_exit(
    tool: &str,
    authorized: &ToolProcessIdentity,
    deadline: Instant,
    operation: &ToolConfigOperationControl,
) -> Result<bool> {
    loop {
        ensure_tool_config_operation_active(operation, "authorized process exit")?;
        let now = Instant::now();
        if now >= deadline {
            return Ok(false);
        }
        let remaining = deadline.saturating_duration_since(now);
        let Some(current) =
            process_identity_by_pid_with_timeout(authorized.pid, remaining).with_context(|| {
                format!(
                    "TOOL_CONFIG_PROCESS_PROBE_FAILED: verify PID {} exit",
                    authorized.pid
                )
            })?
        else {
            return Ok(true);
        };
        if current != *authorized {
            return Err(process_replacement_error(
                tool,
                &format!(
                    "authorized PID {} changed identity: expected {authorized:?}, found {current:?}",
                    authorized.pid
                ),
            ));
        }
        thread::sleep(std::cmp::min(
            TOOL_PROCESS_POLL_INTERVAL,
            deadline.saturating_duration_since(Instant::now()),
        ));
    }
}

#[cfg(not(target_os = "windows"))]
fn terminate_authorized_process_trees(
    identities: &[ToolProcessIdentity],
    deadline: Instant,
) -> Result<()> {
    for identity in identities {
        terminate_authorized_process_tree(identity, deadline)?;
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn terminate_authorized_process_tree(
    identity: &ToolProcessIdentity,
    deadline: Instant,
) -> Result<()> {
    let mut process_tree = unix_process_tree_identities(identity.pid, deadline)?;
    let Some(root) = process_tree
        .iter()
        .find(|current| current.pid == identity.pid)
    else {
        return Ok(());
    };
    if root != identity {
        return Err(anyhow!(
            "TOOL_CONFIG_CONFIRMATION_PROCESS_REPLACED: authorized PID {} changed immediately before force kill",
            identity.pid
        ));
    }
    // Once the confirmed snapshot is consumed, terminate the captured descendants and root
    // immediately instead of attempting a graceful exit.
    process_tree.reverse();
    signal_unix_process_identities(&process_tree, "-KILL", deadline)
}

#[cfg(not(target_os = "windows"))]
fn force_kill_remaining(deadline: Instant) -> Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(anyhow!(
            "TOOL_CONFIG_PROCESS_CLOSE_TIMEOUT: force-kill budget expired"
        ));
    }
    Ok(std::cmp::min(remaining, TOOL_PROCESS_FORCE_KILL_TIMEOUT))
}

#[cfg(not(target_os = "windows"))]
fn unix_process_tree_identities(
    root_pid: u32,
    deadline: Instant,
) -> Result<Vec<ToolProcessIdentity>> {
    let mut command = helper_command("ps");
    command.args(["-axo", "pid=,ppid="]);
    let output = run_process_probe_command_with_timeout(
        &mut command,
        force_kill_remaining(deadline)?,
        "ps authorized process tree",
    )?;
    let rows = std::str::from_utf8(&output.stdout)
        .context("TOOL_CONFIG_PROCESS_SNAPSHOT_PARSE_FAILED: process tree is not UTF-8")?;
    let mut children = HashMap::<u32, Vec<u32>>::new();
    for row in rows.lines().filter(|line| !line.trim().is_empty()) {
        let columns = row.split_whitespace().collect::<Vec<_>>();
        if columns.len() != 2 {
            return Err(anyhow!(
                "TOOL_CONFIG_PROCESS_SNAPSHOT_PARSE_FAILED: malformed process tree row: {row}"
            ));
        }
        let pid = columns[0].parse::<u32>()?;
        let parent = columns[1].parse::<u32>()?;
        children.entry(parent).or_default().push(pid);
    }
    let mut pids = vec![root_pid];
    let mut index = 0;
    while index < pids.len() {
        if let Some(descendants) = children.get(&pids[index]) {
            pids.extend(descendants.iter().copied());
        }
        index += 1;
    }
    let mut identities = Vec::with_capacity(pids.len());
    for pid in pids {
        if let Some(identity) = unix_process_identity_by_pid_with_timeout(
            pid,
            force_kill_remaining(deadline)?,
        )? {
            identities.push(identity);
        }
    }
    Ok(identities)
}

#[cfg(not(target_os = "windows"))]
fn unix_process_identity_by_pid_with_timeout(
    pid: u32,
    timeout: Duration,
) -> Result<Option<ToolProcessIdentity>> {
    let mut command = helper_command("ps");
    command.args(["-p", &pid.to_string(), "-o", "pid=,lstart=,comm="]);
    let output = run_helper_command_with_timeout(
        &mut command,
        timeout,
        &format!("ps PID {pid} identity"),
    )?;
    if output.status.code() == Some(1) && output.stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err(anyhow!(
            "TOOL_CONFIG_PROCESS_PROBE_FAILED: ps PID {pid} exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .context("TOOL_CONFIG_PROCESS_SNAPSHOT_PARSE_FAILED: PID identity is not UTF-8")?;
    let Some(row) = stdout.lines().find(|line| !line.trim().is_empty()) else {
        return Ok(None);
    };
    let identity = parse_unix_process_snapshot_row(row)?;
    #[cfg(target_os = "linux")]
    let identity = {
        let mut identity = identity;
        match fs::read_link(format!("/proc/{pid}/exe")) {
            Ok(path) => identity.executable_path = path.display().to_string(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                    "TOOL_CONFIG_PROCESS_IDENTITY_UNAVAILABLE: read executable path for PID {pid}"
                )
                })
            }
        }
        identity
    };
    Ok(Some(identity))
}

#[cfg(not(target_os = "windows"))]
fn signal_unix_process_identities(
    identities: &[ToolProcessIdentity],
    signal: &str,
    deadline: Instant,
) -> Result<()> {
    for identity in identities {
        let Some(current) = unix_process_identity_by_pid_with_timeout(
            identity.pid,
            force_kill_remaining(deadline)?,
        )? else {
            continue;
        };
        if current != *identity {
            continue;
        }
        let mut command = helper_command("kill");
        command.args([signal, &identity.pid.to_string()]);
        let output = run_helper_command_with_timeout(
            &mut command,
            force_kill_remaining(deadline)?,
            &format!("kill {signal} PID {}", identity.pid),
        )?;
        if !output.status.success()
            && unix_process_identity_by_pid_with_timeout(
                identity.pid,
                force_kill_remaining(deadline)?,
            )?
            .as_ref()
                == Some(identity)
        {
            return Err(anyhow!(
                "TOOL_CONFIG_PROCESS_CLOSE_FAILED: kill {signal} PID {} exited with {}: {}",
                identity.pid,
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }
    Ok(())
}

fn tool_config_operation_lock(tool: &str) -> Result<Arc<Mutex<()>>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_LOCK_REGISTRY_POISONED: tool configuration lock registry is poisoned"))?;
    Ok(locks
        .entry(tool.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone())
}

fn random_tool_config_secret(prefix: &str) -> Result<String> {
    let mut bytes = [0_u8; 32];
    SysRng
        .try_fill_bytes(&mut bytes)
        .context("operating system random generator unavailable")?;
    Ok(format!("{prefix}_{}", sha256_hex(&bytes)))
}

fn tool_config_confirmation_capabilities(
) -> &'static Mutex<HashMap<String, ToolConfigConfirmationCapability>> {
    static CAPABILITIES: OnceLock<Mutex<HashMap<String, ToolConfigConfirmationCapability>>> =
        OnceLock::new();
    CAPABILITIES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn issue_tool_config_confirmation_capability(
    tool: &str,
    action: ToolConfigAction,
    protocol: Option<&str>,
    restart: bool,
    context_digest: &str,
    process_snapshot: &ToolProcessSnapshot,
) -> Result<String> {
    issue_tool_config_confirmation_capability_with_ttl(
        tool,
        action,
        protocol,
        restart,
        context_digest,
        process_snapshot,
        TOOL_CONFIG_CONFIRMATION_TTL,
    )
}

fn issue_tool_config_confirmation_capability_with_ttl(
    tool: &str,
    action: ToolConfigAction,
    protocol: Option<&str>,
    restart: bool,
    context_digest: &str,
    process_snapshot: &ToolProcessSnapshot,
    ttl: Duration,
) -> Result<String> {
    issue_tool_config_confirmation_capability_for_mode(
        tool,
        action,
        protocol,
        restart,
        context_digest,
        process_snapshot,
        ToolConfigConfirmationMode::CloseAndContinue,
        ttl,
    )
}

fn issue_tool_config_force_continue_capability(
    tool: &str,
    action: ToolConfigAction,
    protocol: Option<&str>,
    restart: bool,
    context_digest: &str,
    process_snapshot: &ToolProcessSnapshot,
) -> Result<String> {
    issue_tool_config_confirmation_capability_for_mode(
        tool,
        action,
        protocol,
        restart,
        context_digest,
        process_snapshot,
        ToolConfigConfirmationMode::ForceContinue,
        TOOL_CONFIG_CONFIRMATION_TTL,
    )
}

fn issue_tool_config_confirmation_capability_for_mode(
    tool: &str,
    action: ToolConfigAction,
    protocol: Option<&str>,
    restart: bool,
    context_digest: &str,
    process_snapshot: &ToolProcessSnapshot,
    mode: ToolConfigConfirmationMode,
    ttl: Duration,
) -> Result<String> {
    let token = random_tool_config_secret("tcc")?;
    let now = Instant::now();
    let mut capabilities = tool_config_confirmation_capabilities()
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_CONFIRMATION_REGISTRY_POISONED"))?;
    capabilities.retain(|_, capability| capability.retain_until > now);
    capabilities.insert(
        token.clone(),
        ToolConfigConfirmationCapability {
            tool: tool.to_string(),
            action,
            protocol: protocol.map(str::to_string),
            restart,
            context_digest: context_digest.to_string(),
            process_snapshot: process_snapshot.clone(),
            mode,
            expires_at: now + ttl,
            retain_until: now + ttl + TOOL_CONFIG_OPERATION_RETENTION,
            state: ToolConfigConfirmationState::Pending,
        },
    );
    Ok(token)
}

fn consume_tool_config_confirmation_capability(
    token: &str,
    tool: &str,
    action: ToolConfigAction,
    protocol: Option<&str>,
    restart: bool,
    context_digest: &str,
) -> Result<ToolConfigConfirmationCapability> {
    let now = Instant::now();
    let mut capabilities = tool_config_confirmation_capabilities()
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_CONFIRMATION_REGISTRY_POISONED"))?;
    let capability = capabilities
        .get_mut(token)
        .ok_or_else(|| anyhow!("TOOL_CONFIG_CONFIRMATION_INVALID: token is unknown or replayed"))?;
    match capability.state {
        ToolConfigConfirmationState::Consumed => {
            return Err(anyhow!("TOOL_CONFIG_CONFIRMATION_REPLAYED"))
        }
        ToolConfigConfirmationState::Expired => {
            return Err(anyhow!("TOOL_CONFIG_CONFIRMATION_EXPIRED"))
        }
        ToolConfigConfirmationState::Pending if capability.expires_at <= now => {
            capability.state = ToolConfigConfirmationState::Expired;
            return Err(anyhow!("TOOL_CONFIG_CONFIRMATION_EXPIRED"));
        }
        ToolConfigConfirmationState::Pending => {}
    }
    if capability.tool != tool
        || capability.action != action
        || capability.protocol.as_deref() != protocol
        || capability.restart != restart
        || capability.context_digest != context_digest
    {
        return Err(anyhow!(
            "TOOL_CONFIG_CONFIRMATION_MISMATCH: token is not valid for {tool}/{}/protocol={protocol:?}/restart={restart}",
            action.as_str()
        ));
    }
    capability.state = ToolConfigConfirmationState::Consumed;
    Ok(capability.clone())
}

#[cfg(test)]
fn expire_test_confirmation_token(token: &str) {
    let mut capabilities = tool_config_confirmation_capabilities()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let capability = capabilities.get_mut(token).expect("confirmation token");
    capability.expires_at = Instant::now();
}

fn tool_config_operations() -> &'static Mutex<HashMap<String, Arc<ToolConfigOperationControl>>> {
    static OPERATIONS: OnceLock<Mutex<HashMap<String, Arc<ToolConfigOperationControl>>>> =
        OnceLock::new();
    OPERATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn create_tool_config_operation(
    tool: &str,
    action: ToolConfigAction,
    restart: bool,
) -> Result<ToolConfigOperationTicket> {
    create_tool_config_operation_control(tool, action, restart)
}

pub(crate) fn create_confirmed_tool_config_operation(
    tool: &str,
    action: ToolConfigAction,
    restart: bool,
) -> Result<ToolConfigOperationTicket> {
    create_tool_config_operation_control(tool, action, restart)
}

fn create_tool_config_operation_control(
    tool: &str,
    action: ToolConfigAction,
    restart: bool,
) -> Result<ToolConfigOperationTicket> {
    let operation_id = random_tool_config_secret("tco")?;
    let now = Instant::now();
    let operation = Arc::new(ToolConfigOperationControl {
        id: operation_id.clone(),
        tool: tool.to_string(),
        action,
        restart,
        retain_until: Mutex::new(now + TOOL_CONFIG_OPERATION_RETENTION),
        cancelled: AtomicBool::new(false),
        state: Mutex::new(ToolConfigOperationState::Pending),
        progress: Mutex::new(ToolConfigOperationProgress::default()),
        state_changed: Condvar::new(),
        cancel_requested: tokio::sync::Notify::new(),
    });
    let mut operations = tool_config_operations()
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_REGISTRY_POISONED"))?;
    operations.retain(|_, existing| {
        let active = existing
            .state
            .lock()
            .map(|state| !state.terminal())
            .unwrap_or(true);
        let retained = existing
            .retain_until
            .lock()
            .map(|retain_until| *retain_until > now)
            .unwrap_or(true);
        active || retained
    });
    operations.insert(operation_id.clone(), operation);
    Ok(ToolConfigOperationTicket { operation_id })
}

pub(crate) fn start_tool_config_operation(
    operation_id: &str,
    tool: &str,
    action: ToolConfigAction,
    restart: bool,
) -> Result<Arc<ToolConfigOperationControl>> {
    let operation = tool_config_operations()
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_REGISTRY_POISONED"))?
        .get(operation_id)
        .cloned()
        .ok_or_else(|| anyhow!("TOOL_CONFIG_OPERATION_INVALID: unknown operation id"))?;
    if operation.tool != tool || operation.action != action || operation.restart != restart {
        return Err(anyhow!(
            "TOOL_CONFIG_OPERATION_MISMATCH: operation is not valid for {tool}/{}/restart={restart}",
            action.as_str()
        ));
    }
    let mut state = operation
        .state
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_STATE_POISONED"))?;
    if *state != ToolConfigOperationState::Pending {
        return Err(anyhow!(
            "TOOL_CONFIG_OPERATION_REPLAYED: operation is {}",
            state.as_str()
        ));
    }
    if operation.cancelled.load(Ordering::Acquire) {
        operation.cancelled.store(true, Ordering::Release);
        *state = ToolConfigOperationState::Cancelled;
        operation.state_changed.notify_all();
        return Err(anyhow!(
            "TOOL_CONFIG_OPERATION_CANCELLED: operation did not start"
        ));
    }
    *state = ToolConfigOperationState::Running;
    drop(state);
    update_tool_config_operation_progress(&operation, "preparing", None, None);
    Ok(operation)
}

pub(crate) fn finish_tool_config_operation(
    operation: &ToolConfigOperationControl,
    succeeded: bool,
) {
    let Ok(mut state) = operation.state.lock() else {
        return;
    };
    if !state.terminal() {
        *state = if operation.cancelled.load(Ordering::Acquire) {
            ToolConfigOperationState::Cancelled
        } else if succeeded {
            ToolConfigOperationState::Completed
        } else {
            ToolConfigOperationState::Failed
        };
    }
    let terminal_state = *state;
    drop(state);
    let stage = match terminal_state {
        ToolConfigOperationState::Completed => "completed",
        ToolConfigOperationState::Failed => "failed",
        ToolConfigOperationState::Cancelled => "cancelled",
        ToolConfigOperationState::CommittedNotStarted => "committed_not_started",
        _ => "completed",
    };
    update_tool_config_operation_progress(operation, stage, None, None);
    if let Ok(mut retain_until) = operation.retain_until.lock() {
        *retain_until = Instant::now() + TOOL_CONFIG_OPERATION_RETENTION;
    }
    operation.state_changed.notify_all();
}

#[cfg(test)]
pub(crate) fn cancel_tool_config_operation_and_wait(
    operation_id: &str,
) -> Result<ToolConfigCancelAck> {
    let requested = request_cancel_tool_config_operation(operation_id)?;
    let operation = tool_config_operation_by_id(operation_id)?;
    let wait_deadline = Instant::now() + TOOL_CONFIG_CANCEL_SETTLE_GRACE;
    let mut state = operation
        .state
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_STATE_POISONED"))?;
    while !state.terminal() {
        let (next, timeout) = operation
            .state_changed
            .wait_timeout(state, TOOL_PROCESS_POLL_INTERVAL)
            .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_STATE_POISONED"))?;
        state = next;
        if timeout.timed_out() && Instant::now() >= wait_deadline {
            return Err(anyhow!(
                "TOOL_CONFIG_CANCEL_ACK_TIMEOUT: backend operation did not reach a terminal state"
            ));
        }
    }
    drop(state);
    wait_for_tool_config_operation_lease_release(&operation)?;
    let state = operation
        .state
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_STATE_POISONED"))?;
    Ok(ToolConfigCancelAck {
        operation_id: operation.id.clone(),
        state: state.as_str().to_string(),
        cancellation_accepted: requested.cancellation_accepted,
    })
}

fn tool_config_operation_by_id(operation_id: &str) -> Result<Arc<ToolConfigOperationControl>> {
    tool_config_operations()
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_REGISTRY_POISONED"))?
        .get(operation_id)
        .cloned()
        .ok_or_else(|| anyhow!("TOOL_CONFIG_OPERATION_INVALID: unknown operation id"))
}

fn tool_config_operation_status_from_state(
    operation: &ToolConfigOperationControl,
    state: ToolConfigOperationState,
) -> Result<ToolConfigOperationStatus> {
    let progress = operation
        .progress
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_PROGRESS_POISONED"))?
        .clone();
    Ok(ToolConfigOperationStatus {
        operation_id: operation.id.clone(),
        state: state.as_str().to_string(),
        terminal: state.terminal(),
        cancellation_requested: operation.cancelled.load(Ordering::Acquire),
        progress_version: progress.version,
        progress_stage: progress.stage,
        progress_completed: progress.completed,
        progress_total: progress.total,
        progress_ratio: progress.ratio,
    })
}

pub(crate) fn update_tool_config_operation_progress(
    operation: &ToolConfigOperationControl,
    stage: &str,
    completed: Option<u64>,
    total: Option<u64>,
) {
    update_tool_config_operation_progress_with_ratio(operation, stage, completed, total, None);
}

pub(crate) fn update_tool_config_operation_progress_with_ratio(
    operation: &ToolConfigOperationControl,
    stage: &str,
    completed: Option<u64>,
    total: Option<u64>,
    ratio: Option<f64>,
) {
    let Ok(mut progress) = operation.progress.lock() else {
        return;
    };
    let ratio = ratio
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0));
    if progress.stage == stage
        && progress.completed == completed
        && progress.total == total
        && progress.ratio == ratio
    {
        return;
    }
    progress.version = progress.version.saturating_add(1);
    progress.stage = stage.to_string();
    progress.completed = completed;
    progress.total = total;
    progress.ratio = ratio;
    drop(progress);
    operation.state_changed.notify_all();
}

pub(crate) fn request_cancel_tool_config_operation(
    operation_id: &str,
) -> Result<ToolConfigCancelAck> {
    let operation = tool_config_operation_by_id(operation_id)?;
    let mut state = operation
        .state
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_STATE_POISONED"))?;
    let cancellation_accepted = match *state {
        ToolConfigOperationState::Pending => {
            operation.cancelled.store(true, Ordering::Release);
            *state = ToolConfigOperationState::Cancelled;
            true
        }
        ToolConfigOperationState::Running => {
            operation.cancelled.store(true, Ordering::Release);
            true
        }
        ToolConfigOperationState::Committing
        | ToolConfigOperationState::LaunchStarted
        | ToolConfigOperationState::CommittedNotStarted
        | ToolConfigOperationState::Completed
        | ToolConfigOperationState::Failed
        | ToolConfigOperationState::Cancelled => false,
    };
    if cancellation_accepted {
        operation.cancel_requested.notify_waiters();
    }
    operation.state_changed.notify_all();
    Ok(ToolConfigCancelAck {
        operation_id: operation.id.clone(),
        state: state.as_str().to_string(),
        cancellation_accepted,
    })
}

pub(crate) fn wait_for_tool_config_operation_terminal(
    operation_id: &str,
    timeout: Duration,
) -> Result<ToolConfigOperationStatus> {
    let operation = tool_config_operation_by_id(operation_id)?;
    let wait_timeout = std::cmp::min(timeout, TOOL_CONFIG_CANCEL_SETTLE_GRACE);
    let deadline = Instant::now() + wait_timeout;
    let mut state = operation
        .state
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_STATE_POISONED"))?;
    while !state.terminal() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let (next, timeout) = operation
            .state_changed
            .wait_timeout(state, remaining)
            .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_STATE_POISONED"))?;
        state = next;
        if timeout.timed_out() {
            break;
        }
    }
    tool_config_operation_status_from_state(&operation, *state)
}

pub(crate) fn wait_for_tool_config_operation_update(
    operation_id: &str,
    after_version: u64,
    timeout: Duration,
) -> Result<ToolConfigOperationStatus> {
    let operation = tool_config_operation_by_id(operation_id)?;
    let wait_timeout = std::cmp::min(timeout, TOOL_CONFIG_PROGRESS_WAIT_MAX);
    let deadline = Instant::now() + wait_timeout;
    let mut state = operation
        .state
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_STATE_POISONED"))?;
    while !state.terminal() {
        let changed = operation
            .progress
            .lock()
            .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_PROGRESS_POISONED"))?
            .version
            > after_version;
        if changed {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let (next, timeout) = operation
            .state_changed
            .wait_timeout(state, remaining)
            .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_STATE_POISONED"))?;
        state = next;
        if timeout.timed_out() {
            break;
        }
    }
    tool_config_operation_status_from_state(&operation, *state)
}

pub(crate) fn ensure_tool_config_operation_active(
    operation: &ToolConfigOperationControl,
    stage: &str,
) -> Result<()> {
    if operation.cancelled.load(Ordering::Acquire) {
        return Err(anyhow!(
            "TOOL_CONFIG_OPERATION_CANCELLED: cancelled before {stage}"
        ));
    }
    Ok(())
}

pub(crate) fn tool_config_operation_stage_timeout(
    operation: &ToolConfigOperationControl,
    stage: &str,
    timeout: Duration,
) -> Result<Duration> {
    ensure_tool_config_operation_active(operation, stage)?;
    Ok(timeout)
}

pub(crate) async fn wait_for_tool_config_operation_cancellation(
    operation: &ToolConfigOperationControl,
) {
    loop {
        let notified = operation.cancel_requested.notified();
        if operation.cancelled.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

fn begin_tool_config_operation_commit(operation: &ToolConfigOperationControl) -> Result<()> {
    let mut state = operation
        .state
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_STATE_POISONED"))?;
    if *state != ToolConfigOperationState::Running {
        return Err(anyhow!(
            "TOOL_CONFIG_OPERATION_INVALID_STATE: cannot commit from {}",
            state.as_str()
        ));
    }
    if operation.cancelled.load(Ordering::Acquire) {
        *state = ToolConfigOperationState::Cancelled;
        operation.state_changed.notify_all();
        return Err(anyhow!(
            "TOOL_CONFIG_OPERATION_CANCELLED: cancelled before write"
        ));
    }
    *state = ToolConfigOperationState::Committing;
    Ok(())
}

fn tool_config_operation_launch_fence(operation: &ToolConfigOperationControl) -> Result<bool> {
    let mut state = operation
        .state
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_STATE_POISONED"))?;
    if *state != ToolConfigOperationState::Committing {
        return Err(anyhow!(
            "TOOL_CONFIG_OPERATION_INVALID_STATE: cannot launch from {}",
            state.as_str()
        ));
    }
    if operation.cancelled.load(Ordering::Acquire) {
        *state = ToolConfigOperationState::CommittedNotStarted;
        operation.state_changed.notify_all();
        return Ok(false);
    }
    Ok(true)
}

fn tool_config_lifecycle_owners() -> &'static ToolConfigLifecycleOwners {
    static LIFECYCLE: OnceLock<ToolConfigLifecycleOwners> = OnceLock::new();
    LIFECYCLE.get_or_init(ToolConfigLifecycleOwners::default)
}

pub(crate) fn acquire_tool_config_operation_lease(
    operation: &ToolConfigOperationControl,
) -> Result<ToolConfigOperationLease> {
    let lifecycle = tool_config_lifecycle_owners();
    let mut owners = lifecycle
        .owners
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_LIFECYCLE_LOCK_POISONED"))?;
    loop {
        ensure_tool_config_operation_active(operation, "tool lifecycle lock")?;
        if !owners.contains_key(&operation.tool) {
            owners.insert(operation.tool.clone(), operation.id.clone());
            return Ok(ToolConfigOperationLease {
                tool: operation.tool.clone(),
                operation_id: operation.id.clone(),
            });
        }
        let (next, _) = lifecycle
            .changed
            .wait_timeout(owners, TOOL_PROCESS_POLL_INTERVAL)
            .map_err(|_| anyhow!("TOOL_CONFIG_LIFECYCLE_LOCK_POISONED"))?;
        owners = next;
    }
}

#[cfg(test)]
fn wait_for_tool_config_operation_lease_release(
    operation: &ToolConfigOperationControl,
) -> Result<()> {
    let lifecycle = tool_config_lifecycle_owners();
    let wait_deadline = Instant::now() + TOOL_CONFIG_CANCEL_SETTLE_GRACE;
    let mut owners = lifecycle
        .owners
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_LIFECYCLE_LOCK_POISONED"))?;
    while owners.get(&operation.tool) == Some(&operation.id) {
        let (next, timeout) = lifecycle
            .changed
            .wait_timeout(owners, TOOL_PROCESS_POLL_INTERVAL)
            .map_err(|_| anyhow!("TOOL_CONFIG_LIFECYCLE_LOCK_POISONED"))?;
        owners = next;
        if timeout.timed_out() && Instant::now() >= wait_deadline {
            return Err(anyhow!(
                "TOOL_CONFIG_CANCEL_ACK_TIMEOUT: operation retained its tool lock"
            ));
        }
    }
    Ok(())
}

fn lock_tool_config_operation<'a>(
    tool: &str,
    operation_lock: &'a Mutex<()>,
    operation: &ToolConfigOperationControl,
) -> Result<MutexGuard<'a, ()>> {
    loop {
        ensure_tool_config_operation_active(operation, "tool lock")?;
        match operation_lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::WouldBlock) => thread::sleep(TOOL_PROCESS_POLL_INTERVAL),
            Err(TryLockError::Poisoned(_)) => {
                return Err(anyhow!(
                    "TOOL_CONFIG_LOCK_POISONED: {tool} tool configuration lock is poisoned"
                ))
            }
        }
    }
}

pub(crate) fn execute_atomic_tool_config_operation<F, L>(
    operation: &ToolConfigOperationControl,
    protocol: Option<&str>,
    context_digest: &str,
    confirmation_token: Option<&str>,
    write: F,
    launch: L,
) -> Result<ToolConfigWriteOutcome>
where
    F: FnOnce() -> Result<ToolApplyResult>,
    L: FnOnce(&ToolConfigLaunchContext<'_>) -> Result<(String, PathBuf)>,
{
    execute_atomic_tool_config_operation_with_preview(
        operation,
        protocol,
        context_digest,
        confirmation_token,
        || Ok(ToolApplyBuilder::default().finish(operation.tool.as_str())),
        write,
        launch,
    )
}

pub(crate) fn execute_atomic_tool_config_operation_with_preview<P, F, L>(
    operation: &ToolConfigOperationControl,
    protocol: Option<&str>,
    context_digest: &str,
    confirmation_token: Option<&str>,
    preview: P,
    write: F,
    launch: L,
) -> Result<ToolConfigWriteOutcome>
where
    P: FnOnce() -> Result<ToolApplyResult>,
    F: FnOnce() -> Result<ToolApplyResult>,
    L: FnOnce(&ToolConfigLaunchContext<'_>) -> Result<(String, PathBuf)>,
{
    let tool = operation.tool.as_str();
    let operation_lock = tool_config_operation_lock(tool)?;
    let _guard = lock_tool_config_operation(tool, &operation_lock, operation)?;
    update_tool_config_operation_progress(operation, "checking_config", None, None);
    ensure_tool_config_operation_active(operation, "configuration preview")?;
    // Ask before changing configuration or stopping an app, not after a commit.
    if operation.restart && tool_prefers_desktop_program(tool) {
        #[cfg(test)]
        let simulated = test_tool_runtime_snapshot(tool).is_some();
        #[cfg(not(test))]
        let simulated = false;
        if !simulated {
            require_unambiguous_tool_program(
                tool,
                &tool_program_candidates_without_process_scan(tool)?,
            )?;
        }
    }
    let mut preview_result = preview().map_err(|err| {
        anyhow!(
            "TOOL_CONFIG_PREVIEW_FAILED: preview {} configuration: {err:#}",
            tool_runtime_display_name(tool)
        )
    })?;
    if preview_result.already_configured {
        preview_result.details.insert(
            "configuration_unchanged".to_string(),
            "true".to_string(),
        );
        if !operation.restart {
            return Ok(ToolConfigWriteOutcome::Completed(preview_result));
        }
        precheck_tool_config_launch(operation, tool)?;
        begin_tool_config_operation_commit(operation)?;
        return launch_completed_tool_config_operation(operation, tool, preview_result, launch);
    }

    update_tool_config_operation_progress(operation, "checking_process", None, None);
    ensure_tool_config_operation_active(operation, "initial process check")?;

    let capability = match confirmation_token {
        Some(token) => Some(consume_tool_config_confirmation_capability(
            token,
            tool,
            operation.action,
            protocol,
            operation.restart,
            context_digest,
        )?),
        None => None,
    };

    // ForceContinue is issued only after forced shutdown failed and the UI
    // explicitly asked the user. It intentionally skips
    // process probes so a broken detector cannot make the user's Continue
    // choice loop back into the same failure.
    let force_continue = capability
        .as_ref()
        .is_some_and(|value| value.mode == ToolConfigConfirmationMode::ForceContinue);
    let process_snapshot = if force_continue {
        capability
            .as_ref()
            .map(|capability| capability.process_snapshot.clone())
            .unwrap_or_default()
    } else if let Some(capability) = capability.as_ref() {
        // The only full snapshot was captured before the confirmation dialog.
        // PID/path/start identity is revalidated atomically by the kill path.
        capability.process_snapshot.clone()
    } else {
        match tool_process_snapshot(tool) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                eprintln!(
                    "[tool-config] {} process snapshot unavailable; requesting a manual close decision: {err:#}",
                    tool_runtime_display_name(tool)
                );
                let unknown_snapshot = ToolProcessSnapshot::default();
                return manual_close_required_outcome(
                    operation,
                    protocol,
                    context_digest,
                    &unknown_snapshot,
                    "TOOL_CONFIG_PROCESS_PROBE_FAILED".to_string(),
                );
            }
        }
    };

    if !force_continue {
        ensure_tool_config_operation_active(operation, "confirmation check")?;

        if capability.is_none() && process_snapshot.running() {
            return confirmation_required_outcome(
                operation,
                protocol,
                context_digest,
                &process_snapshot,
            );
        }
    }

    let close_capability = capability.as_ref().filter(|value| {
        value.mode == ToolConfigConfirmationMode::CloseAndContinue
            && value.process_snapshot.running()
    });
    let closed_before_write = if let Some(capability) = close_capability {
        update_tool_config_operation_progress(operation, "closing_process", None, None);
        ensure_tool_config_operation_active(operation, "process close")?;
        match close_authorized_tool_processes(tool, &capability.process_snapshot, operation) {
            Ok(true) => true,
            Ok(false) => {
                // A failed close already has an authorized identity snapshot.
                // Re-scanning here only delays the already-authorized manual
                // choice and can repeat the same unreliable system probe.
                let snapshot = capability.process_snapshot.clone();
                return manual_close_required_outcome(
                    operation,
                    protocol,
                    context_digest,
                    &snapshot,
                    "TOOL_CONFIG_PROCESS_STILL_RUNNING".to_string(),
                );
            }
            Err(err) if tool_config_operation_was_interrupted(&err) => return Err(err),
            Err(err) => {
                let snapshot = capability.process_snapshot.clone();
                let detail = format!("{err:#}");
                eprintln!("[tool-config] {tool} close failed; configuration was not written: {detail}");
                let reason = if detail.contains("TOOL_CONFIG_") {
                    detail
                } else {
                    "TOOL_CONFIG_PROCESS_CLOSE_FAILED".to_string()
                };
                return manual_close_required_outcome(
                    operation,
                    protocol,
                    context_digest,
                    &snapshot,
                    reason,
                );
            }
        }
    } else {
        false
    };

    if operation.restart {
        precheck_tool_config_launch(operation, tool)?;
    }
    update_tool_config_operation_progress(operation, "writing_config", None, None);
    begin_tool_config_operation_commit(operation)?;
    let mut result = write().map_err(|err| {
        anyhow!(
            "TOOL_CONFIG_WRITE_FAILED: write {} configuration: {err:#}",
            tool_runtime_display_name(tool)
        )
    })?;
    if !result.files.is_empty() || result.file_statuses.iter().any(|status| status.changed) {
        result.details.insert(
            "configuration_changed".to_string(),
            "true".to_string(),
        );
    }
    attach_closed_before_config(&mut result, closed_before_write);
    if force_continue {
        result.details.insert(
            "continued_after_close_failure".to_string(),
            "true".to_string(),
        );
    }
    launch_completed_tool_config_operation(operation, tool, result, launch)
}

fn precheck_tool_config_launch(operation: &ToolConfigOperationControl, tool: &str) -> Result<()> {
    ensure_tool_config_operation_active(operation, "start precheck")?;
    ensure_tool_launch_not_claimed_by_external_launcher(tool, codex_plus_plus_running()?).map_err(
        |err| {
            anyhow!(
                "TOOL_CONFIG_START_PRECHECK_FAILED: check {} launch conditions: {err:#}",
                tool_runtime_display_name(tool)
            )
        },
    )
}

fn launch_completed_tool_config_operation<L>(
    operation: &ToolConfigOperationControl,
    tool: &str,
    mut result: ToolApplyResult,
    launch: L,
) -> Result<ToolConfigWriteOutcome>
where
    L: FnOnce(&ToolConfigLaunchContext<'_>) -> Result<(String, PathBuf)>,
{
    if !operation.restart {
        return Ok(ToolConfigWriteOutcome::Completed(result));
    }
    update_tool_config_operation_progress(operation, "launching", None, None);
    if !tool_config_operation_launch_fence(operation)? {
        result.details.insert(
            "operation_status".to_string(),
            "committed_not_started".to_string(),
        );
        return Ok(ToolConfigWriteOutcome::Completed(result));
    }
    let launch_context = ToolConfigLaunchContext { operation };
    let (command, launcher_path) = match launch(&launch_context) {
        Ok(launched) => launched,
        Err(err) if err.downcast_ref::<ToolConfigCommittedNotStarted>().is_some() => {
            result.details.insert(
                "operation_status".to_string(),
                "committed_not_started".to_string(),
            );
            return Ok(ToolConfigWriteOutcome::Completed(result));
        }
        Err(err) => {
            return Err(anyhow!(
                "TOOL_CONFIG_START_FAILED: start and verify {}: {err:#}",
                tool_runtime_display_name(tool)
            ))
        }
    };
    result
        .details
        .insert("restart_command".to_string(), command);
    result.details.insert(
        "restart_launcher_path".to_string(),
        launcher_path.display().to_string(),
    );
    Ok(ToolConfigWriteOutcome::Completed(result))
}

fn confirmation_required_outcome(
    operation: &ToolConfigOperationControl,
    protocol: Option<&str>,
    context_digest: &str,
    process_snapshot: &ToolProcessSnapshot,
) -> Result<ToolConfigWriteOutcome> {
    update_tool_config_operation_progress(operation, "awaiting_confirmation", None, None);
    let confirmation_token = issue_tool_config_confirmation_capability(
        &operation.tool,
        operation.action,
        protocol,
        operation.restart,
        context_digest,
        process_snapshot,
    )?;
    Ok(ToolConfigWriteOutcome::ConfirmationRequired {
        running: process_snapshot.running(),
        confirmation_token,
    })
}

fn manual_close_required_outcome(
    operation: &ToolConfigOperationControl,
    protocol: Option<&str>,
    context_digest: &str,
    process_snapshot: &ToolProcessSnapshot,
    reason: String,
) -> Result<ToolConfigWriteOutcome> {
    update_tool_config_operation_progress(operation, "awaiting_manual_close", None, None);
    let confirmation_token = issue_tool_config_force_continue_capability(
        &operation.tool,
        operation.action,
        protocol,
        operation.restart,
        context_digest,
        process_snapshot,
    )?;
    Ok(ToolConfigWriteOutcome::ManualCloseRequired {
        running: process_snapshot.running(),
        confirmation_token,
        reason,
    })
}

#[cfg(any(test, not(target_os = "windows")))]
fn tool_process_close_deadline(_tool: &str, started: Instant) -> Instant {
    #[cfg(test)]
    if test_tool_runtime_snapshot(_tool)
        .map(|state| state.close_timeout_immediately)
        .unwrap_or(false)
    {
        return started;
    }
    started + TOOL_PROCESS_CLOSE_TIMEOUT
}

pub(crate) fn launch_tool_program_for_atomic_operation(
    tool: &str,
    launch_context: &ToolConfigLaunchContext<'_>,
) -> Result<(String, PathBuf)> {
    launch_tool_program_inner(tool, Some(launch_context))
}

fn wait_for_tool_program_state_until(
    tool: &str,
    expected_running: bool,
    deadline: Instant,
    action: &str,
) -> Result<()> {
    wait_for_tool_program_state_until_inner(tool, expected_running, deadline, action, None)
}

fn wait_for_tool_program_state_until_inner(
    tool: &str,
    expected_running: bool,
    deadline: Instant,
    action: &str,
    operation: Option<&ToolConfigOperationControl>,
) -> Result<()> {
    loop {
        if let Some(operation) = operation {
            ensure_tool_config_operation_active(operation, "process state handshake")?;
        }
        let running = tool_program_running(tool).with_context(|| {
            format!(
                "After {}, check {} process state",
                action,
                tool_runtime_display_name(tool)
            )
        })?;
        if running == expected_running {
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            let display_name = tool_runtime_display_name(tool);
            return if expected_running {
                Err(anyhow!(
                    "{display_name} was not detected within {} seconds of launch. Configuration was saved; check its path or start it manually.",
                    TOOL_PROCESS_START_TIMEOUT.as_secs()
                ))
            } else {
                Err(anyhow!(
                    "{display_name} did not exit within {} seconds; operation stopped without writing configuration.",
                    TOOL_PROCESS_CLOSE_TIMEOUT.as_secs()
                ))
            };
        }
        thread::sleep(std::cmp::min(
            TOOL_PROCESS_POLL_INTERVAL,
            deadline.saturating_duration_since(now),
        ));
    }
}

fn wait_for_tool_program_start(tool: &str) -> Result<()> {
    wait_for_tool_program_state_until(
        tool,
        true,
        Instant::now() + TOOL_PROCESS_START_TIMEOUT,
        "launch",
    )
}

#[derive(Debug)]
struct CommandLaunchHandshake {
    id: String,
    started: PathBuf,
    exited: PathBuf,
}

fn create_command_launch_handshake(tool: &str) -> Result<CommandLaunchHandshake> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let directory = crate::client_data_root()
        .join("runtime")
        .join("tools")
        .join("handshakes");
    fs::create_dir_all(&directory)?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = format!(
        "{}-{}-{}-{}",
        tool,
        std::process::id(),
        timestamp,
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    Ok(CommandLaunchHandshake {
        id: id.clone(),
        started: directory.join(format!("{id}.started")),
        exited: directory.join(format!("{id}.exited")),
    })
}

fn wait_for_command_launch_handshake(
    started: &Path,
    exited: &Path,
    deadline: Instant,
    stability_window: Duration,
) -> Result<()> {
    loop {
        if exited.exists() {
            let detail = fs::read_to_string(exited).unwrap_or_else(|_| "exited".to_string());
            return Err(anyhow!(
                "TOOL_START_UNSTABLE: command exited before startup confirmation ({})",
                detail.trim()
            ));
        }
        if started.exists() {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(anyhow!(
                "TOOL_START_HANDSHAKE_TIMEOUT: terminal did not execute the tool command within 10 seconds"
            ));
        }
        thread::sleep(std::cmp::min(
            TOOL_PROCESS_POLL_INTERVAL,
            deadline.saturating_duration_since(now),
        ));
    }

    let stable_until = std::cmp::min(deadline, Instant::now() + stability_window);
    loop {
        if exited.exists() {
            let detail = fs::read_to_string(exited).unwrap_or_else(|_| "exited".to_string());
            return Err(anyhow!(
                "TOOL_START_UNSTABLE: command exited during startup stability check ({})",
                detail.trim()
            ));
        }
        let now = Instant::now();
        if now >= stable_until {
            return Ok(());
        }
        thread::sleep(std::cmp::min(
            TOOL_PROCESS_POLL_INTERVAL,
            stable_until.saturating_duration_since(now),
        ));
    }
}

fn confirm_command_launch(handshake: &CommandLaunchHandshake) -> Result<()> {
    wait_for_command_launch_handshake(
        &handshake.started,
        &handshake.exited,
        Instant::now() + TOOL_PROCESS_START_TIMEOUT,
        COMMAND_START_STABILITY_WINDOW,
    )
}

fn attach_closed_before_config(config: &mut ToolApplyResult, closed: bool) {
    if closed {
        config
            .details
            .insert("closed_before_config".to_string(), "true".to_string());
    }
}

fn ensure_tool_launch_not_claimed_by_external_launcher(
    tool: &str,
    codex_plus_plus_is_running: bool,
) -> Result<()> {
    if tool == "codex" && codex_plus_plus_is_running {
        return Err(anyhow!(
            "Cannot safely configure and restart Codex while Codex++ is running: it overwrites ~/.codex/config.toml. Exit the Codex++ manager and Codex++, then launch official ChatGPT or legacy Codex Desktop from CONST API."
        ));
    }
    Ok(())
}

pub(crate) fn launch_claude_code_program_for_atomic_operation(
    root_url: &str,
    api_key: &str,
    launch_context: &ToolConfigLaunchContext<'_>,
) -> Result<(String, PathBuf)> {
    launch_claude_code_program_inner(root_url, api_key, Some(launch_context))
}

pub(crate) fn launch_claude_code_program(
    root_url: &str,
    api_key: &str,
) -> Result<(String, PathBuf)> {
    launch_claude_code_program_inner(root_url, api_key, None)
}

fn launch_claude_code_program_inner(
    root_url: &str,
    api_key: &str,
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<(String, PathBuf)> {
    #[cfg(test)]
    if test_tool_runtime_snapshot("claude").is_some() {
        let launched = match launch_context {
            Some(context) => context.spawn("test Claude Code process", || {
                test_tool_runtime_launch("claude").expect("test runtime checked above")
            })?,
            None => test_tool_runtime_launch("claude").expect("test runtime checked above")?,
        };
        wait_for_tool_program_start("claude")?;
        return Ok(launched);
    }

    let root_url = tool_surface_url(root_url, "anthropic");
    let Some(path) = locate_tool_program("claude")?
        .candidates
        .into_iter()
        .find(|candidate| candidate.kind == "command" && candidate.exists)
        .map(|candidate| PathBuf::from(candidate.path))
    else {
        return Err(anyhow!(
            "TOOL_START_CANDIDATE_MISSING: no launchable claude command found"
        ));
    };

    let path = command_launch_path(&path);
    let envs = claude_code_child_environment(&root_url, api_key);
    launch_command_tool_with_env(
        "claude",
        &path,
        &envs,
        CLAUDE_CODE_CONFLICTING_ENV_NAMES,
        launch_context,
    )?;
    Ok((
        "Started with Claude Code environment variables".to_string(),
        path,
    ))
}

fn claude_code_child_environment<'a>(
    root_url: &'a str,
    api_key: &'a str,
) -> [(&'static str, &'a str); 3] {
    [
        ("ANTHROPIC_BASE_URL", root_url),
        ("ANTHROPIC_AUTH_TOKEN", api_key),
        ("CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST", "1"),
    ]
}

pub(crate) fn launch_claude_science_program_for_atomic_operation(
    root_url: &str,
    api_key: &str,
    launch_context: &ToolConfigLaunchContext<'_>,
) -> Result<(String, PathBuf)> {
    launch_claude_science_program_inner(root_url, api_key, Some(launch_context))
}

pub(crate) fn launch_claude_science_program(
    root_url: &str,
    api_key: &str,
) -> Result<(String, PathBuf)> {
    launch_claude_science_program_inner(root_url, api_key, None)
}

fn launch_claude_science_program_inner(
    root_url: &str,
    api_key: &str,
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<(String, PathBuf)> {
    #[cfg(test)]
    if test_tool_runtime_snapshot("claude-science").is_some() {
        let launched = match launch_context {
            Some(context) => context.spawn("test Claude Science process", || {
                test_tool_runtime_launch("claude-science").expect("test runtime checked above")
            })?,
            None => test_tool_runtime_launch("claude-science")
                .expect("test runtime checked above")?,
        };
        wait_for_tool_program_start("claude-science")?;
        return Ok(launched);
    }

    let Some(path) = locate_tool_program("claude-science")?
        .candidates
        .into_iter()
        .find(|candidate| candidate.exists && candidate.launchable)
        .map(|candidate| PathBuf::from(candidate.path))
    else {
        return Err(anyhow!(
            "TOOL_START_CANDIDATE_MISSING: no launchable Claude Science program found"
        ));
    };

    let path = command_launch_path(&path);
    let envs = [
        ("ANTHROPIC_BASE_URL", root_url),
        ("ANTHROPIC_AUTH_TOKEN", api_key),
    ];
    launch_command_tool_with_env_and_args(
        "claude-science",
        &path,
        &["serve"],
        &envs,
        CLAUDE_SCIENCE_CONFLICTING_ENV_NAMES,
        launch_context,
    )?;
    Ok((
        "Started with Claude Science environment variables".to_string(),
        path,
    ))
}

#[cfg(target_os = "windows")]
fn ps_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) fn launch_gemini_cli_program_for_atomic_operation(
    root_url: &str,
    api_key: &str,
    launch_context: &ToolConfigLaunchContext<'_>,
) -> Result<(String, PathBuf)> {
    launch_gemini_cli_program_inner(root_url, api_key, Some(launch_context))
}

pub(crate) fn launch_gemini_cli_program(
    root_url: &str,
    api_key: &str,
) -> Result<(String, PathBuf)> {
    launch_gemini_cli_program_inner(root_url, api_key, None)
}

fn launch_gemini_cli_program_inner(
    root_url: &str,
    api_key: &str,
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<(String, PathBuf)> {
    #[cfg(test)]
    if test_tool_runtime_snapshot("gemini").is_some() {
        let launched = match launch_context {
            Some(context) => context.spawn("test Gemini CLI process", || {
                test_tool_runtime_launch("gemini").expect("test runtime checked above")
            })?,
            None => test_tool_runtime_launch("gemini").expect("test runtime checked above")?,
        };
        wait_for_tool_program_start("gemini")?;
        return Ok(launched);
    }

    let root_url = tool_surface_url(root_url, "gemini");
    let Some(path) = locate_tool_program("gemini")?
        .candidates
        .into_iter()
        .find(|candidate| candidate.kind == "command" && candidate.exists)
        .map(|candidate| PathBuf::from(candidate.path))
    else {
        return Err(anyhow!(
            "TOOL_START_CANDIDATE_MISSING: no launchable gemini command found"
        ));
    };

    let path = command_launch_path(&path);
    let envs = gemini_cli_child_environment(&root_url, api_key);
    launch_command_tool_with_env(
        "gemini",
        &path,
        &envs,
        GEMINI_CLI_CONFLICTING_ENV_NAMES,
        launch_context,
    )?;
    Ok((
        "Started with Gemini environment variables".to_string(),
        path,
    ))
}

fn gemini_cli_child_environment<'a>(
    root_url: &'a str,
    api_key: &'a str,
) -> [(&'static str, &'a str); 2] {
    [
        ("GOOGLE_GEMINI_BASE_URL", root_url),
        ("GEMINI_API_KEY", api_key),
    ]
}

fn launch_tool_program_with_environment_inner(
    tool: &str,
    envs: &[(&str, &str)],
    removed_envs: &[&str],
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<(String, PathBuf)> {
    #[cfg(test)]
    if test_tool_runtime_snapshot(tool).is_some() {
        let launched = match launch_context {
            Some(context) => context.spawn("test configured tool process", || {
                test_tool_runtime_launch(tool).expect("test runtime checked above")
            })?,
            None => test_tool_runtime_launch(tool).expect("test runtime checked above")?,
        };
        wait_for_tool_program_start(tool)?;
        return Ok(launched);
    }

    let location = locate_tool_program(tool)?;
    require_unambiguous_tool_program(tool, &location.candidates)?;
    let Some(candidate) = select_tool_launch_candidate(tool, &location.candidates) else {
        return Err(anyhow!(
            "TOOL_START_CANDIDATE_MISSING: no launchable {} program was found",
            tool_runtime_display_name(tool)
        ));
    };
    let path = PathBuf::from(&candidate.path);
    match candidate.kind.as_str() {
        "command" => launch_command_tool_with_env(tool, &path, envs, removed_envs, launch_context)?,
        "windows_exe" | "linux_desktop" => {
            let mut command = Command::new(&path);
            command.args(tool_program_launch_args(tool));
            for name in removed_envs {
                command.env_remove(name);
            }
            for (name, value) in envs {
                command.env(name, value);
            }
            spawn_launch_command(&mut command, launch_context, &path.display().to_string())?;
            wait_for_tool_program_start(tool)?;
        }
        #[cfg(target_os = "macos")]
        "mac_app" => {
            // `open -a` can reuse an existing LaunchServices environment. Start
            // the bundle executable directly so launch-only credentials reach
            // this process, without changing the user's global environment.
            let executable = macos_app_main_executable_path(&path).ok_or_else(|| {
                anyhow!(
                    "TOOL_START_CANDIDATE_MISSING: bundle executable for {}",
                    path.display()
                )
            })?;
            let mut command = Command::new(&executable);
            command.args(tool_program_launch_args(tool));
            for name in removed_envs {
                command.env_remove(name);
            }
            for (name, value) in envs {
                command.env(name, value);
            }
            spawn_launch_command(&mut command, launch_context, &path.display().to_string())?;
            wait_for_tool_program_start(tool)?;
        }
        _ => {
            return Err(anyhow!(
                "TOOL_START_ENVIRONMENT_UNSUPPORTED: {} cannot safely receive its configured environment through {}",
                tool_runtime_display_name(tool),
                candidate.kind
            ));
        }
    }
    Ok((
        "launched with the configured CONST API environment".to_string(),
        path,
    ))
}

pub(crate) fn launch_copilot_cli_program_for_atomic_operation(
    base_url: &str,
    api_key: &str,
    launch_context: &ToolConfigLaunchContext<'_>,
) -> Result<(String, PathBuf)> {
    launch_copilot_cli_program_inner(base_url, api_key, Some(launch_context))
}

pub(crate) fn launch_copilot_cli_program(
    base_url: &str,
    api_key: &str,
) -> Result<(String, PathBuf)> {
    launch_copilot_cli_program_inner(base_url, api_key, None)
}

fn launch_copilot_cli_program_inner(
    base_url: &str,
    api_key: &str,
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<(String, PathBuf)> {
    let path = copilot_env_path();
    let raw = read_text_or_empty(&path)?;
    let model = env_value(&raw, "COPILOT_MODEL")
        .ok_or_else(|| anyhow!("TOOL_CONFIG_MODEL_MISSING: Copilot CLI"))?;
    let base_url = tool_surface_url(base_url, "v1");
    let providers_path = copilot_providers_path().display().to_string();
    let envs = copilot_cli_child_environment(&base_url, api_key, &model, &providers_path);
    launch_tool_program_with_environment_inner(
        "copilot",
        &envs,
        COPILOT_CONFLICTING_ENVIRONMENT,
        launch_context,
    )
}

const COPILOT_CONFLICTING_ENVIRONMENT: &[&str] = &[
    "COPILOT_PROVIDER_API_KEY_COMMAND",
    "COPILOT_PROVIDER_BEARER_TOKEN",
    "COPILOT_PROVIDER_HEADERS",
    "COPILOT_PROVIDER_WIRE_MODEL",
    "COPILOT_PROVIDER_MODEL_ID",
];

fn copilot_cli_child_environment<'a>(
    base_url: &'a str,
    api_key: &'a str,
    model: &'a str,
    providers_path: &'a str,
) -> [(&'static str, &'a str); 7] {
    [
        ("COPILOT_PROVIDER_TYPE", "openai"),
        ("COPILOT_PROVIDER_BASE_URL", base_url),
        ("COPILOT_PROVIDER_API_KEY", api_key),
        ("COPILOT_MODEL", model),
        ("COPILOT_PROVIDERS_CONFIG", providers_path),
        ("COPILOT_PROVIDER_WIRE_API", "completions"),
        ("COPILOT_PROVIDER_TRANSPORT", "http"),
    ]
}

pub(crate) fn launch_goose_program_for_atomic_operation(
    api_key: &str,
    launch_context: &ToolConfigLaunchContext<'_>,
) -> Result<(String, PathBuf)> {
    launch_goose_program_inner(api_key, Some(launch_context))
}

pub(crate) fn launch_goose_program(
    api_key: &str,
) -> Result<(String, PathBuf)> {
    launch_goose_program_inner(api_key, None)
}

fn launch_goose_program_inner(
    api_key: &str,
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<(String, PathBuf)> {
    let (_, env_path) = goose_config_paths();
    let raw = read_text_or_empty(&env_path)?;
    let model = env_value(&raw, "GOOSE_MODEL")
        .ok_or_else(|| anyhow!("TOOL_CONFIG_MODEL_MISSING: Goose"))?;
    let envs = [
        ("GOOSE_PROVIDER", "const_api"),
        ("GOOSE_MODEL", model.as_str()),
        (ADDITIONAL_CONST_API_ENV_KEY, api_key),
    ];
    launch_tool_program_with_environment_inner("goose", &envs, &[], launch_context)
}

#[cfg(any(unix, test))]
fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
