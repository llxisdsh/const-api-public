use crate::launch::LaunchMode;
use anyhow::{Context, Result, anyhow};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const UPDATE_RELAUNCH_SCHEMA_VERSION: u32 = 1;
const UPDATE_RELAUNCH_FILE_NAME: &str = "update-relaunch.json";
const UPDATE_RELAUNCH_MAX_AGE: Duration = Duration::from_secs(2 * 60 * 60);
const UPDATE_RELAUNCH_FUTURE_SKEW: Duration = Duration::from_secs(5 * 60);
#[cfg(any(target_os = "macos", test))]
pub(crate) const MACOS_UPDATE_REOPEN_SUPPRESSION: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct UpdateRelaunchMarker {
    schema_version: u32,
    mode: LaunchMode,
    target_version: String,
    created_at_unix_seconds: u64,
}

pub(crate) fn stage_update_relaunch(
    state_dir: &Path,
    mode: LaunchMode,
    target_version: &str,
) -> Result<()> {
    stage_update_relaunch_at(state_dir, mode, target_version, unix_now())
}

pub(crate) fn consume_update_relaunch_mode(
    state_dir: &Path,
    current_version: &str,
) -> Result<Option<LaunchMode>> {
    consume_update_relaunch_mode_at(state_dir, current_version, unix_now())
}

pub(crate) fn peek_update_relaunch_mode(
    state_dir: &Path,
    current_version: &str,
) -> Result<Option<LaunchMode>> {
    peek_update_relaunch_mode_at(state_dir, current_version, unix_now())
}

pub(crate) fn clear_update_relaunch(state_dir: &Path) -> Result<()> {
    remove_if_present(&update_relaunch_path(state_dir))
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn should_suppress_macos_update_reopen(
    restored_update_tray: bool,
    has_visible_windows: bool,
    elapsed: Duration,
) -> bool {
    restored_update_tray && !has_visible_windows && elapsed <= MACOS_UPDATE_REOPEN_SUPPRESSION
}

fn stage_update_relaunch_at(
    state_dir: &Path,
    mode: LaunchMode,
    target_version: &str,
    now_unix_seconds: u64,
) -> Result<()> {
    let target_version = parse_version(target_version, "target update version")?;
    let marker = UpdateRelaunchMarker {
        schema_version: UPDATE_RELAUNCH_SCHEMA_VERSION,
        mode,
        target_version: target_version.to_string(),
        created_at_unix_seconds: now_unix_seconds,
    };
    let encoded = serde_json::to_vec_pretty(&marker).context("encode update relaunch marker")?;
    write_atomically(&update_relaunch_path(state_dir), &encoded)
}

fn consume_update_relaunch_mode_at(
    state_dir: &Path,
    current_version: &str,
    now_unix_seconds: u64,
) -> Result<Option<LaunchMode>> {
    let mode = peek_update_relaunch_mode_at(state_dir, current_version, now_unix_seconds)?;
    if mode.is_some() {
        consume_marker_file(&update_relaunch_path(state_dir))?;
    }
    Ok(mode)
}

fn peek_update_relaunch_mode_at(
    state_dir: &Path,
    current_version: &str,
    now_unix_seconds: u64,
) -> Result<Option<LaunchMode>> {
    let path = update_relaunch_path(state_dir);
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read update relaunch marker {}", path.display()));
        }
    };
    let marker = match serde_json::from_slice::<UpdateRelaunchMarker>(&raw) {
        Ok(marker) => marker,
        Err(error) => {
            let _ = remove_if_present(&path);
            return Err(error).context("decode update relaunch marker");
        }
    };
    if marker.schema_version != UPDATE_RELAUNCH_SCHEMA_VERSION {
        let _ = remove_if_present(&path);
        return Err(anyhow!("update relaunch marker is incompatible"));
    }

    let future_limit = now_unix_seconds.saturating_add(UPDATE_RELAUNCH_FUTURE_SKEW.as_secs());
    let age = now_unix_seconds.saturating_sub(marker.created_at_unix_seconds);
    if marker.created_at_unix_seconds > future_limit || age > UPDATE_RELAUNCH_MAX_AGE.as_secs() {
        remove_if_present(&path)?;
        return Ok(None);
    }

    let current_version = parse_version(current_version, "current client version")?;
    let target_version = match parse_version(&marker.target_version, "marker target version") {
        Ok(version) => version,
        Err(error) => {
            let _ = remove_if_present(&path);
            return Err(error);
        }
    };
    if current_version != target_version {
        // The old process can be started briefly while the installer is still
        // handing off. Leave a fresh marker for the exact target version.
        return Ok(None);
    }

    Ok(Some(marker.mode))
}

fn update_relaunch_path(state_dir: &Path) -> PathBuf {
    state_dir.join(UPDATE_RELAUNCH_FILE_NAME)
}

fn parse_version(raw: &str, label: &str) -> Result<Version> {
    let value = raw.trim().strip_prefix('v').unwrap_or(raw.trim());
    Version::parse(value).with_context(|| format!("parse {label}"))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn write_atomically(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("update relaunch marker path has no parent"))?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "create update relaunch state directory {}",
            parent.display()
        )
    })?;
    let temp_path = parent.join(format!(
        ".{UPDATE_RELAUNCH_FILE_NAME}.{}.tmp",
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp_path)
        .with_context(|| format!("create update relaunch marker {}", temp_path.display()))?;
    file.write_all(content)?;
    file.sync_all()?;
    drop(file);

    if path.exists() {
        fs::remove_file(path)
            .with_context(|| format!("replace stale update relaunch marker {}", path.display()))?;
    }
    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error)
            .with_context(|| format!("publish update relaunch marker {}", path.display()));
    }
    Ok(())
}

fn consume_marker_file(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("update relaunch marker path has no parent"))?;
    let consumed_path = parent.join(format!(
        ".{UPDATE_RELAUNCH_FILE_NAME}.{}.consumed",
        std::process::id()
    ));
    match fs::rename(path, &consumed_path) {
        Ok(()) => {
            let _ = fs::remove_file(consumed_path);
            Ok(())
        }
        Err(rename_error) => fs::remove_file(path).with_context(|| {
            format!(
                "consume update relaunch marker {} after rename failed: {rename_error}",
                path.display()
            )
        }),
    }
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("remove update relaunch marker {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn matching_target_consumes_marker_once() {
        let temp = tempfile::tempdir().unwrap();
        stage_update_relaunch_at(temp.path(), LaunchMode::Tray, "v0.2.0", NOW).unwrap();

        assert_eq!(
            peek_update_relaunch_mode_at(temp.path(), "0.2.0", NOW + 1).unwrap(),
            Some(LaunchMode::Tray)
        );
        assert!(update_relaunch_path(temp.path()).exists());
        assert_eq!(
            consume_update_relaunch_mode_at(temp.path(), "0.2.0", NOW + 1).unwrap(),
            Some(LaunchMode::Tray)
        );
        assert_eq!(
            consume_update_relaunch_mode_at(temp.path(), "0.2.0", NOW + 2).unwrap(),
            None
        );
    }

    #[test]
    fn fresh_marker_waits_for_the_exact_target_version() {
        let temp = tempfile::tempdir().unwrap();
        stage_update_relaunch_at(temp.path(), LaunchMode::Desktop, "0.2.0", NOW).unwrap();

        assert_eq!(
            consume_update_relaunch_mode_at(temp.path(), "0.1.9", NOW + 1).unwrap(),
            None
        );
        assert!(update_relaunch_path(temp.path()).exists());
        assert_eq!(
            consume_update_relaunch_mode_at(temp.path(), "0.2.0", NOW + 2).unwrap(),
            Some(LaunchMode::Desktop)
        );
    }

    #[test]
    fn expired_or_future_marker_is_removed_without_restoring() {
        let temp = tempfile::tempdir().unwrap();
        stage_update_relaunch_at(temp.path(), LaunchMode::Tray, "0.2.0", NOW).unwrap();
        assert_eq!(
            consume_update_relaunch_mode_at(
                temp.path(),
                "0.2.0",
                NOW + UPDATE_RELAUNCH_MAX_AGE.as_secs() + 1,
            )
            .unwrap(),
            None
        );
        assert!(!update_relaunch_path(temp.path()).exists());

        stage_update_relaunch_at(
            temp.path(),
            LaunchMode::Tray,
            "0.2.0",
            NOW + UPDATE_RELAUNCH_FUTURE_SKEW.as_secs() + 1,
        )
        .unwrap();
        assert_eq!(
            consume_update_relaunch_mode_at(temp.path(), "0.2.0", NOW).unwrap(),
            None
        );
        assert!(!update_relaunch_path(temp.path()).exists());
    }

    #[test]
    fn marker_rejects_invalid_versions() {
        let temp = tempfile::tempdir().unwrap();
        assert!(stage_update_relaunch_at(temp.path(), LaunchMode::Tray, "latest", NOW).is_err());
    }

    #[test]
    fn macos_reopen_guard_is_narrow_and_time_bounded() {
        assert!(should_suppress_macos_update_reopen(
            true,
            false,
            Duration::from_secs(2)
        ));
        assert!(!should_suppress_macos_update_reopen(
            true,
            true,
            Duration::from_secs(2)
        ));
        assert!(!should_suppress_macos_update_reopen(
            false,
            false,
            Duration::from_secs(2)
        ));
        assert!(!should_suppress_macos_update_reopen(
            true,
            false,
            MACOS_UPDATE_REOPEN_SUPPRESSION + Duration::from_millis(1)
        ));
    }
}
