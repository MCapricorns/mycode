//! Arguments handled before a window is created.
//!
//! `--help` and `--version` print and exit.

/// What `main` should print before it exits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EarlyAction {
    /// `-h` / `--help`.
    Help,
    /// `-V` / `--version`.
    Version,
    /// A flag this binary does not accept.
    Unknown(String),
}

/// `Some` when the argument list asks for help, version, or an unknown flag.
///
/// Version wins when both help and version are present. An unknown flag is
/// reported only when neither of those is present. The updater parser runs
/// first and owns `--mycode-apply-update`.
#[must_use]
pub fn early_action<S: AsRef<str>>(args: &[S]) -> Option<EarlyAction> {
    let mut help = false;
    let mut version = false;
    let mut unknown = None;
    for arg in args {
        match arg.as_ref() {
            "--help" | "-h" => help = true,
            "--version" | "-V" => version = true,
            other if other.starts_with('-') && unknown.is_none() => {
                unknown = Some(other.to_owned());
            }
            _ => {}
        }
    }
    if version {
        Some(EarlyAction::Version)
    } else if help {
        Some(EarlyAction::Help)
    } else {
        unknown.map(EarlyAction::Unknown)
    }
}

/// `mycode <version>` for `--version` and the help header.
#[must_use]
pub fn version_line() -> String {
    format!("mycode {}", env!("CARGO_PKG_VERSION"))
}

/// Text for `--help`.
#[must_use]
pub fn help_text() -> String {
    format!(
        "{}\n\
Desktop coding agent.\n\
\n\
Usage: mycode-desktop [OPTIONS]\n\
\n\
Options:\n\
  -h, --help      Show this help and exit\n\
  -V, --version   Show the version and exit\n",
        version_line()
    )
}

/// A clear message for a display or surface panic.
///
/// `None` when `panic_message` is some other failure and the default panic
/// hook should run.
#[must_use]
pub fn surface_failure_message(panic_message: &str) -> Option<String> {
    let lower = panic_message.to_ascii_lowercase();
    if lower.contains("failed to create surface")
        || lower.contains("failed to connect to x server")
        || lower.contains("display is not set")
    {
        Some(format!(
            "mycode: cannot open a window ({panic_message}). No usable display is available. Use --version or --help."
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{EarlyAction, early_action, help_text, surface_failure_message, version_line};

    #[test]
    fn version_and_help_are_recognized() {
        assert_eq!(early_action(&["--version"]), Some(EarlyAction::Version));
        assert_eq!(early_action(&["-V"]), Some(EarlyAction::Version));
        assert_eq!(early_action(&["--help"]), Some(EarlyAction::Help));
        assert_eq!(early_action(&["-h"]), Some(EarlyAction::Help));
        assert_eq!(
            early_action(&["--help", "--version"]),
            Some(EarlyAction::Version)
        );
        assert_eq!(early_action::<&str>(&[]), None);
        match early_action(&["--other"]) {
            Some(EarlyAction::Unknown(flag)) => assert_eq!(flag, "--other"),
            other => panic!("unknown flag should be an error, got {other:?}"),
        }
        assert_eq!(early_action(&["folder"]), None);
        let version = version_line();
        assert!(version.starts_with("mycode "), "{version}");
        assert!(help_text().contains("--version"));
        assert!(help_text().contains(&version));
    }

    #[test]
    fn surface_panic_becomes_a_display_message() {
        let message = surface_failure_message("Failed to create surface").expect("message");
        assert!(message.contains("cannot open a window"));
        assert!(message.contains("--version"));
        assert!(surface_failure_message("failed to connect to X server :99").is_some());
        assert!(surface_failure_message("other panic").is_none());
    }
}
