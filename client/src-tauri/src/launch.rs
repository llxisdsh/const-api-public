use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

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

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Invocation {
    Desktop(LaunchMode),
    Cli(CliCommand),
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CliCommand {
    Help,
    Version,
    Image(Vec<OsString>),
    Media(Vec<OsString>),
}

pub(crate) fn attach_cli_console() {
    #[cfg(windows)]
    // SAFETY: packaged GUI builds can attach to the invoking terminal. Redirected
    // stdout/stderr also work when the caller has no console to attach to.
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
}

impl CliCommand {
    pub(crate) fn run(self) -> ExitCode {
        attach_cli_console();
        let name = env!("CARGO_PKG_NAME");
        match self {
            Self::Image(args) => return crate::image_cli::run(args.into_iter()),
            Self::Media(args) => return crate::media_cli::run(args),
            Self::Help => {
                let _ = writeln!(
                    std::io::stdout().lock(),
                    "Usage: {name} [--help | --version | image ... | media ...]\n\nRun without arguments to open the desktop app.\nImages: {name} image --help\nVideo and speech: {name} media --help"
                );
            }
            Self::Version => {
                let _ = writeln!(
                    std::io::stdout().lock(),
                    "{name} {}",
                    env!("CARGO_PKG_VERSION")
                );
            }
        }
        ExitCode::SUCCESS
    }
}

pub(crate) fn parse_args<I>(args: I) -> Result<Invocation, LaunchError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(Invocation::Desktop(LaunchMode::Desktop));
    };
    let action = match first.to_str() {
        Some("image") => return Ok(Invocation::Cli(CliCommand::Image(args.collect()))),
        Some("media") => return Ok(Invocation::Cli(CliCommand::Media(args.collect()))),
        Some("--help" | "-h") => Invocation::Cli(CliCommand::Help),
        Some("--version" | "-V") => Invocation::Cli(CliCommand::Version),
        // Older autostart registrations use --background.
        Some("--tray" | "--background") => Invocation::Desktop(LaunchMode::Tray),
        Some(_) => {
            return Err(LaunchError {
                message: "Unsupported application argument. See --help for usage.".to_string(),
            });
        }
        None => {
            return Err(LaunchError {
                message: "application arguments must be valid Unicode".to_string(),
            });
        }
    };
    if args.next().is_some() {
        return Err(LaunchError {
            message: format!("{} must be used alone", first.to_string_lossy()),
        });
    }
    Ok(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Invocation, LaunchError> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_desktop_and_internal_tray_launches() {
        assert_eq!(
            parse(&[]).unwrap(),
            Invocation::Desktop(LaunchMode::Desktop)
        );
        for flag in ["--tray", "--background"] {
            assert_eq!(
                parse(&[flag]).unwrap(),
                Invocation::Desktop(LaunchMode::Tray)
            );
        }

        let legacy_mode: LaunchMode = serde_json::from_str("\"background\"").unwrap();
        assert_eq!(legacy_mode, LaunchMode::Tray);
        assert_eq!(
            serde_json::to_string(&LaunchMode::Tray).unwrap(),
            "\"tray\""
        );
    }

    #[test]
    fn discovers_image_commands_without_entering_desktop_mode() {
        for flag in ["--help", "-h"] {
            assert_eq!(parse(&[flag]).unwrap(), Invocation::Cli(CliCommand::Help));
        }
        for flag in ["--version", "-V"] {
            assert_eq!(
                parse(&[flag]).unwrap(),
                Invocation::Cli(CliCommand::Version)
            );
        }
        assert_eq!(
            parse(&["image", "--help"]).unwrap(),
            Invocation::Cli(CliCommand::Image(vec!["--help".into()]))
        );
    }

    #[test]
    fn rejects_retired_commands_and_mixed_launch_modes() {
        for args in [
            &["--headless"][..],
            &["--check"],
            &["--tray", "--background"],
            &["--help", "--tray"],
        ] {
            assert!(parse(args).is_err());
        }
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
