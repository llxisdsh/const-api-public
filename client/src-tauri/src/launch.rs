use std::ffi::OsString;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LaunchMode {
    Desktop,
    #[serde(alias = "background")]
    Tray,
}

impl LaunchMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Tray => "tray",
        }
    }
}

pub(crate) fn update_relaunch_mode_for_window(window_visible: bool) -> LaunchMode {
    if window_visible {
        LaunchMode::Desktop
    } else {
        LaunchMode::Tray
    }
}

pub(crate) fn resolve_update_relaunch_mode(
    requested: LaunchMode,
    update_mode: Option<LaunchMode>,
) -> LaunchMode {
    update_mode.unwrap_or(requested)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaunchError {
    message: String,
}

impl LaunchError {
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

pub(crate) fn parse_args<I>(args: I) -> Result<LaunchMode, LaunchError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut mode = LaunchMode::Desktop;
    let mut selected = false;
    for raw in args {
        let Some(arg) = raw.to_str() else {
            return Err(LaunchError {
                message: "application arguments must be valid Unicode".to_string(),
            });
        };
        match arg {
            // `--tray` is an internal desktop launch hint used by autostart.
            // Keep the old alias so existing registrations continue to work.
            "--tray" | "--background" if !selected => {
                mode = LaunchMode::Tray;
                selected = true;
            }
            "--tray" | "--background" => {
                return Err(LaunchError {
                    message: "only one desktop launch mode may be specified".to_string(),
                });
            }
            _ => {
                return Err(LaunchError {
                    message: format!(
                        "unsupported application argument: {arg}. CONST API is a desktop application"
                    ),
                });
            }
        }
    }
    Ok(mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<LaunchMode, LaunchError> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_desktop_and_internal_tray_launches() {
        assert_eq!(parse(&[]).unwrap(), LaunchMode::Desktop);
        assert_eq!(parse(&["--tray"]).unwrap(), LaunchMode::Tray);
        assert_eq!(parse(&["--background"]).unwrap(), LaunchMode::Tray);

        let legacy_mode: LaunchMode = serde_json::from_str("\"background\"").unwrap();
        assert_eq!(legacy_mode, LaunchMode::Tray);
        assert_eq!(
            serde_json::to_string(&LaunchMode::Tray).unwrap(),
            "\"tray\""
        );
    }

    #[test]
    fn rejects_public_cli_and_multiple_launch_modes() {
        for argument in ["--headless", "--check", "--help", "--version"] {
            assert!(
                parse(&[argument])
                    .unwrap_err()
                    .message()
                    .contains("desktop application")
            );
        }
        assert!(
            parse(&["--tray", "--background"])
                .unwrap_err()
                .message()
                .contains("only one")
        );
    }

    #[test]
    fn update_relaunch_restores_the_recorded_window_state() {
        assert_eq!(update_relaunch_mode_for_window(true), LaunchMode::Desktop);
        assert_eq!(update_relaunch_mode_for_window(false), LaunchMode::Tray);
        assert_eq!(
            resolve_update_relaunch_mode(LaunchMode::Desktop, Some(LaunchMode::Tray)),
            LaunchMode::Tray
        );
        assert_eq!(
            resolve_update_relaunch_mode(LaunchMode::Tray, Some(LaunchMode::Desktop)),
            LaunchMode::Desktop
        );
        assert_eq!(
            resolve_update_relaunch_mode(LaunchMode::Desktop, None),
            LaunchMode::Desktop
        );
    }
}
