// Native, read-only discovery also covers desktop apps installed outside the
// default Program Files folders. No PowerShell startup or drive-wide scan.
fn windows_installed_program_paths(tool: &str) -> Vec<PathBuf> {
    use windows_sys::Win32::{Foundation::ERROR_SUCCESS, System::Registry::*};
    let Some(profile) = tool_profile(tool).filter(|p| !p.windows_program_paths.is_empty()) else {
        return Vec::new();
    };
    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }
    fn open(parent: HKEY, name: &[u16], view: u32) -> Option<Key> {
        let mut key = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(parent, name.as_ptr(), 0, KEY_READ | view, &mut key) }
            == ERROR_SUCCESS
        {
            Some(Key(key))
        } else {
            None
        }
    }
    fn string(key: &Key, name: &str) -> Option<String> {
        let name = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let mut size = 0;
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
        if unsafe {
            RegGetValueW(
                key.0,
                std::ptr::null(),
                name.as_ptr(),
                flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        } != ERROR_SUCCESS
        {
            return None;
        }
        // Registry data is an external boundary; paths/names cannot exceed the
        // Windows maximum string length or allocate an arbitrary value size.
        if size == 0 || size > 65_536 {
            return None;
        }
        let mut value = vec![0u16; (size as usize).div_ceil(2)];
        if unsafe {
            RegGetValueW(
                key.0,
                std::ptr::null(),
                name.as_ptr(),
                flags,
                std::ptr::null_mut(),
                value.as_mut_ptr().cast(),
                &mut size,
            )
        } != ERROR_SUCCESS
        {
            return None;
        }
        let end = value.iter().position(|c| *c == 0).unwrap_or(value.len());
        String::from_utf16(&value[..end]).ok()
    }
    let uninstall = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall"
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut paths = Vec::new();
    for (hive, view) in [
        (HKEY_CURRENT_USER, 0),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY),
    ] {
        let Some(root) = open(hive, &uninstall, view) else {
            continue;
        };
        for index in 0..u32::MAX {
            let mut name = [0u16; 256]; // registry subkeys are at most 255 characters
            let mut length = name.len() as u32;
            if unsafe {
                RegEnumKeyExW(
                    root.0,
                    index,
                    name.as_mut_ptr(),
                    &mut length,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            } != ERROR_SUCCESS
            {
                break;
            }
            let Some(entry) = open(root.0, &name, view) else {
                continue;
            };
            let Some(display) = string(&entry, "DisplayName") else {
                continue;
            };
            if !profile
                .windows_program_paths
                .iter()
                .any(|(product, _)| windows_installed_product_matches(&display, product))
            {
                continue;
            }
            for path in windows_installed_entry_paths(
                profile,
                string(&entry, "InstallLocation").as_deref(),
                string(&entry, "DisplayIcon").as_deref(),
            ) {
                if path.is_file() {
                    push_unique_path(&mut paths, path);
                }
            }
        }
    }
    paths
}

fn windows_installed_product_matches(display: &str, product: &str) -> bool {
    let display = display.trim().to_ascii_lowercase();
    let product = product.to_ascii_lowercase();
    display == product
        || display.strip_prefix(&product).is_some_and(|suffix| {
            suffix.starts_with(' ')
                && suffix
                    .trim_start()
                    .starts_with(|c: char| c.is_ascii_digit())
        })
}

fn windows_installed_entry_paths(
    profile: &ToolProfile,
    install_location: Option<&str>,
    display_icon: Option<&str>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(path) = install_location.map(str::trim).filter(|s| !s.is_empty()) {
        dirs.push(PathBuf::from(path.trim_matches('"')));
    }
    if let Some(icon) = display_icon {
        // DisplayIcon is a path with an optional numeric resource index, never
        // a shell command. The icon may be an .ico rather than the actual EXE.
        let icon = icon
            .rsplit_once(',')
            .filter(|(_, index)| index.trim().parse::<i32>().is_ok())
            .map_or(icon, |(path, _)| path)
            .trim()
            .trim_matches('"');
        if let Some(dir) = Path::new(icon).parent() {
            dirs.push(dir.to_path_buf());
        }
    }
    dirs.into_iter()
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| {
            profile
                .windows_program_paths
                .iter()
                .map(move |(_, exe)| dir.join(exe))
        })
        .collect()
}
