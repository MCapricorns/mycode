//! Default platform-shell discovery and the process-wide runtime preference.
//!
//! Windows prefers PowerShell 7 (`pwsh`), then Windows PowerShell 5.1
//! (`powershell.exe`), then Git Bash. `cmd.exe` is only the runtime fallback
//! when none of those exist, and it is not a settings kind. POSIX hosts use
//! bash, then `sh`. WSL's `System32\bash.exe` is not a candidate: it is a
//! Linux environment, and the session cwd is a Windows path.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::builtin::fs_search::lexical_normalize;

static RUNTIME_SHELL: RwLock<Option<DetectedShell>> = RwLock::new(None);

/// Environment variable that forces the shell for this process.
///
/// Values: `pwsh`, `powershell`, `bash`, `cmd`, `auto` (or empty) to keep
/// detection, or a path to an executable. This overrides saved settings and
/// is not written back to them.
pub const MYCODE_SHELL_ENV: &str = "MYCODE_SHELL";

/// Kind of platform shell used by the shell tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellKind {
    /// PowerShell 7+ (`pwsh`).
    Pwsh,
    /// Windows PowerShell 5.1 (`powershell.exe`).
    WindowsPowerShell,
    /// Bash (`bash` / `sh`), including Git Bash on Windows.
    Bash,
    /// Windows `cmd.exe`. Runtime fallback and `MYCODE_SHELL=cmd` only.
    /// [`Self::parse`] does not accept it, so settings cannot persist it.
    Cmd,
}

impl ShellKind {
    /// Stable settings / wire name for this kind.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pwsh => "pwsh",
            Self::WindowsPowerShell => "powershell",
            Self::Bash => "bash",
            Self::Cmd => "cmd",
        }
    }

    /// Model-facing tool name. PowerShell editions share one tool.
    #[must_use]
    pub fn tool_name(self) -> &'static str {
        match self {
            Self::Pwsh | Self::WindowsPowerShell => "powershell",
            Self::Bash => "bash",
            Self::Cmd => "cmd",
        }
    }

    /// Whether script mode launches PowerShell (`pwsh` or `powershell.exe`).
    #[must_use]
    pub fn is_powershell(self) -> bool {
        matches!(self, Self::Pwsh | Self::WindowsPowerShell)
    }

    /// Parses a settings / wire name. `cmd` is intentionally rejected.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pwsh" => Some(Self::Pwsh),
            "powershell" => Some(Self::WindowsPowerShell),
            "bash" => Some(Self::Bash),
            _ => None,
        }
    }

    /// Classifies a program path from its file stem.
    #[must_use]
    pub fn from_program(path: &Path) -> Self {
        match file_stem(path).as_str() {
            "pwsh" => Self::Pwsh,
            "powershell" => Self::WindowsPowerShell,
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

    fn fallback_executable(self) -> &'static str {
        match self {
            Self::Pwsh => "pwsh",
            Self::WindowsPowerShell => "powershell.exe",
            Self::Bash => "bash",
            Self::Cmd => "cmd.exe",
        }
    }
}

/// File stem, treating both `/` and `\` as separators so a Windows path
/// classifies the same way when the check runs on a POSIX host.
fn file_stem(path: &Path) -> String {
    let text = path.to_string_lossy();
    let name = text.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = name
        .rsplit_once('.')
        .filter(|(_, ext)| ext.eq_ignore_ascii_case("exe"))
        .map(|(stem, _)| stem)
        .unwrap_or(name);
    stem.to_ascii_lowercase()
}

/// How an explicit shell override should be resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShellOverride {
    /// Unset, empty, or `auto`: use settings, then detection.
    Default,
    /// A kind name. The program comes from detection for that kind.
    Kind(ShellKind),
    /// An executable path, classified from its file name.
    Program(PathBuf),
}

/// Classifies `MYCODE_SHELL` text. Does not touch the filesystem.
#[must_use]
pub fn classify_shell_override(value: &str) -> ShellOverride {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("auto") {
        return ShellOverride::Default;
    }
    let token = value.strip_suffix(".exe").unwrap_or(value);
    if !value.contains(['/', '\\']) && Path::new(token).components().count() == 1 {
        if let Some(kind) = ShellKind::parse(token) {
            return ShellOverride::Kind(kind);
        }
        if token.eq_ignore_ascii_case("cmd") {
            return ShellOverride::Kind(ShellKind::Cmd);
        }
    }
    ShellOverride::Program(PathBuf::from(value))
}

/// A resolved shell program and its kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetectedShell {
    /// Interpreter family used to build launch arguments and the tool spec.
    pub kind: ShellKind,
    /// Absolute or PATH-resolved program path.
    pub program: PathBuf,
}

/// What a candidate path looks like to discovery.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShellImage {
    /// Not present, or not a usable file.
    Missing,
    /// A regular executable image (larger than a Store execution alias).
    Regular,
    /// A WindowsApps `pwsh.exe` execution alias.
    PwshStoreAlias,
}

/// Picks the first usable shell across ordered tiers.
///
/// Each tier is one family. A regular image in an earlier tier beats a
/// regular image later, and a PowerShell 7 Store alias beats Windows
/// PowerShell 5.1.
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn select_shell_tiers(
    tiers: &[Vec<(ShellKind, PathBuf)>],
    image: impl Fn(&Path) -> ShellImage,
) -> Option<DetectedShell> {
    for tier in tiers {
        if let Some((kind, program)) = tier
            .iter()
            .find(|(_, program)| image(program) == ShellImage::Regular)
        {
            return Some(DetectedShell {
                kind: *kind,
                program: program.clone(),
            });
        }
        if let Some((kind, program)) = tier.iter().find(|(kind, program)| {
            *kind == ShellKind::Pwsh && image(program) == ShellImage::PwshStoreAlias
        }) {
            return Some(DetectedShell {
                kind: *kind,
                program: program.clone(),
            });
        }
    }
    None
}

/// `true` when `path` is the WSL launcher `bash.exe` under System32 or SysWOW64.
///
/// Checked on the path text so the rule is the same on every host.
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn is_wsl_bash_launcher(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('/', "\\");
    let lower = text.to_ascii_lowercase();
    lower.ends_with("\\system32\\bash.exe") || lower.ends_with("\\syswow64\\bash.exe")
}

/// `true` when `path` is the 32-bit Windows PowerShell under SysWOW64.
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn is_syswow64_powershell(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('/', "\\");
    let lower = text.to_ascii_lowercase();
    lower.contains("\\syswow64\\") && lower.ends_with("\\powershell.exe")
}

/// First pinnable platform shell for the current host.
///
/// Does not return `cmd.exe`. Callers that need a launchable shell use
/// [`resolved_shell`], which adds that fallback on Windows.
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

/// Resolves one shell kind to a program, when one is installed.
#[must_use]
pub fn detect_shell_kind(kind: ShellKind) -> Option<DetectedShell> {
    #[cfg(windows)]
    {
        if kind == ShellKind::Cmd {
            return windows_system_cmd().map(|program| DetectedShell { kind, program });
        }
        let tiers = windows_shell_tiers(&WindowsShellEnv::from_process());
        let tier = match kind {
            ShellKind::Pwsh => &tiers[0],
            ShellKind::WindowsPowerShell => &tiers[1],
            ShellKind::Bash => &tiers[2],
            ShellKind::Cmd => unreachable!("cmd is handled above"),
        };
        select_shell_tiers(std::slice::from_ref(tier), image_of)
    }
    #[cfg(not(windows))]
    {
        if kind == ShellKind::Bash {
            detect_posix_shell()
        } else {
            None
        }
    }
}

/// Shell selected for this process: `MYCODE_SHELL`, then the runtime
/// preference, then detection. `None` when nothing is installed (the Windows
/// `cmd.exe` fallback is applied by [`resolved_shell`]).
#[must_use]
pub fn active_shell() -> Option<DetectedShell> {
    if let Ok(value) = std::env::var(MYCODE_SHELL_ENV)
        && let Some(forced) = shell_from_override(&classify_shell_override(&value))
    {
        return Some(forced);
    }
    runtime_shell().or_else(detect_default_shell)
}

/// Shell the tool and the system prompt agree on.
///
/// On Windows, when PowerShell and Git Bash are both missing, this is
/// `cmd.exe` and the model sees the `cmd` tool.
#[must_use]
pub fn resolved_shell() -> DetectedShell {
    active_shell().unwrap_or_else(fallback_shell)
}

fn shell_from_override(override_value: &ShellOverride) -> Option<DetectedShell> {
    match override_value {
        ShellOverride::Default => None,
        ShellOverride::Kind(kind) => Some(resolve_kind(*kind)),
        ShellOverride::Program(program) => Some(DetectedShell {
            kind: ShellKind::from_program(program),
            program: program.clone(),
        }),
    }
}

fn resolve_kind(kind: ShellKind) -> DetectedShell {
    detect_shell_kind(kind).unwrap_or(DetectedShell {
        kind,
        program: PathBuf::from(kind.fallback_executable()),
    })
}

fn fallback_shell() -> DetectedShell {
    #[cfg(windows)]
    {
        if let Some(program) = windows_system_cmd() {
            return DetectedShell {
                kind: ShellKind::Cmd,
                program,
            };
        }
        DetectedShell {
            kind: ShellKind::WindowsPowerShell,
            program: PathBuf::from("powershell.exe"),
        }
    }
    #[cfg(not(windows))]
    {
        DetectedShell {
            kind: ShellKind::Bash,
            program: PathBuf::from("/bin/bash"),
        }
    }
}

/// Replaces the process-wide shell preference used when `MYCODE_SHELL` is unset.
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

/// Arguments for a `cmd.exe /d /s /c` invocation.
#[must_use]
pub fn cmd_fallback_args(command: &str) -> Vec<String> {
    vec![
        "/d".to_owned(),
        "/s".to_owned(),
        "/c".to_owned(),
        command.to_owned(),
    ]
}

#[cfg(any(windows, test))]
#[derive(Debug, Default)]
pub(crate) struct WindowsShellEnv {
    path: Option<std::ffi::OsString>,
    program_files: Option<std::ffi::OsString>,
    program_files_x86: Option<std::ffi::OsString>,
    local_app_data: Option<std::ffi::OsString>,
    user_profile: Option<std::ffi::OsString>,
    system_root: Option<std::ffi::OsString>,
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
            system_root: std::env::var_os("SystemRoot"),
        }
    }
}

/// First pinnable candidate from an injected Windows environment.
#[cfg(windows)]
#[must_use]
pub(crate) fn detect_windows_shell_with(env: &WindowsShellEnv) -> Option<DetectedShell> {
    let tiers = windows_shell_tiers(env);
    select_shell_tiers(&tiers, image_of)
}

/// Candidate programs in discovery order, one vector per family.
///
/// Existence is not required. The order is PowerShell 7, Windows PowerShell
/// 5.1, then Git Bash. WSL `bash.exe` is omitted.
#[cfg(any(windows, test))]
pub(crate) fn windows_shell_tiers(env: &WindowsShellEnv) -> [Vec<(ShellKind, PathBuf)>; 3] {
    [
        pwsh_candidates(env),
        powershell51_candidates(env),
        git_bash_candidates(env),
    ]
}

#[cfg(any(windows, test))]
fn pwsh_candidates(env: &WindowsShellEnv) -> Vec<(ShellKind, PathBuf)> {
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
    candidates
}

#[cfg(any(windows, test))]
fn powershell51_candidates(env: &WindowsShellEnv) -> Vec<(ShellKind, PathBuf)> {
    let mut candidates = Vec::new();
    if let Some(root) = env.system_root.as_ref() {
        candidates.push((
            ShellKind::WindowsPowerShell,
            Path::new(root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe"),
        ));
    }
    candidates.extend(
        path_named_files(env.path.as_deref(), "powershell.exe")
            .into_iter()
            .filter(|program| !is_syswow64_powershell(program))
            .map(|program| (ShellKind::WindowsPowerShell, program)),
    );
    candidates
}

#[cfg(any(windows, test))]
fn git_bash_candidates(env: &WindowsShellEnv) -> Vec<(ShellKind, PathBuf)> {
    let mut candidates = Vec::new();
    if let Some(program_files) = env.program_files.as_ref() {
        let program_files = Path::new(program_files);
        candidates.push((
            ShellKind::Bash,
            program_files.join("Git").join("bin").join("bash.exe"),
        ));
        candidates.push((
            ShellKind::Bash,
            program_files
                .join("Git")
                .join("usr")
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
    candidates.extend(
        path_named_files(env.path.as_deref(), "bash.exe")
            .into_iter()
            .filter(|program| !is_wsl_bash_launcher(program))
            .map(|program| (ShellKind::Bash, program)),
    );
    candidates
}

#[cfg(windows)]
fn image_of(path: &Path) -> ShellImage {
    if image_is_regular_executable(path) {
        ShellImage::Regular
    } else if is_store_execution_alias(path) {
        ShellImage::PwshStoreAlias
    } else {
        ShellImage::Missing
    }
}

/// A regular file larger than a Store execution alias (at most 64 bytes).
#[cfg(windows)]
fn image_is_regular_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 64)
}

/// A Store execution alias `pwsh.exe` under `WindowsApps` (at most 64 bytes).
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

/// `%SystemRoot%\System32\cmd.exe` when that file exists.
#[cfg(windows)]
pub(crate) fn windows_system_cmd() -> Option<PathBuf> {
    let root = std::env::var_os("SystemRoot")?;
    let path = PathBuf::from(root).join("System32").join("cmd.exe");
    path.is_file().then_some(path)
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
#[cfg(any(windows, test))]
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
    packages.sort_unstable_by(|a, b| b.cmp(a));
    packages
        .into_iter()
        .map(|package| package.join("pwsh.exe"))
        .filter(|pwsh| pwsh.exists())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        DetectedShell, ShellImage, ShellKind, ShellOverride, WindowsShellEnv,
        classify_shell_override, cmd_fallback_args, is_syswow64_powershell, is_wsl_bash_launcher,
        select_shell_tiers, windows_shell_tiers,
    };
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    #[test]
    fn settings_kinds_cover_powershell_editions_and_not_cmd() {
        assert_eq!(ShellKind::parse("pwsh"), Some(ShellKind::Pwsh));
        assert_eq!(
            ShellKind::parse("powershell"),
            Some(ShellKind::WindowsPowerShell)
        );
        assert_eq!(ShellKind::parse("bash"), Some(ShellKind::Bash));
        assert_eq!(ShellKind::parse("cmd"), None);
        assert_eq!(ShellKind::Pwsh.as_str(), "pwsh");
        assert_eq!(ShellKind::WindowsPowerShell.as_str(), "powershell");
        assert_eq!(ShellKind::Pwsh.tool_name(), "powershell");
        assert_eq!(ShellKind::WindowsPowerShell.tool_name(), "powershell");
        assert_eq!(ShellKind::Bash.tool_name(), "bash");
        assert_eq!(ShellKind::Cmd.tool_name(), "cmd");
        assert!(ShellKind::Pwsh.is_powershell());
        assert!(ShellKind::WindowsPowerShell.is_powershell());
        assert!(!ShellKind::Bash.is_powershell());
    }

    #[test]
    fn program_stems_select_the_interpreter_family() {
        assert_eq!(
            ShellKind::from_program(Path::new("pwsh.exe")),
            ShellKind::Pwsh
        );
        assert_eq!(
            ShellKind::from_program(Path::new(
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
            )),
            ShellKind::WindowsPowerShell
        );
        assert_eq!(
            ShellKind::from_program(Path::new(r"C:\Program Files\Git\bin\bash.exe")),
            ShellKind::Bash
        );
        assert_eq!(
            ShellKind::from_program(Path::new(r"C:\Windows\System32\cmd.exe")),
            ShellKind::Cmd
        );
    }

    #[test]
    fn override_tokens_force_a_family_without_touching_the_filesystem() {
        assert_eq!(classify_shell_override("  "), ShellOverride::Default);
        assert_eq!(classify_shell_override("auto"), ShellOverride::Default);
        assert_eq!(
            classify_shell_override("pwsh"),
            ShellOverride::Kind(ShellKind::Pwsh)
        );
        assert_eq!(
            classify_shell_override("powershell.exe"),
            ShellOverride::Kind(ShellKind::WindowsPowerShell)
        );
        assert_eq!(
            classify_shell_override("bash"),
            ShellOverride::Kind(ShellKind::Bash)
        );
        assert_eq!(
            classify_shell_override("cmd"),
            ShellOverride::Kind(ShellKind::Cmd)
        );
        assert_eq!(
            classify_shell_override(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            ShellOverride::Program(PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe"))
        );
    }

    #[test]
    fn tiers_prefer_pwsh_then_windows_powershell_then_git_bash() {
        let pwsh = PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe");
        let alias = PathBuf::from(r"C:\Program Files\WindowsApps\pwsh.exe");
        let powershell =
            PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
        let bash = PathBuf::from(r"C:\Program Files\Git\bin\bash.exe");
        let tiers = [
            vec![
                (ShellKind::Pwsh, pwsh.clone()),
                (ShellKind::Pwsh, alias.clone()),
            ],
            vec![(ShellKind::WindowsPowerShell, powershell.clone())],
            vec![(ShellKind::Bash, bash.clone())],
        ];
        let regular = |path: &Path| {
            if path == alias.as_path() {
                ShellImage::PwshStoreAlias
            } else {
                ShellImage::Regular
            }
        };
        let picked = select_shell_tiers(&tiers, regular).unwrap();
        assert_eq!(picked.kind, ShellKind::Pwsh);
        assert_eq!(picked.program, pwsh);

        let alias_only = |path: &Path| {
            if path == alias.as_path() {
                ShellImage::PwshStoreAlias
            } else if path == pwsh.as_path() {
                ShellImage::Missing
            } else {
                ShellImage::Regular
            }
        };
        let picked = select_shell_tiers(&tiers, alias_only).unwrap();
        assert_eq!(
            picked,
            DetectedShell {
                kind: ShellKind::Pwsh,
                program: alias,
            }
        );

        let no_pwsh = |path: &Path| {
            if path == powershell.as_path() {
                ShellImage::Regular
            } else {
                ShellImage::Missing
            }
        };
        let picked = select_shell_tiers(&tiers, no_pwsh).unwrap();
        assert_eq!(picked.kind, ShellKind::WindowsPowerShell);

        let bash_only = |path: &Path| {
            if path == bash.as_path() {
                ShellImage::Regular
            } else {
                ShellImage::Missing
            }
        };
        let picked = select_shell_tiers(&tiers, bash_only).unwrap();
        assert_eq!(picked.kind, ShellKind::Bash);
        assert!(select_shell_tiers(&tiers, |_| ShellImage::Missing).is_none());
    }

    #[test]
    fn windows_candidate_list_skips_wsl_bash_and_includes_powershell_51() {
        let env = WindowsShellEnv {
            path: Some(OsString::from(
                r"C:\Windows\System32;C:\Program Files\Git\bin",
            )),
            program_files: Some(OsString::from(r"C:\Program Files")),
            program_files_x86: None,
            local_app_data: None,
            user_profile: None,
            system_root: Some(OsString::from(r"C:\Windows")),
        };
        let tiers = windows_shell_tiers(&env);
        let powershell = &tiers[1];
        assert!(
            powershell.iter().any(|(_, path)| {
                path.ends_with(
                    Path::new("WindowsPowerShell")
                        .join("v1.0")
                        .join("powershell.exe"),
                )
            }),
            "5.1 candidate missing: {powershell:?}"
        );
        let bash_paths: Vec<_> = tiers[2].iter().map(|(_, path)| path.clone()).collect();
        assert!(
            bash_paths
                .iter()
                .any(|path| path.ends_with(Path::new("Git").join("bin").join("bash.exe"))),
            "Git Bash candidate missing: {bash_paths:?}"
        );
        assert!(
            bash_paths.iter().all(|path| !is_wsl_bash_launcher(path)),
            "WSL bash launcher leaked into candidates: {bash_paths:?}"
        );
        assert!(is_wsl_bash_launcher(Path::new(
            r"C:\Windows\System32\bash.exe"
        )));
        assert!(is_wsl_bash_launcher(Path::new(
            r"C:\Windows\SysWOW64\bash.exe"
        )));
        assert!(!is_wsl_bash_launcher(Path::new(
            r"C:\Program Files\Git\bin\bash.exe"
        )));
        assert!(is_syswow64_powershell(Path::new(
            r"C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe"
        )));
        assert!(!is_syswow64_powershell(Path::new(
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
        )));
    }

    #[test]
    fn cmd_args_are_the_runtime_fallback_shape() {
        assert_eq!(
            cmd_fallback_args("echo hi"),
            vec![
                "/d".to_owned(),
                "/s".to_owned(),
                "/c".to_owned(),
                "echo hi".to_owned()
            ]
        );
    }
}
