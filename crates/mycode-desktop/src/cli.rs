//! Arguments handled before a window is created.
//!
//! `--help` and `--version` print and exit. A Linux session with neither
//! `DISPLAY` nor `WAYLAND_DISPLAY` is rejected the same way, so a headless
//! run does not panic inside the GPU surface.

use std::ffi::OsStr;

/// What `main` should print before it exits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EarlyAction {
    /// `-h` / `--help`.
    Help,
    /// `-V` / `--version`.
    Version,
}

/// `Some` when the argument list asks for help or version.
///
/// Version wins when both are present. Other arguments are ignored here;
/// the updater parser runs first and owns `--mycode-apply-update`.
#[must_use]
pub fn early_action<S: AsRef<str>>(args: &[S]) -> Option<EarlyAction> {
    let mut help = false;
    let mut version = false;
    for arg in args {
        match arg.as_ref() {
            "--help" | "-h" => help = true,
            "--version" | "-V" => version = true,
            _ => {}
        }
    }
    if version {
        Some(EarlyAction::Version)
    } else if help {
        Some(EarlyAction::Help)
    } else {
        None
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

/// Linux graphical session: `DISPLAY` or `WAYLAND_DISPLAY` is non-empty.
#[must_use]
pub fn linux_display_available(display: Option<&OsStr>, wayland: Option<&OsStr>) -> bool {
    fn present(value: Option<&OsStr>) -> bool {
        value.is_some_and(|value| !value.is_empty())
    }
    present(display) || present(wayland)
}

/// Printed when Linux has no display variables.
#[must_use]
pub fn missing_display_message() -> &'static str {
    "mycode: no display found. DISPLAY and WAYLAND_DISPLAY are unset, so a window cannot be opened. mycode-desktop is a graphical app. Use --version or --help."
}

/// A clear message for the GPU panic `Failed to create surface`.
///
/// `None` when `panic_message` is some other failure and the default panic
/// hook should run.
#[must_use]
pub fn surface_failure_message(panic_message: &str) -> Option<String> {
    if panic_message.contains("Failed to create surface") {
        Some(format!(
            "mycode: cannot open a window ({panic_message}). No usable display is available. Use --version or --help."
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EarlyAction, early_action, help_text, linux_display_available, missing_display_message,
        surface_failure_message, version_line,
    };

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
        assert_eq!(early_action(&["--other"]), None);
        let version = version_line();
        assert!(version.starts_with("mycode "), "{version}");
        assert!(help_text().contains("--version"));
        assert!(help_text().contains(&version));
    }

    #[test]
    fn linux_display_requires_a_non_empty_variable() {
        assert!(!linux_display_available(None, None));
        assert!(!linux_display_available(
            Some(std::ffi::OsStr::new("")),
            None
        ));
        assert!(linux_display_available(
            Some(std::ffi::OsStr::new(":0")),
            None
        ));
        assert!(linux_display_available(
            None,
            Some(std::ffi::OsStr::new("wayland-0"))
        ));
    }

    #[test]
    fn surface_panic_becomes_a_display_message() {
        let message = surface_failure_message("Failed to create surface").expect("message");
        assert!(message.contains("cannot open a window"));
        assert!(message.contains("--version"));
        assert!(surface_failure_message("other panic").is_none());
        assert!(missing_display_message().contains("DISPLAY"));
    }
}
