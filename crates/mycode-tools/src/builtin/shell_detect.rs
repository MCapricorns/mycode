//! Default platform-shell discovery and the process-wide runtime preference.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::builtin::fs_search::lexical_normalize;

static RUNTIME_SHELL: RwLock<Option<DetectedShell>> = RwLock::new(None);

/// Kind of platform shell used by the `shell` tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellKind {
    /// PowerShell 7+ (`pwsh`).
    Pwsh,
    /// Bash (`bash` / `sh`).
    Bash,
    /// Windows `cmd.exe`. Runtime fallback only; [`Self::parse`] does not
    /// accept it, so settings cannot persist this kind.
    Cmd,
}

impl ShellKind {
    /// Stable settings / wire name for this kind.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pwsh => "pwsh",
            Self::Bash => "bash",
            Self::Cmd => "cmd",
        }
    }

    /// Parses a settings / wire name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pwsh" => Some(Self::Pwsh),
            "bash" => Some(Self::Bash),
            // `cmd` is a runtime fallback, not a stored settings kind.
            _ => None,
        }
    }

    /// Classifies a program path from its file stem.
    #[must_use]
    pub fn from_program(path: &Path) -> Self {
        let stem = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        match stem.to_ascii_lowercase().as_str() {
            "pwsh" => Self::Pwsh,
            "bash" | "sh" => Self::Bash,
            "cmd" => Self::Cmd,
            _ => {
                #[cfg(windows)]
                {
                    Self::Pwsh
                }
                #[cfg(not(windows))]
                {
                    Self::Bash
                }
            }
        }
    }
}

/// A resolved shell program and its kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetectedShell {
    /// Interpreter family used to build launch arguments.
    pub kind: ShellKind,
    /// Absolute or PATH-resolved program path.
    pub program: PathBuf,
}

/// Picks a detected shell, or `cmd.exe` when nothing else is available.
///
/// `cmd_path` is used only when `detected` is empty. Callers must not persist
/// the `cmd` kind into settings.
#[must_use]
#[cfg_attr(not(windows), allow(dead_code))]
pub fn select_windows_shell(
    detected: Option<DetectedShell>,
    cmd_path: Option<PathBuf>,
) -> Option<DetectedShell> {
    if let Some(detected) = detected {
        return Some(detected);
    }
    cmd_path.map(|program| DetectedShell {
        kind: ShellKind::Cmd,
        program,
    })
}

/// Arguments for a `cmd.exe /d /s /c` invocation.
#[must_use]
#[cfg_attr(not(windows), allow(dead_code))]
pub fn cmd_fallback_args(command: &str) -> Vec<String> {
    vec![
        "/d".to_owned(),
        "/s".to_owned(),
        "/c".to_owned(),
        command.to_owned(),
    ]
}

/// First pinnable platform shell for the current host.
#[must_use]
pub fn detect_default_shell() -> Option<DetectedShell> {
    #[cfg(windows)]
    {
        detect_windows_shell_with(&WindowsShellEnv::from_process())
    }
    #[cfg(not(windows))]
    {
        detect_posix_shell()
    }
}

/// Resolves one shell kind, including a WindowsApps execution alias when no
/// regular `pwsh.exe` image is visible.
#[must_use]
pub fn detect_shell_kind(kind: ShellKind) -> Option<DetectedShell> {
    #[cfg(windows)]
    {
        let candidates = windows_shell_candidates(&WindowsShellEnv::from_process())
            .into_iter()
            .filter(|(candidate, _)| *candidate == kind)
            .collect();
        pick_windows_shell(candidates)
    }
    #[cfg(not(windows))]
    {
        detect_posix_shell().filter(|shell| shell.kind == kind)
    }
}

/// Replaces the process-wide shell preference used by execute.
pub fn set_runtime_shell(shell: Option<DetectedShell>) {
    *RUNTIME_SHELL
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = shell;
}

/// Current process-wide shell preference, if desktop or a test set one.
#[must_use]
pub(crate) fn runtime_shell() -> Option<DetectedShell> {
    RUNTIME_SHELL
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[cfg(not(windows))]
fn detect_posix_shell() -> Option<DetectedShell> {
    let path = std::env::var_os("PATH");
    let mut candidates = vec![PathBuf::from("/bin/bash")];
    candidates.extend(path_named_files(path.as_deref(), "bash"));
    candidates.extend(path_named_files(path.as_deref(), "sh"));
    candidates
        .into_iter()
        .find(|program| program.is_file())
        .map(|program| DetectedShell {
            kind: ShellKind::Bash,
            program,
        })
}

#[cfg(windows)]
#[derive(Debug, Default)]
pub(crate) struct WindowsShellEnv {
    path: Option<std::ffi::OsString>,
    program_files: Option<std::ffi::OsString>,
    program_files_x86: Option<std::ffi::OsString>,
    local_app_data: Option<std::ffi::OsString>,
    user_profile: Option<std::ffi::OsString>,
}

#[cfg(windows)]
impl WindowsShellEnv {
    fn from_process() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            program_files: std::env::var_os("ProgramFiles"),
            program_files_x86: std::env::var_os("ProgramFiles(x86)"),
            local_app_data: std::env::var_os("LOCALAPPDATA")
                .or_else(|| std::env::var_os("LocalAppData")),
            user_profile: std::env::var_os("USERPROFILE"),
        }
    }
}

/// First pinnable candidate from an injected Windows environment.
#[cfg(windows)]
#[must_use]
pub(crate) fn detect_windows_shell_with(env: &WindowsShellEnv) -> Option<DetectedShell> {
    pick_windows_shell(windows_shell_candidates(env))
}

/// Prefers a regular executable, then a Store execution alias for pwsh.
#[cfg(windows)]
fn pick_windows_shell(mut candidates: Vec<(ShellKind, PathBuf)>) -> Option<DetectedShell> {
    if let Some(index) = candidates
        .iter()
        .position(|(_, program)| image_is_regular_executable(program))
    {
        let (kind, program) = candidates.swap_remove(index);
        return Some(DetectedShell { kind, program });
    }
    candidates
        .into_iter()
        .find(|(kind, program)| {
            matches!(kind, ShellKind::Pwsh) && is_store_execution_alias(program)
        })
        .map(|(kind, program)| DetectedShell { kind, program })
}

/// A regular file larger than a Store execution alias (at most 64 bytes).
#[cfg(windows)]
fn image_is_regular_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 64)
}

/// A Store execution alias `pwsh.exe` under `WindowsApps` (at most 64 bytes).
///
/// The Store publishes these as non-directory files. They are not PE images,
/// but launching them starts the real package, so they are usable when the
/// package directory itself is not readable.
#[cfg(windows)]
fn is_store_execution_alias(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if !name.eq_ignore_ascii_case("pwsh.exe") {
        return false;
    }
    let in_windows_apps = path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("WindowsApps")
    });
    if !in_windows_apps {
        return false;
    }
    std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.is_dir() && meta.len() <= 64)
}

/// Candidate programs in discovery order. Existence is not required.
///
/// Windows PowerShell 5.1 and `cmd` are deliberate non-candidates: detection
/// prefers PowerShell 7 and falls back to Git bash.
#[cfg(windows)]
fn windows_shell_candidates(env: &WindowsShellEnv) -> Vec<(ShellKind, PathBuf)> {
    let mut candidates = Vec::new();

    candidates.extend(
        path_named_files(env.path.as_deref(), "pwsh.exe")
            .into_iter()
            .map(|program| (ShellKind::Pwsh, program)),
    );

    if let Some(program_files) = env.program_files.as_ref() {
        let program_files = Path::new(program_files);
        candidates.push((
            ShellKind::Pwsh,
            program_files.join("PowerShell").join("7").join("pwsh.exe"),
        ));
        candidates.push((
            ShellKind::Pwsh,
            program_files
                .join("PowerShell")
                .join("7-preview")
                .join("pwsh.exe"),
        ));
        candidates.extend(
            fuzzy_windows_apps_pwsh(&program_files.join("WindowsApps"))
                .into_iter()
                .map(|program| (ShellKind::Pwsh, program)),
        );
    }

    if let Some(program_files_x86) = env.program_files_x86.as_ref() {
        candidates.push((
            ShellKind::Pwsh,
            Path::new(program_files_x86)
                .join("PowerShell")
                .join("7")
                .join("pwsh.exe"),
        ));
    }

    if let Some(local_app_data) = env.local_app_data.as_ref() {
        let local_app_data = Path::new(local_app_data);
        candidates.push((
            ShellKind::Pwsh,
            local_app_data
                .join("Microsoft")
                .join("WinGet")
                .join("Links")
                .join("pwsh.exe"),
        ));
        candidates.push((
            ShellKind::Pwsh,
            local_app_data
                .join("Microsoft")
                .join("WindowsApps")
                .join("pwsh.exe"),
        ));
    }

    if let Some(user_profile) = env.user_profile.as_ref() {
        candidates.push((
            ShellKind::Pwsh,
            Path::new(user_profile)
                .join("scoop")
                .join("shims")
                .join("pwsh.exe"),
        ));
    }

    if let Some(program_files) = env.program_files.as_ref() {
        candidates.push((
            ShellKind::Bash,
            Path::new(program_files)
                .join("Git")
                .join("bin")
                .join("bash.exe"),
        ));
    }
    if let Some(local_app_data) = env.local_app_data.as_ref() {
        candidates.push((
            ShellKind::Bash,
            Path::new(local_app_data)
                .join("Programs")
                .join("Git")
                .join("bin")
                .join("bash.exe"),
        ));
    }

    candidates
}

fn path_named_files(path_var: Option<&OsStr>, file_name: &str) -> Vec<PathBuf> {
    let Some(path_var) = path_var else {
        return Vec::new();
    };
    std::env::split_paths(path_var)
        .filter(|entry| is_absolute_path_entry(entry))
        .map(|entry| entry.join(file_name))
        .collect()
}

fn is_absolute_path_entry(entry: &Path) -> bool {
    !entry.as_os_str().is_empty() && lexical_normalize(entry).is_absolute()
}

/// Finds `pwsh.exe` inside versioned `Microsoft.PowerShell_*` package
/// directories under a WindowsApps root, newest version first.
#[cfg(windows)]
#[must_use]
pub(crate) fn fuzzy_windows_apps_pwsh(windows_apps: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(windows_apps) else {
        return Vec::new();
    };
    let mut packages: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("Microsoft.PowerShell_"))
        })
        .collect();
    // Descending lexical order puts the highest version first for the
    // stable `Major.Minor.Patch.Build` naming scheme.
    packages.sort_unstable_by(|a, b| b.cmp(a));
    packages
        .into_iter()
        .map(|package| package.join("pwsh.exe"))
        .filter(|pwsh| pwsh.exists())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{DetectedShell, ShellKind, cmd_fallback_args, select_windows_shell};
    use std::path::{Path, PathBuf};

    #[test]
    fn cmd_is_a_runtime_fallback_not_a_settings_kind() {
        assert_eq!(ShellKind::parse("cmd"), None);
        assert_eq!(ShellKind::parse("pwsh"), Some(ShellKind::Pwsh));
        assert_eq!(ShellKind::parse("bash"), Some(ShellKind::Bash));
        assert_eq!(
            ShellKind::from_program(Path::new("cmd.exe")),
            ShellKind::Cmd
        );
        #[cfg(windows)]
        assert_eq!(
            ShellKind::from_program(Path::new(r"C:\Windows\System32\cmd.exe")),
            ShellKind::Cmd
        );
        let fallback = select_windows_shell(None, Some(PathBuf::from("cmd.exe"))).unwrap();
        assert_eq!(fallback.kind, ShellKind::Cmd);
        assert_eq!(
            cmd_fallback_args("echo hi"),
            vec![
                "/d".to_owned(),
                "/s".to_owned(),
                "/c".to_owned(),
                "echo hi".to_owned()
            ]
        );
        let preferred = DetectedShell {
            kind: ShellKind::Pwsh,
            program: PathBuf::from("pwsh"),
        };
        let kept = select_windows_shell(Some(preferred), Some(PathBuf::from("cmd.exe"))).unwrap();
        assert_eq!(kept.kind, ShellKind::Pwsh);
    }
}
