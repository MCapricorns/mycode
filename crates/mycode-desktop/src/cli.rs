//! Arguments handled before a window is created.
//!
//! `--help` and `--version` print and exit. A Linux session with neither
//! `DISPLAY` nor `WAYLAND_DISPLAY` is rejected the same way, so a headless
//! run does not panic inside the GPU surface.

use std::ffi::OsStr;
use std::path::Path;

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

fn nonempty(value: Option<&OsStr>) -> Option<&OsStr> {
    value.filter(|value| !value.is_empty())
}

/// Why a Linux process cannot open a window, if it cannot.
///
/// A set `DISPLAY` that names no X server (for example `DISPLAY=:99` with
/// nothing listening) is the same failure as an unset display: print the
/// message and exit 1 instead of panicking inside the GPU stack.
#[must_use]
pub fn linux_session_problem(
    display: Option<&OsStr>,
    wayland: Option<&OsStr>,
    runtime_dir: Option<&OsStr>,
) -> Option<String> {
    let display = nonempty(display);
    let wayland = nonempty(wayland);
    if display.is_none() && wayland.is_none() {
        return Some(missing_display_message().to_owned());
    }
    if wayland.is_some_and(|name| wayland_socket_exists(name, runtime_dir)) {
        return None;
    }
    if let Some(name) = display {
        let text = name.to_string_lossy();
        if x11_display_reachable(&text) {
            return None;
        }
        return Some(format!(
            "mycode: cannot open a window. DISPLAY={text} is set, but no X server is listening there. mycode-desktop is a graphical app. Use --version or --help."
        ));
    }
    let name = wayland
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(format!(
        "mycode: cannot open a window. WAYLAND_DISPLAY={name} is set, but that Wayland socket is not available. mycode-desktop is a graphical app. Use --version or --help."
    ))
}

fn wayland_socket_exists(name: &OsStr, runtime_dir: Option<&OsStr>) -> bool {
    let name = Path::new(name);
    if name.is_absolute() {
        return name.exists();
    }
    runtime_dir.is_some_and(|dir| Path::new(dir).join(name).exists())
}

/// `true` when a short connect to the X server named by `display` succeeds.
fn x11_display_reachable(display: &str) -> bool {
    let Some((host, number)) = x11_endpoint(display) else {
        return false;
    };
    if host.is_empty() || host == "unix" {
        return unix_display_socket_open(&format!("/tmp/.X11-unix/X{number}"));
    }
    let Ok(display_number) = number.parse::<u16>() else {
        return false;
    };
    let port = 6000u16.saturating_add(display_number);
    let host = if host == "localhost" {
        "127.0.0.1"
    } else {
        host
    };
    let Ok(address) = format!("{host}:{port}").parse() else {
        return false;
    };
    std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(200)).is_ok()
}

/// `(host, display number)` from an X `DISPLAY` value.
#[cfg(unix)]
fn unix_display_socket_open(path: &str) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(not(unix))]
fn unix_display_socket_open(_path: &str) -> bool {
    false
}

fn x11_endpoint(display: &str) -> Option<(&str, &str)> {
    let display = display.trim();
    let (host, rest) = if let Some(rest) = display.strip_prefix("unix:") {
        ("unix", rest)
    } else {
        display.rsplit_once(':')?
    };
    let number = rest.split('.').next().unwrap_or("");
    if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some((host, number))
}

/// Printed when Linux has no display variables.
#[must_use]
pub fn missing_display_message() -> &'static str {
    "mycode: no display found. DISPLAY and WAYLAND_DISPLAY are unset, so a window cannot be opened. mycode-desktop is a graphical app. Use --version or --help."
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
    use super::{
        EarlyAction, early_action, help_text, linux_session_problem, missing_display_message,
        surface_failure_message, version_line, x11_endpoint,
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
    fn a_display_with_no_server_is_a_clear_error() {
        let missing = linux_session_problem(None, None, None).expect("unset");
        assert!(missing.contains("DISPLAY"));
        assert!(missing.contains("--help"));
        let dead = linux_session_problem(Some(std::ffi::OsStr::new(":99")), None, None)
            .expect("no server");
        assert!(dead.contains("DISPLAY=:99"), "{dead}");
        assert!(dead.contains("no X server"), "{dead}");
        assert!(dead.contains("--help"), "{dead}");
        assert_eq!(x11_endpoint(":99"), Some(("", "99")));
        assert_eq!(x11_endpoint("localhost:1.0"), Some(("localhost", "1")));
    }

    #[test]
    fn surface_panic_becomes_a_display_message() {
        let message = surface_failure_message("Failed to create surface").expect("message");
        assert!(message.contains("cannot open a window"));
        assert!(message.contains("--version"));
        assert!(surface_failure_message("failed to connect to X server :99").is_some());
        assert!(surface_failure_message("other panic").is_none());
        assert!(missing_display_message().contains("DISPLAY"));
    }
}
