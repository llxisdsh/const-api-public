use anyhow::{Context, Result};
use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

const DEFAULT_ENABLE_PENDING_FILE_NAME: &str = "autostart-default-enable.pending";
const INITIALIZED_FILE_NAME: &str = "autostart-initialized-v1";
pub(crate) const TRAY_ARGUMENT: &str = "--tray";

fn pending_path(state_dir: &Path) -> PathBuf {
    state_dir.join(DEFAULT_ENABLE_PENDING_FILE_NAME)
}

fn initialized_path(state_dir: &Path) -> PathBuf {
    state_dir.join(INITIALIZED_FILE_NAME)
}

fn marker_exists(path: &Path) -> Result<bool> {
    path.try_exists()
        .with_context(|| format!("inspect autostart marker {}", path.display()))
}

fn create_marker_if_absent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("autostart marker {} has no parent", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create autostart state directory {}", parent.display()))?;
    let mut file = match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("create autostart marker {}", path.display()));
        }
    };
    file.write_all(b"1\n")
        .with_context(|| format!("write autostart marker {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync autostart marker {}", path.display()))?;
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("remove autostart marker {}", path.display()))
        }
    }
}

pub(crate) fn stage_default_enablement(_: &Path) -> Result<()> { Ok(()) }

pub(crate) fn default_enablement_pending(state_dir: &Path) -> Result<bool> {
    if marker_exists(&initialized_path(state_dir))? {
        return Ok(false);
    }
    marker_exists(&pending_path(state_dir))
}

/// Makes either the successfully-applied default or an explicit user choice
/// authoritative before the pending marker is removed.
pub(crate) fn mark_initialized(state_dir: &Path) -> Result<()> {
    create_marker_if_absent(&initialized_path(state_dir))?;
    remove_if_present(&pending_path(state_dir))
}

pub(crate) fn enable(app: &tauri::AppHandle) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let (path, executable) = macos_launch_agent_paths(app)?;
        enable_launch_agent_file(
            &path,
            &app.package_info().name,
            &app.config().identifier,
            &executable,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        use tauri_plugin_autostart::ManagerExt;
        app.autolaunch().enable().map_err(Into::into)
    }
}

#[cfg(target_os = "macos")]
fn macos_launch_agent_paths(app: &tauri::AppHandle) -> Result<(PathBuf, PathBuf)> {
    let app_name = &app.package_info().name;
    let home = dirs::home_dir().context("find home directory for login startup")?;
    // Match the plugin's path so its status and disable operations keep working.
    let path = home
        .join("Library/LaunchAgents")
        .join(format!("{app_name}.plist"));
    let executable = std::env::current_exe()
        .context("find executable for login startup")?
        .canonicalize()
        .context("resolve executable for login startup")?;
    Ok((path, executable))
}

#[cfg(target_os = "macos")]
fn enable_launch_agent_file(
    path: &Path,
    app_name: &str,
    bundle_id: &str,
    executable: &Path,
) -> Result<()> {
    use plist::{Dictionary, Value};

    let executable = executable
        .to_str()
        .context("login startup executable path is not valid UTF-8")?;
    let mut fields = Dictionary::new();
    fields.insert("Label".to_owned(), Value::String(app_name.to_owned()));
    fields.insert(
        "ProgramArguments".to_owned(),
        Value::Array(vec![
            Value::String(executable.to_owned()),
            Value::String(TRAY_ARGUMENT.to_owned()),
        ]),
    );
    fields.insert("RunAtLoad".to_owned(), Value::Boolean(true));
    fields.insert(
        "AssociatedBundleIdentifiers".to_owned(),
        Value::String(bundle_id.to_owned()),
    );

    // Publish the complete plist at once. Patching the plugin's file afterwards
    // lets Background Items cache the signing name before the association exists.
    let permissions = launch_agent_metadata(path)?.map(|metadata| metadata.permissions());
    write_launch_agent_file(path, &Value::Dictionary(fields), permissions)
}

#[cfg(target_os = "macos")]
fn launch_agent_metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect login startup agent {}", path.display()));
        }
    };
    anyhow::ensure!(
        metadata.file_type().is_file(),
        "login startup agent is not a regular file: {}",
        path.display()
    );
    Ok(Some(metadata))
}

#[cfg(target_os = "macos")]
pub(crate) fn associate_macos_launch_agent(app: &tauri::AppHandle) -> Result<bool> {
    let (path, executable) = macos_launch_agent_paths(app)?;
    associate_launch_agent_file(
        &path,
        &app.package_info().name,
        &app.config().identifier,
        &executable,
    )
}

#[cfg(target_os = "macos")]
fn associate_launch_agent_file(
    path: &Path,
    app_name: &str,
    bundle_id: &str,
    executable: &Path,
) -> Result<bool> {
    use plist::Value;

    let Some(metadata) = launch_agent_metadata(path)? else {
        return Ok(false);
    };

    let mut agent = Value::from_file(path)
        .with_context(|| format!("read login startup agent {}", path.display()))?;
    let fields = agent
        .as_dictionary_mut()
        .context("login startup agent is not a dictionary")?;
    anyhow::ensure!(
        fields.get("Label").and_then(Value::as_string) == Some(app_name),
        "login startup agent has an unexpected label: {}",
        path.display()
    );
    let registered_executable = fields
        .get("ProgramArguments")
        .and_then(Value::as_array)
        .and_then(|arguments| arguments.first())
        .and_then(Value::as_string)
        .context("login startup agent has no executable")?;
    anyhow::ensure!(
        Path::new(registered_executable) == executable,
        "login startup agent points to a different executable: {}",
        path.display()
    );

    let association = Value::String(bundle_id.to_owned());
    if fields.get("AssociatedBundleIdentifiers") == Some(&association) {
        return Ok(false);
    }
    fields.insert("AssociatedBundleIdentifiers".to_owned(), association);

    write_launch_agent_file(path, &agent, Some(metadata.permissions()))?;
    Ok(true)
}

#[cfg(target_os = "macos")]
fn write_launch_agent_file(
    path: &Path,
    agent: &plist::Value,
    permissions: Option<fs::Permissions>,
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let parent = path.parent().context("login startup agent has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create login startup directory {}", parent.display()))?;
    let mut replacement = tempfile::NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "create replacement login startup agent in {}",
            parent.display()
        )
    })?;
    replacement
        .as_file()
        .set_permissions(permissions.unwrap_or_else(|| fs::Permissions::from_mode(0o644)))
        .context("set login startup agent permissions")?;
    agent
        .to_writer_xml(replacement.as_file_mut())
        .context("write associated login startup agent")?;
    replacement
        .as_file_mut()
        .sync_all()
        .context("sync associated login startup agent")?;
    replacement
        .persist(path)
        .with_context(|| format!("replace login startup agent {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    use plist::Value;

    #[test]
    fn new_install_stages_default_enablement() {
        let temp = tempfile::tempdir().unwrap();

        assert!(!default_enablement_pending(temp.path()).unwrap());
        stage_default_enablement(temp.path()).unwrap();
        stage_default_enablement(temp.path()).unwrap();

        assert!(default_enablement_pending(temp.path()).unwrap());
    }

    #[test]
    fn completed_initialization_clears_pending_default() {
        let temp = tempfile::tempdir().unwrap();
        stage_default_enablement(temp.path()).unwrap();

        mark_initialized(temp.path()).unwrap();

        assert!(!default_enablement_pending(temp.path()).unwrap());
        assert!(!pending_path(temp.path()).exists());
        assert!(initialized_path(temp.path()).exists());
    }

    #[test]
    fn initialized_install_ignores_a_stale_pending_marker() {
        let temp = tempfile::tempdir().unwrap();
        mark_initialized(temp.path()).unwrap();
        create_marker_if_absent(&pending_path(temp.path())).unwrap();

        assert!(!default_enablement_pending(temp.path()).unwrap());
        stage_default_enablement(temp.path()).unwrap();
        assert!(!default_enablement_pending(temp.path()).unwrap());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn enables_launch_agent_with_complete_bundle_association() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("LaunchAgents/CONST API.plist");
        let executable = Path::new("/Applications/CONST & API.app/Contents/MacOS/const-api-client");

        enable_launch_agent_file(&path, "CONST API", "xin.const.api.client", executable).unwrap();

        let agent = Value::from_file(&path).unwrap();
        let fields = agent.as_dictionary().unwrap();
        assert_eq!(fields["Label"].as_string(), Some("CONST API"));
        assert_eq!(
            fields["AssociatedBundleIdentifiers"].as_string(),
            Some("xin.const.api.client")
        );
        assert_eq!(
            fields["ProgramArguments"].as_array().unwrap(),
            &vec![
                Value::String(executable.to_str().unwrap().to_owned()),
                Value::String("--tray".to_owned()),
            ]
        );
        assert_eq!(fields["RunAtLoad"].as_boolean(), Some(true));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reenabling_launch_agent_updates_executable_and_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("CONST API.plist");
        let old_executable = Path::new("/Old/CONST API.app/Contents/MacOS/const-api-client");
        let executable = Path::new("/Applications/CONST API.app/Contents/MacOS/const-api-client");
        enable_launch_agent_file(&path, "CONST API", "xin.const.api.client", old_executable)
            .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        enable_launch_agent_file(&path, "CONST API", "xin.const.api.client", executable).unwrap();

        let agent = Value::from_file(&path).unwrap();
        let fields = agent.as_dictionary().unwrap();
        assert_eq!(
            fields["ProgramArguments"].as_array().unwrap()[0].as_string(),
            executable.to_str()
        );
        assert_eq!(
            fields["AssociatedBundleIdentifiers"].as_string(),
            Some("xin.const.api.client")
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn association_does_not_enable_a_disabled_launch_agent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("LaunchAgents/CONST API.plist");
        let executable = Path::new("/Applications/CONST API.app/Contents/MacOS/const-api-client");

        assert!(
            !associate_launch_agent_file(&path, "CONST API", "xin.const.api.client", executable)
                .unwrap()
        );
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn enabling_launch_agent_rejects_symlinks_without_modifying_the_target() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("user-owned.plist");
        let path = temp.path().join("CONST API.plist");
        let executable = Path::new("/Applications/CONST API.app/Contents/MacOS/const-api-client");
        fs::write(&target, b"user-owned content").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        assert!(
            enable_launch_agent_file(&path, "CONST API", "xin.const.api.client", executable)
                .is_err()
        );
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&target).unwrap(), b"user-owned content");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn associates_existing_launch_agent_with_app_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("CONST API.plist");
        let executable = Path::new("/Applications/CONST API.app/Contents/MacOS/const-api-client");
        fs::write(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>Label</key><string>CONST API</string>
<key>ProgramArguments</key><array>
<string>/Applications/CONST API.app/Contents/MacOS/const-api-client</string>
<string>--tray</string>
</array>
<key>RunAtLoad</key><true/>
</dict></plist>"#,
        )
        .unwrap();

        assert!(
            associate_launch_agent_file(&path, "CONST API", "xin.const.api.client", executable)
                .unwrap()
        );
        assert!(
            !associate_launch_agent_file(&path, "CONST API", "xin.const.api.client", executable)
                .unwrap()
        );

        let agent = Value::from_file(&path).unwrap();
        let fields = agent.as_dictionary().unwrap();
        assert_eq!(
            fields["AssociatedBundleIdentifiers"].as_string(),
            Some("xin.const.api.client")
        );
        assert_eq!(
            fields["ProgramArguments"].as_array().unwrap()[1].as_string(),
            Some("--tray")
        );
        assert_eq!(fields["RunAtLoad"].as_boolean(), Some(true));
    }
}
