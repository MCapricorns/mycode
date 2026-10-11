//! System Python 3 used by `run_code`.
//!
//! The probe runs once per process and is reused for every session in that
//! process, so the system prompt and the interpreter stay stable for a
//! session. A later install is picked up on the next launch.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Minimum CPython mycode will run. 3.10 matches the SDK text (`X | None`)
/// and is the first version whose audit-hook and asyncio behavior we test.
pub const MIN_PYTHON_MAJOR: u32 = 3;
pub const MIN_PYTHON_MINOR: u32 = 10;

/// One interpreter that passed the version check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PythonInterpreter {
    /// Executable, or the `py` launcher.
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
        "Install Python 3.10 or newer from https://www.python.org/downloads/ and enable \"Add python.exe to PATH\", or run `winget install Python.Python.3.12`. The Microsoft Store alias under WindowsApps is skipped."
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

fn detect_python() -> PythonStatus {
    let mut too_old: Option<String> = None;
    for candidate in candidates() {
        for program in search_path(candidate.name) {
            if is_windowsapps_stub(&program) {
                continue;
            }
            match probe(&program, candidate.prefix) {
                Ok(found) => return PythonStatus::Ready(found),
                Err(ProbeError::TooOld(text)) => {
                    too_old.get_or_insert(text);
                }
                Err(ProbeError::Skip) => {}
            }
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
                name: "py",
                prefix: &["-3"],
            },
            Candidate {
                name: "python3",
                prefix: &[],
            },
            Candidate {
                name: "python",
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

/// The Windows Store stub (`...\WindowsApps\python.exe`) opens the Store
/// instead of running Python. `py.exe` is the real launcher and is not skipped.
///
/// The check is textual so a Windows path is recognized on every host.
#[must_use]
pub fn is_windowsapps_stub(path: &Path) -> bool {
    let text = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let python = text.ends_with("/python.exe")
        || text.ends_with("/python3.exe")
        || text.ends_with("/python")
        || text.ends_with("/python3");
    python && text.contains("/windowsapps/")
}

fn search_path(name: &str) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let direct = dir.join(name);
        if direct.is_file() {
            found.push(direct);
            continue;
        }
        if cfg!(windows) && Path::new(name).extension().is_none() {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                found.push(exe);
            }
        }
    }
    found
}

enum ProbeError {
    Skip,
    TooOld(String),
}

fn probe(program: &Path, prefix: &[&str]) -> Result<PythonInterpreter, ProbeError> {
    let mut command = std::process::Command::new(program);
    command.args(prefix);
    command.arg("-c");
    command.arg(
        "import sys; print(f'{sys.version_info[0]}.{sys.version_info[1]}.{sys.version_info[2]}')",
    );
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::null());
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
            "Found Python {major}.{minor}.{patch} at {}, but run_code needs Python {MIN_PYTHON_MAJOR}.{MIN_PYTHON_MINOR} or newer.",
            program.display()
        )));
    }
    Ok(PythonInterpreter {
        program: program.to_path_buf(),
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
        MIN_PYTHON_MINOR, PythonStatus, is_windowsapps_stub, parse_version, python_install_steps,
        python_status,
    };

    #[test]
    fn version_parser_accepts_three_components() {
        assert_eq!(parse_version("3.12.3"), Some((3, 12, 3)));
        assert_eq!(parse_version("3.10.0"), Some((3, 10, 0)));
        assert!(parse_version("3.12").is_some());
        assert!(parse_version("python 3.12.3").is_none());
    }

    #[test]
    fn windowsapps_python_stub_is_skipped() {
        let stub =
            std::path::Path::new(r"C:\Users\me\AppData\Local\Microsoft\WindowsApps\python.exe");
        assert!(is_windowsapps_stub(stub));
        let real = std::path::Path::new(r"C:\Windows\py.exe");
        assert!(!is_windowsapps_stub(real));
        let program_files = std::path::Path::new(r"C:\Program Files\Python312\python.exe");
        assert!(!is_windowsapps_stub(program_files));
    }

    #[test]
    fn this_process_finds_python_or_explains_how() {
        match python_status() {
            PythonStatus::Ready(found) => {
                assert!(found.major > 3 || found.minor >= MIN_PYTHON_MINOR);
                assert!(found.program.is_file() || found.program.exists());
            }
            PythonStatus::Missing { message } => {
                assert!(message.contains("3.10"));
                assert!(message.contains(python_install_steps()));
            }
        }
    }
}
