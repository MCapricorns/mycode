//! Arguments and launch steps handled before a window is created.
//!
//! `--help` and `--version` print and exit. [`launch`] runs the steps every
//! `mycode*` binary takes before its UI.

use mycode_app::{ConfigError, HomeEnv, HomeLayout};

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

/// The self-update helper entry point shared by every `mycode*` binary.
///
/// `Some(code)` means the args requested `--mycode-apply-update` and the
/// helper already ran: the caller exits with `code` without touching the
/// rest of the startup path. `None` means a normal launch.
#[must_use]
pub fn apply_update_from_args(args: &[String]) -> Option<i32> {
    match mycode_app::parse_apply_update_args(args) {
        Ok(Some(request)) => Some(match mycode_app::run_apply_update_helper(&request) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("updater: {error}");
                1
            }
        }),
        Err(error) => {
            eprintln!("updater: {error}");
            Some(1)
        }
        Ok(None) => None,
    }
}

/// What the shared launch steps leave for the UI.
#[derive(Debug)]
pub struct Launch {
    pub home: HomeLayout,
    /// A staged self-update: re-armed for this run's exit, or applied since
    /// the last one.
    pub update: mycode_app::PendingUpdate,
}

/// Resolves the home every `mycode*` binary runs against.
///
/// # Errors
///
/// Returns [`ConfigError`] when the home directory cannot be resolved.
pub fn home() -> Result<HomeLayout, ConfigError> {
    HomeLayout::from_env(HomeEnv::from_process())
}

/// The launch steps the desktop runs before its UI: opens the home, clears
/// self-update leftovers, and settles a staged update.
///
/// `relaunch` is [`mycode_app::reconcile_pending_update`]'s: a re-armed
/// update makes this launch exit into the new version.
///
/// # Errors
///
/// Returns [`ConfigError`] when the home directory cannot be resolved.
pub fn launch(relaunch: bool) -> Result<Launch, ConfigError> {
    Ok(settle(home()?, relaunch))
}

/// [`launch`] over a home the binary already resolved. The desktop claims
/// its home first, so a second desktop never clears update files the
/// running one is using.
#[must_use]
pub fn settle(home: HomeLayout, relaunch: bool) -> Launch {
    // Remove staging directories and install-directory leftovers
    // (`.mycode-previous`, `.mycode-incoming`) from earlier self-updates.
    // The apply helper returns before this, so it does not delete the
    // staged binary it is installing.
    mycode_app::cleanup_stale_stages();
    mycode_app::cleanup_previous_binary();
    let update = mycode_app::reconcile_pending_update(&home, relaunch);
    Launch { home, update }
}

/// `<bin> <version>` for `--version` and the help header.
#[must_use]
pub fn version_line(bin: &str) -> String {
    format!("{bin} {}", env!("CARGO_PKG_VERSION"))
}

/// Text for `--help`. `bin` is the invoking binary name shown in the usage
/// line and the version header.
#[must_use]
pub fn help_text(bin: &str) -> String {
    format!(
        "{}\n\
Coding agent.\n\
\n\
Usage: {bin} [OPTIONS]\n\
\n\
Options:\n\
  -h, --help      Show this help and exit\n\
  -V, --version   Show the version and exit\n",
        version_line(bin)
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
        let version = version_line("mycode-desktop");
        assert_eq!(
            version,
            format!("mycode-desktop {}", env!("CARGO_PKG_VERSION"))
        );
        assert!(help_text("mycode-desktop").contains("--version"));
        assert!(help_text("mycode-desktop").contains("Usage: mycode-desktop "));
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
