//! Automatic platform-shell discovery.
//!
//! The choice is fixed for the process. `MYCODE_SHELL` and `tools.shell` are
//! not overrides: callers that still see them log once and ignore them.
//!
//! Windows follows Codex and Gemini CLI: PowerShell 7 (`pwsh`), then Windows
//! PowerShell 5.1, then `cmd.exe`. Git Bash, MSYS2, Cygwin, and WSL are not
//! candidates. A `WindowsApps\pwsh.exe` execution alias is recognized by path
//! shape, not by file size, and is used only when no regular `pwsh` exists.
//!
//! Linux and macOS follow Codex's user-shell rule, limited to POSIX shells.
//! `$SHELL` wins when it is an absolute `bash`, `zsh`, or `sh`. Otherwise
//! macOS tries `zsh`, then `bash`, then `sh`; other Unix tries `bash`, then
//! `zsh`, then `sh`. `pwsh` and `cmd` are never selected there.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::builtin::fs_search::lexical_normalize;

/// Kind of platform shell used by the shell tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellKind {
    /// PowerShell 7+ (`pwsh`).
    Pwsh,
    /// Windows PowerShell 5.1 (`powershell.exe`).
    WindowsPowerShell,
    /// Bash.
    Bash,
    /// Zsh.
    Zsh,
    /// POSIX `sh`.
    Sh,
    /// Windows `cmd.exe`. Runtime fallback only. Not a settings value.
    Cmd,
}

impl ShellKind {
    /// Stable name for logs and tests.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pwsh => "pwsh",
            Self::WindowsPowerShell => "powershell",
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Sh => "sh",
            Self::Cmd => "cmd",
        }
    }

    /// Model-facing tool name. PowerShell editions share one tool.
    #[must_use]
    pub fn tool_name(self) -> &'static str {
        match self {
            Self::Pwsh | Self::WindowsPowerShell => "powershell",
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Sh => "sh",
            Self::Cmd => "cmd",
        }
    }

    /// Whether script mode launches PowerShell (`pwsh` or `powershell.exe`).
    #[must_use]
    pub fn is_powershell(self) -> bool {
        matches!(self, Self::Pwsh | Self::WindowsPowerShell)
    }

    /// Parses a shell family name. `cmd` is intentionally rejected.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pwsh" => Some(Self::Pwsh),
            "powershell" => Some(Self::WindowsPowerShell),
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "sh" => Some(Self::Sh),
            _ => None,
        }
    }

    /// Classifies a program path from its file stem.
    ///
    /// Unknown stems use the host default family so a forced test double
    /// still has a launch style. Discovery never uses that default: POSIX
    /// selection only accepts `bash`, `zsh`, and `sh`.
    #[must_use]
    pub fn from_program(path: &Path) -> Self {
        match file_stem(path).as_str() {
            "pwsh" => Self::Pwsh,
            "powershell" => Self::WindowsPowerShell,
            "bash" => Self::Bash,
            "zsh" => Self::Zsh,
            "sh" => Self::Sh,
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

/// `bash`, `zsh`, or `sh`. Anything else, including `pwsh` and `fish`, is
/// not a POSIX shell for this tool.
fn posix_kind(path: &Path) -> Option<ShellKind> {
    match file_stem(path).as_str() {
        "bash" => Some(ShellKind::Bash),
        "zsh" => Some(ShellKind::Zsh),
        "sh" => Some(ShellKind::Sh),
        _ => None,
    }
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
    /// A regular executable image.
    Regular,
    /// `WindowsApps\pwsh.exe`, the Store execution alias, not a package binary.
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
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn is_wsl_bash_launcher(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('/', "\\");
    let lower = text.to_ascii_lowercase();
    lower.ends_with("\\system32\\bash.exe") || lower.ends_with("\\syswow64\\bash.exe")
}

/// `true` when `path` is `WindowsApps\<name>.exe` itself, not a package folder
/// under `WindowsApps`.
///
/// Store execution aliases live at that exact path. Real PowerShell 7 packages
/// live in `WindowsApps\Microsoft.PowerShell_*\pwsh.exe`. File size is not
/// part of the check: an alias can be larger than a few dozen bytes, and a
/// tiny file outside `WindowsApps` is not an alias.
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn is_windowsapps_execution_alias(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('/', "\\");
    let mut parts = text.rsplit('\\');
    let file = parts.next().unwrap_or("");
    let parent = parts.next().unwrap_or("");
    parent.eq_ignore_ascii_case("WindowsApps")
        && (file.eq_ignore_ascii_case("pwsh.exe") || file.eq_ignore_ascii_case("bash.exe"))
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

struct CachedShell {
    detected: Option<DetectedShell>,
    resolved: DetectedShell,
}

static CACHED_SHELL: OnceLock<CachedShell> = OnceLock::new();

fn cached_shell() -> &'static CachedShell {
    note_ignored_mycode_shell();
    CACHED_SHELL.get_or_init(|| {
        let detected = detect_default_shell();
        let resolved = detected.clone().unwrap_or_else(fallback_shell);
        CachedShell { detected, resolved }
    })
}

/// Shell discovered on this host, without the Windows `cmd.exe` fallback.
///
/// The value is computed once per process so the tool spec and later launches
/// stay on the same interpreter.
#[must_use]
pub fn active_shell() -> Option<DetectedShell> {
    cached_shell().detected.clone()
}

/// Shell the tool and the system prompt agree on for this process.
///
/// On Windows, when PowerShell is missing, this is `cmd.exe` and the model
/// sees the `cmd` tool.
#[must_use]
pub fn resolved_shell() -> DetectedShell {
    cached_shell().resolved.clone()
}

fn note_ignored_mycode_shell() {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    let Ok(value) = std::env::var("MYCODE_SHELL") else {
        return;
    };
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
        return;
    }
    if LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "mycode: ignoring MYCODE_SHELL ({trimmed}); the shell is chosen automatically for this operating system"
    );
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

#[cfg(not(windows))]
fn detect_posix_shell() -> Option<DetectedShell> {
    choose_posix_shell(
        std::env::var_os("SHELL").as_deref(),
        std::env::var_os("PATH").as_deref(),
        cfg!(target_os = "macos"),
        |program| program.is_file(),
    )
}

/// POSIX search order. Existence is not required.
///
/// `$SHELL` is first when it is an absolute `bash`, `zsh`, or `sh`. Relative
/// values and other families (`pwsh`, `fish`, `nu`) are skipped.
#[cfg(any(not(windows), test))]
pub(crate) fn posix_shell_candidates(
    user_shell: Option<&OsStr>,
    path_var: Option<&OsStr>,
    macos: bool,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(value) = user_shell {
        let path = PathBuf::from(value);
        if is_absolute_path_entry(&path) && posix_kind(&path).is_some() {
            candidates.push(path);
        }
    }
    if macos {
        candidates.push(PathBuf::from("/bin/zsh"));
        candidates.push(PathBuf::from("/bin/bash"));
        candidates.extend(path_named_files(path_var, "zsh"));
        candidates.extend(path_named_files(path_var, "bash"));
    } else {
        candidates.push(PathBuf::from("/bin/bash"));
        candidates.push(PathBuf::from("/usr/bin/bash"));
        candidates.extend(path_named_files(path_var, "bash"));
        candidates.extend(path_named_files(path_var, "zsh"));
        candidates.push(PathBuf::from("/bin/zsh"));
    }
    candidates.push(PathBuf::from("/bin/sh"));
    candidates.extend(path_named_files(path_var, "sh"));
    candidates
}

/// First existing POSIX candidate.
#[cfg(any(not(windows), test))]
pub(crate) fn choose_posix_shell(
    user_shell: Option<&OsStr>,
    path_var: Option<&OsStr>,
    macos: bool,
    exists: impl Fn(&Path) -> bool,
) -> Option<DetectedShell> {
    posix_shell_candidates(user_shell, path_var, macos)
        .into_iter()
        .find(|program| exists(program))
        .map(|program| DetectedShell {
            kind: posix_kind(&program).unwrap_or(ShellKind::Bash),
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
/// The order is PowerShell 7, then Windows PowerShell 5.1. Bash is not a
/// Windows candidate. `cmd.exe` is not listed here; [`resolved_shell`] adds
/// it when both tiers miss.
#[cfg(any(windows, test))]
pub(crate) fn windows_shell_tiers(env: &WindowsShellEnv) -> [Vec<(ShellKind, PathBuf)>; 2] {
    [pwsh_candidates(env), powershell51_candidates(env)]
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

#[cfg(windows)]
fn image_of(path: &Path) -> ShellImage {
    if is_windowsapps_execution_alias(path) {
        return if alias_file_present(path) {
            ShellImage::PwshStoreAlias
        } else {
            ShellImage::Missing
        };
    }
    if image_is_regular_executable(path) {
        ShellImage::Regular
    } else {
        ShellImage::Missing
    }
}

/// A non-empty file. Store aliases are excluded by path before this runs.
#[cfg(windows)]
fn image_is_regular_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

/// The alias path exists and is not a directory. Size is irrelevant.
#[cfg(windows)]
fn alias_file_present(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.is_dir())
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
        DetectedShell, ShellImage, ShellKind, WindowsShellEnv, choose_posix_shell,
        cmd_fallback_args, is_syswow64_powershell, is_windowsapps_execution_alias,
        is_wsl_bash_launcher, posix_shell_candidates, resolved_shell, select_shell_tiers,
        windows_shell_tiers,
    };
    use std::ffi::{OsStr, OsString};
    use std::path::{Path, PathBuf};

    #[test]
    fn shell_names_cover_posix_families_and_not_cmd() {
        assert_eq!(ShellKind::parse("pwsh"), Some(ShellKind::Pwsh));
        assert_eq!(
            ShellKind::parse("powershell"),
            Some(ShellKind::WindowsPowerShell)
        );
        assert_eq!(ShellKind::parse("bash"), Some(ShellKind::Bash));
        assert_eq!(ShellKind::parse("zsh"), Some(ShellKind::Zsh));
        assert_eq!(ShellKind::parse("sh"), Some(ShellKind::Sh));
        assert_eq!(ShellKind::parse("cmd"), None);
        assert_eq!(ShellKind::Zsh.tool_name(), "zsh");
        assert_eq!(ShellKind::Sh.tool_name(), "sh");
        assert_eq!(ShellKind::Pwsh.tool_name(), "powershell");
        assert_eq!(ShellKind::WindowsPowerShell.tool_name(), "powershell");
        assert_eq!(ShellKind::Bash.tool_name(), "bash");
        assert_eq!(ShellKind::Cmd.tool_name(), "cmd");
        assert!(ShellKind::Pwsh.is_powershell());
        assert!(!ShellKind::Zsh.is_powershell());
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
            ShellKind::from_program(Path::new("/bin/zsh")),
            ShellKind::Zsh
        );
        assert_eq!(ShellKind::from_program(Path::new("/bin/sh")), ShellKind::Sh);
        assert_eq!(
            ShellKind::from_program(Path::new(r"C:\Windows\System32\cmd.exe")),
            ShellKind::Cmd
        );
    }

    #[test]
    fn posix_shell_prefers_an_absolute_user_shell_then_the_platform_default() {
        let zsh = Path::new("/bin/zsh");
        let bash = Path::new("/bin/bash");
        let sh = Path::new("/bin/sh");
        let have = |path: &Path| path == zsh || path == bash || path == sh;

        let picked = choose_posix_shell(Some(OsStr::new("/bin/zsh")), None, false, have).unwrap();
        assert_eq!(picked.kind, ShellKind::Zsh);
        assert_eq!(picked.program, zsh);

        let picked =
            choose_posix_shell(Some(OsStr::new("/usr/bin/fish")), None, false, have).unwrap();
        assert_eq!(picked.kind, ShellKind::Bash, "fish is not the tool shell");

        let picked =
            choose_posix_shell(Some(OsStr::new("/usr/bin/pwsh")), None, false, have).unwrap();
        assert_eq!(picked.kind, ShellKind::Bash, "pwsh is not a POSIX shell");
        assert_ne!(picked.program, PathBuf::from("/usr/bin/pwsh"));

        let macos = choose_posix_shell(None, None, true, have).unwrap();
        assert_eq!(macos.program, zsh);

        let linux =
            choose_posix_shell(None, None, false, |path| path == bash || path == sh).unwrap();
        assert_eq!(linux.program, bash);

        let only_sh = choose_posix_shell(None, None, false, |path| path == sh).unwrap();
        assert_eq!(only_sh.kind, ShellKind::Sh);

        assert!(choose_posix_shell(None, None, false, |_| false).is_none());
    }

    #[test]
    fn relative_user_shell_is_not_searched() {
        let candidates = posix_shell_candidates(Some(OsStr::new("bash")), None, false);
        assert!(
            candidates.iter().all(|path| path != Path::new("bash")),
            "{candidates:?}"
        );
    }

    #[test]
    fn tiers_prefer_regular_pwsh_then_store_alias_then_windows_powershell() {
        let pwsh = PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe");
        let alias = PathBuf::from(r"C:\Program Files\WindowsApps\pwsh.exe");
        let powershell =
            PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
        let tiers = [
            vec![
                (ShellKind::Pwsh, pwsh.clone()),
                (ShellKind::Pwsh, alias.clone()),
            ],
            vec![(ShellKind::WindowsPowerShell, powershell.clone())],
        ];
        let regular = |path: &Path| {
            if path == alias.as_path() {
                ShellImage::PwshStoreAlias
            } else {
                ShellImage::Regular
            }
        };
        let picked = select_shell_tiers(&tiers, regular).unwrap();
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
        assert!(select_shell_tiers(&tiers, |_| ShellImage::Missing).is_none());
    }

    #[test]
    fn windows_candidates_skip_bash_wsl_syswow64_and_classify_store_aliases_by_path() {
        let env = WindowsShellEnv {
            path: Some(OsString::from(
                r"C:\msys64\usr\bin;C:\Windows\System32;C:\Windows\SysWOW64\WindowsPowerShell\v1.0;C:\Program Files\Git\bin",
            )),
            program_files: Some(OsString::from(r"C:\Program Files")),
            program_files_x86: None,
            local_app_data: Some(OsString::from(r"C:\Users\me\AppData\Local")),
            user_profile: None,
            system_root: Some(OsString::from(r"C:\Windows")),
        };
        let tiers = windows_shell_tiers(&env);
        let programs: Vec<_> = tiers
            .iter()
            .flat_map(|tier| tier.iter().map(|(_, path)| path.clone()))
            .collect();
        assert!(
            programs.iter().all(|path| !file_ends_with_bash(path)),
            "bash is not a Windows shell: {programs:?}"
        );
        assert!(
            programs.iter().any(|path| path.ends_with(
                Path::new("WindowsPowerShell")
                    .join("v1.0")
                    .join("powershell.exe")
            )),
            "5.1 candidate missing: {programs:?}"
        );
        assert!(
            programs
                .iter()
                .all(|path| !is_syswow64_powershell(path) && !is_wsl_bash_launcher(path)),
            "{programs:?}"
        );
        assert!(is_windowsapps_execution_alias(Path::new(
            r"C:\Program Files\WindowsApps\pwsh.exe"
        )));
        assert!(is_windowsapps_execution_alias(Path::new(
            r"C:\Users\me\AppData\Local\Microsoft\WindowsApps\bash.exe"
        )));
        assert!(
            !is_windowsapps_execution_alias(Path::new(
                r"C:\Program Files\WindowsApps\Microsoft.PowerShell_8wekyb3d8bbwe\pwsh.exe"
            )),
            "a package binary is not the execution alias"
        );
        assert!(!is_windowsapps_execution_alias(Path::new(
            r"C:\msys64\usr\bin\bash.exe"
        )));
    }

    fn file_ends_with_bash(path: &Path) -> bool {
        path.to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase()
            .ends_with("\\bash.exe")
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("bash"))
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

    #[test]
    fn resolved_shell_stays_on_one_interpreter() {
        let first = resolved_shell();
        let second = resolved_shell();
        assert_eq!(first, second);
        #[cfg(windows)]
        assert!(matches!(
            first.kind,
            ShellKind::Pwsh | ShellKind::WindowsPowerShell | ShellKind::Cmd
        ));
        #[cfg(not(windows))]
        assert!(matches!(
            first.kind,
            ShellKind::Bash | ShellKind::Zsh | ShellKind::Sh
        ));
    }
}
