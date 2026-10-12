//! System Python 3 used by `run_code`.
//!
//! The interpreter is called by name (`py.exe -3`, `python3`, …) and resolved
//! from `PATH` by the operating system. No absolute executable path is
//! assembled or stored: the probe verifies the candidate by running it once
//! per process, and every later `run_code` spawns the same name. A later
//! install is picked up on the next launch. On Windows every spawn sets
//! `CREATE_NO_WINDOW` so no console window flashes.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Minimum CPython mycode will run. 3.10 matches the SDK text (`X | None`)
/// and is the first version whose audit-hook and asyncio behavior we test.
pub const MIN_PYTHON_MAJOR: u32 = 3;
pub const MIN_PYTHON_MINOR: u32 = 10;

/// Windows `CREATE_NO_WINDOW`: no console is allocated for the child.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// One interpreter that passed the version check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PythonInterpreter {
    /// Executable name, or the `py` launcher. Resolved from `PATH` by the OS
    /// at spawn time; never an assembled absolute path.
    pub program: PathBuf,
    /// Extra arguments before `-u`, such as `py -3`.
    pub prefix: Vec<String>,
    /// `major.minor.patch` from `sys.version_info`.
    pub version: String,
    /// `sys.version_info[0]`.
    pub major: u32,
    /// `sys.version_info[1]`.
    pub minor: u32,
}

/// Result of the process-wide probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PythonStatus {
    /// A new enough interpreter is on PATH.
    Ready(PythonInterpreter),
    /// Nothing usable was found. `message` includes this OS's install steps.
    Missing { message: String },
}

impl PythonStatus {
    /// Prompt and startup text when `run_code` cannot start.
    #[must_use]
    pub fn unavailable_message(&self) -> Option<&str> {
        match self {
            Self::Missing { message } => Some(message.as_str()),
            Self::Ready(_) => None,
        }
    }
}

static STATUS: OnceLock<PythonStatus> = OnceLock::new();
static OVERRIDE: Mutex<Option<PythonStatus>> = Mutex::new(None);

/// The interpreter for this process. The first call probes PATH; later calls
/// return the same value, including across sessions.
#[must_use]
pub fn python_status() -> PythonStatus {
    if let Some(status) = OVERRIDE
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
    {
        return status;
    }
    STATUS.get_or_init(detect_python).clone()
}

/// `Some` when startup should warn and the prompt should say Python is missing.
#[must_use]
pub fn python_unavailable_message() -> Option<String> {
    python_status().unavailable_message().map(str::to_owned)
}

/// Install steps for the OS this binary was built for.
#[must_use]
pub fn python_install_steps() -> &'static str {
    if cfg!(windows) {
        "Install Python 3.10 or newer from https://www.python.org/downloads/ and enable \"Add python.exe to PATH\", or run `winget install Python.Python.3.12`."
    } else if cfg!(target_os = "macos") {
        "Install Python 3.10 or newer with `brew install python`, or from https://www.python.org/downloads/."
    } else {
        "Install Python 3.10 or newer with the system package manager, for example `sudo apt install python3` or `sudo dnf install python3`."
    }
}

/// Test-only probe replacement. `None` restores the real probe.
#[cfg(test)]
pub fn set_python_status_for_test(status: Option<PythonStatus>) {
    *OVERRIDE.lock().unwrap_or_else(|err| err.into_inner()) = status;
}

/// The real probe, ignoring [`set_python_status_for_test`].
///
/// Other tests swap the process-global override. This read must not observe
/// that swap.
#[cfg(test)]
fn probed_python_status() -> PythonStatus {
    STATUS.get_or_init(detect_python).clone()
}

fn detect_python() -> PythonStatus {
    let mut too_old: Option<String> = None;
    for candidate in candidates() {
        match probe(candidate.name, candidate.prefix) {
            Ok(found) => return PythonStatus::Ready(found),
            Err(ProbeError::TooOld(text)) => {
                too_old.get_or_insert(text);
            }
            Err(ProbeError::Skip) => {}
        }
    }
    let message = if let Some(text) = too_old {
        format!("{text}\n{}", python_install_steps())
    } else {
        format!(
            "Python {MIN_PYTHON_MAJOR}.{MIN_PYTHON_MINOR}+ was not found on PATH. run_code needs the system Python.\n{}",
            python_install_steps()
        )
    };
    PythonStatus::Missing { message }
}

struct Candidate {
    name: &'static str,
    prefix: &'static [&'static str],
}

fn candidates() -> &'static [Candidate] {
    if cfg!(windows) {
        &[
            Candidate {
                name: "py.exe",
                prefix: &["-3"],
            },
            Candidate {
                name: "python.exe",
                prefix: &[],
            },
            Candidate {
                name: "python3.exe",
                prefix: &[],
            },
        ]
    } else {
        &[
            Candidate {
                name: "python3",
                prefix: &[],
            },
            Candidate {
                name: "python",
                prefix: &[],
            },
        ]
    }
}

/// Applies the no-console creation flags for Windows probe spawns.
#[cfg(windows)]
fn no_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;
    // SAFETY of the flag value: CREATE_NO_WINDOW is a documented creation
    // flag; the call itself is safe FFI through std.
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_command: &mut std::process::Command) {}

enum ProbeError {
    Skip,
    TooOld(String),
}

/// Runs one candidate by name and keeps it when the version is new enough.
///
/// A Windows Store alias (`python.exe` under `WindowsApps`) opens the Store
/// instead of running Python: it prints nothing useful and exits non-zero, so
/// the probe skips it without ever assembling a path.
fn probe(name: &str, prefix: &[&str]) -> Result<PythonInterpreter, ProbeError> {
    let mut command = std::process::Command::new(name);
    command.args(prefix);
    command.arg("-c");
    command.arg(
        "import sys; print(f'{sys.version_info[0]}.{sys.version_info[1]}.{sys.version_info[2]}')",
    );
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::null());
    no_window(&mut command);
    let child = command.spawn().map_err(|_| ProbeError::Skip)?;
    let finished = wait_child(child, Duration::from_secs(8)).map_err(|_| ProbeError::Skip)?;
    if !finished.status.success() {
        return Err(ProbeError::Skip);
    }
    let text = String::from_utf8_lossy(&finished.stdout);
    let Some((major, minor, patch)) = parse_version(text.trim()) else {
        return Err(ProbeError::Skip);
    };
    if major < MIN_PYTHON_MAJOR || (major == MIN_PYTHON_MAJOR && minor < MIN_PYTHON_MINOR) {
        return Err(ProbeError::TooOld(format!(
            "Found Python {major}.{minor}.{patch} at {name}, but run_code needs Python {MIN_PYTHON_MAJOR}.{MIN_PYTHON_MINOR} or newer.",
        )));
    }
    Ok(PythonInterpreter {
        program: PathBuf::from(name),
        prefix: prefix.iter().map(|part| (*part).to_owned()).collect(),
        version: format!("{major}.{minor}.{patch}"),
        major,
        minor,
    })
}

struct FinishedChild {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
}

fn wait_child(
    child: std::process::Child,
    limit: Duration,
) -> Result<FinishedChild, std::io::Error> {
    let mut child = child;
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let mut stdout = Vec::new();
            if let Some(mut pipe) = child.stdout.take() {
                use std::io::Read as _;
                let _ = pipe.read_to_end(&mut stdout);
            }
            return Ok(FinishedChild { status, stdout });
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "python version probe timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Parses `3.12.3`.
#[must_use]
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::{
        MIN_PYTHON_MINOR, PythonInterpreter, PythonStatus, parse_version, probed_python_status,
        python_install_steps,
    };

    #[test]
    fn version_parser_accepts_three_components() {
        assert_eq!(parse_version("3.12.3"), Some((3, 12, 3)));
        assert_eq!(parse_version("3.10.0"), Some((3, 10, 0)));
        assert!(parse_version("3.12").is_some());
        assert!(parse_version("python 3.12.3").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_candidates_call_py_first_by_name() {
        let candidates = super::candidates();
        assert_eq!(candidates[0].name, "py.exe");
        assert_eq!(candidates[0].prefix, &["-3"]);
        for candidate in candidates {
            assert!(
                !candidate.name.contains('\\') && !candidate.name.contains('/'),
                "{} must be a bare PATH-resolved name",
                candidate.name
            );
        }
    }

    #[test]
    fn a_ready_interpreter_stores_the_name_not_an_assembled_path() {
        let ready = PythonStatus::Ready(PythonInterpreter {
            program: "py.exe".into(),
            prefix: vec!["-3".to_owned()],
            version: "3.14.1".to_owned(),
            major: 3,
            minor: 14,
        });
        assert!(ready.unavailable_message().is_none());
    }

    #[test]
    fn this_process_finds_python_or_explains_how() {
        match probed_python_status() {
            PythonStatus::Ready(found) => {
                assert!(found.major > 3 || found.minor >= MIN_PYTHON_MINOR);
                assert!(
                    found.program.is_relative(),
                    "the program must stay a PATH-resolved name, got {}",
                    found.program.display()
                );
            }
            PythonStatus::Missing { message } => {
                assert!(message.contains("3.10"));
                assert!(message.contains(python_install_steps()));
            }
        }
    }
}
