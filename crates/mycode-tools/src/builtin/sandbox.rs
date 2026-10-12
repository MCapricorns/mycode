//! OS confinement for `shell` and `run_code`.
//!
//! `tools.osSandbox` defaults on. When the effective flag is on, Linux wraps
//! the child with bubblewrap and macOS wraps it with `sandbox-exec`
//! (seatbelt). Windows has no helper. The product path leaves the effective
//! flag off there so shell and `run_code` still run, and the settings page
//! says the sandbox is not supported yet. A missing helper, a filesystem-root
//! writable path, or a path that cannot be quoted fails the call. Forcing
//! this policy on Windows does the same. There is no unsandboxed fallback.
//!
//! This is best-effort write confinement, not a security boundary. The
//! filesystem is read-only except the session cwd, extra workspace roots, and
//! the temp directory (a read-only role does not get those write binds).
//! Reads, the network, and the PID namespace stay shared. The inner program
//! is executed by the helper from a path that was pinned immediately before
//! the helper starts; that is not `execveat` of the inner image. Same-user
//! processes, already-open descriptors, and a workspace that lives inside the
//! writable temp directory are outside this promise. Python `-I` and the
//! audit hook are a separate best-effort layer, not part of this helper.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use crate::ctx::ToolCtx;
use crate::tool::{ToolError, ToolResult};

const NO_FALLBACK: &str = "This setting does not fall back to unsandboxed execution.";

/// How much of the filesystem the helper may write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SandboxWrite {
    /// Session cwd, extra workspace roots, and the temp directory are writable.
    Workspace,
    /// No writable workspace bind. Temp stays read-only so a bootstrap file
    /// on another mount is still visible.
    ReadOnly,
}

/// One sandboxed launch. Absent means the child is started without the helper.
#[derive(Clone, Debug)]
pub(crate) struct OsSandboxPolicy {
    write: SandboxWrite,
    cwd: PathBuf,
    extra_writable: Vec<PathBuf>,
    extra_readable: Vec<PathBuf>,
}

impl OsSandboxPolicy {
    /// Workspace-write policy. `extra_readable` paths are mounted read-only
    /// unless a later writable bind names the same path.
    pub(crate) fn workspace(
        cwd: PathBuf,
        extra_writable: Vec<PathBuf>,
        extra_readable: Vec<PathBuf>,
    ) -> Self {
        Self {
            write: SandboxWrite::Workspace,
            cwd,
            extra_writable,
            extra_readable,
        }
    }

    /// Read-only policy. Extra roots are mounted read-only so a workspace on
    /// another mount is still visible.
    pub(crate) fn read_only(cwd: PathBuf, extra_readable: Vec<PathBuf>) -> Self {
        Self {
            write: SandboxWrite::ReadOnly,
            cwd,
            extra_writable: Vec::new(),
            extra_readable,
        }
    }

    /// `None` when this call's effective sandbox flag is off.
    ///
    /// A `true` flag still builds a policy on Windows. [`plan`] then fails
    /// closed because that platform has no helper.
    pub(crate) fn for_tool(ctx: &ToolCtx, write: SandboxWrite) -> Option<Self> {
        if !ctx.os_sandbox {
            return None;
        }
        Some(match write {
            SandboxWrite::Workspace => Self::workspace(
                ctx.cwd.clone(),
                ctx.extra_roots.clone(),
                ctx.read_roots.clone(),
            ),
            SandboxWrite::ReadOnly => {
                let mut readable = ctx.extra_roots.clone();
                readable.extend(ctx.read_roots.iter().cloned());
                Self::read_only(ctx.cwd.clone(), readable)
            }
        })
    }
}

/// Helper plus the argument vector that follows it. The vector does not
/// repeat the helper path; the caller uses that path as `argv0`.
#[derive(Debug)]
pub(crate) struct PlannedSandbox {
    pub(crate) helper_program: String,
    pub(crate) args: Vec<String>,
}

/// Prompt sentence inserted when the effective sandbox flag is on.
#[must_use]
pub fn os_sandbox_prompt_note() -> &'static str {
    "os_sandbox: on. shell and run_code are wrapped by a best-effort OS helper \
(bubblewrap on Linux, seatbelt on macOS). This is not a security boundary. \
Writes outside the session cwd, extra workspace folders, and the temp directory \
are denied; a read-only role's Python workspace is read-only at that layer too. \
Reads of the rest of the filesystem and the network stay available. Python -I \
and the audit hook remain best-effort and are not a sandbox."
}

/// Prompt sentence for the Windows product path.
///
/// The stored setting defaults on, but there is no helper. The note says the
/// sandbox is not supported yet so the model does not treat the call as
/// confined. Forcing the helper still fails the call.
#[must_use]
pub fn os_sandbox_unsupported_prompt_note() -> &'static str {
    "os_sandbox: not supported yet on Windows. There is no bubblewrap or \
seatbelt helper. shell and run_code run without that confinement so they \
still work. This is not an enabled sandbox with a silent unsandboxed \
fallback. Forcing the helper fails the call."
}

/// System-prompt sentence for the product path, if one should be added.
///
/// `effective` is the host policy after the Windows product path.
/// `platform_supported` is false on Windows.
#[must_use]
pub fn os_sandbox_status_note(effective: bool, platform_supported: bool) -> Option<&'static str> {
    if effective {
        Some(os_sandbox_prompt_note())
    } else if !platform_supported {
        Some(os_sandbox_unsupported_prompt_note())
    } else {
        None
    }
}

/// Records which helper wrapped a finished shell result.
///
/// The note is the claim: best-effort write confinement, not a boundary.
pub(crate) fn annotate(result: &mut ToolResult) {
    if let Some(details) = result.details.as_mut() {
        stamp_details(details);
    }
}

fn stamp_details(details: &mut Value) {
    details["os_sandbox"] = json!(helper_label());
    details["os_sandbox_note"] = json!("best-effort write confinement, not a security boundary");
}

/// Name of the helper this platform would use. Not proof that it ran.
#[must_use]
pub(crate) fn helper_label() -> &'static str {
    if cfg!(target_os = "linux") {
        "bubblewrap"
    } else if cfg!(target_os = "macos") {
        "seatbelt"
    } else {
        "unavailable"
    }
}

/// Whether the platform helper is an executable file right now.
#[must_use]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn helper_available() -> bool {
    resolve_helper().is_ok()
}

/// Resolves the helper and builds its argument vector.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] when the helper is missing, the platform
/// has none, or a bind path is the filesystem root or cannot be quoted.
pub(crate) fn plan(
    policy: &OsSandboxPolicy,
    user_program: &str,
    user_args: &[String],
) -> Result<PlannedSandbox, ToolError> {
    let helper = resolve_helper()?;
    let args = match helper.kind {
        HelperKind::Bubblewrap => bubblewrap_args(policy, user_program, user_args)?,
        #[cfg(target_os = "macos")]
        HelperKind::Seatbelt => seatbelt_args(policy, user_program, user_args)?,
    };
    Ok(PlannedSandbox {
        helper_program: helper.program,
        args,
    })
}

#[derive(Clone, Copy)]
enum HelperKind {
    Bubblewrap,
    #[cfg(target_os = "macos")]
    Seatbelt,
}

struct ResolvedHelper {
    program: String,
    kind: HelperKind,
}

#[cfg(any(test, not(any(target_os = "linux", target_os = "macos"))))]
fn unsupported_platform_error() -> ToolError {
    sandbox_error("this platform has no bubblewrap or seatbelt helper.")
}

fn resolve_helper() -> Result<ResolvedHelper, ToolError> {
    #[cfg(target_os = "linux")]
    {
        resolve_bubblewrap()
    }
    #[cfg(target_os = "macos")]
    {
        resolve_seatbelt()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(unsupported_platform_error())
    }
}

#[cfg(target_os = "linux")]
fn resolve_bubblewrap() -> Result<ResolvedHelper, ToolError> {
    let program = find_bubblewrap().ok_or_else(|| {
        sandbox_error(
            "bubblewrap (bwrap) was not found at /usr/bin/bwrap, /bin/bwrap, or an absolute PATH entry.",
        )
    })?;
    Ok(ResolvedHelper {
        program,
        kind: HelperKind::Bubblewrap,
    })
}

#[cfg(target_os = "macos")]
fn resolve_seatbelt() -> Result<ResolvedHelper, ToolError> {
    let path = Path::new("/usr/bin/sandbox-exec");
    if !is_executable(path) {
        return Err(sandbox_error(
            "/usr/bin/sandbox-exec is not an executable file.",
        ));
    }
    Ok(ResolvedHelper {
        program: "/usr/bin/sandbox-exec".to_owned(),
        kind: HelperKind::Seatbelt,
    })
}

#[cfg(target_os = "linux")]
fn find_bubblewrap() -> Option<String> {
    for candidate in ["/usr/bin/bwrap", "/bin/bwrap"] {
        let path = Path::new(candidate);
        if is_executable(path) {
            return Some(candidate.to_owned());
        }
    }
    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        if !entry.is_absolute() {
            continue;
        }
        let candidate = entry.join("bwrap");
        if is_executable(&candidate) {
            return candidate.to_str().map(str::to_owned);
        }
    }
    None
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Bubblewrap argument vector, not including the `bwrap` path itself.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] when a required directory is missing or a
/// writable bind would be the filesystem root.
pub(crate) fn bubblewrap_args(
    policy: &OsSandboxPolicy,
    user_program: &str,
    user_args: &[String],
) -> Result<Vec<String>, ToolError> {
    let binds = collect_binds(policy)?;
    let cwd = path_text(&policy.cwd)?;
    let mut args = vec![
        "--die-with-parent".to_owned(),
        "--ro-bind".to_owned(),
        "/".to_owned(),
        "/".to_owned(),
        "--dev".to_owned(),
        "/dev".to_owned(),
        "--proc".to_owned(),
        "/proc".to_owned(),
    ];
    for bind in binds {
        args.push(if bind.writable {
            "--bind".to_owned()
        } else {
            "--ro-bind".to_owned()
        });
        args.push(bind.path.clone());
        args.push(bind.path);
    }
    args.push("--chdir".to_owned());
    args.push(cwd);
    args.push("--".to_owned());
    args.push(user_program.to_owned());
    args.extend(user_args.iter().cloned());
    Ok(args)
}

/// Seatbelt argument vector for `/usr/bin/sandbox-exec`, not including that path.
///
/// `(allow default)` plus `(deny file-write*)` is write confinement only.
/// Reads, process execution, and the network stay allowed. That is the claim.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] for the same path failures as bubblewrap.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn seatbelt_args(
    policy: &OsSandboxPolicy,
    user_program: &str,
    user_args: &[String],
) -> Result<Vec<String>, ToolError> {
    let binds = collect_binds(policy)?;
    let mut profile = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    profile.push_str("(allow file-write* (literal \"/dev/null\"))\n");
    for bind in &binds {
        if bind.writable {
            profile.push_str("(allow file-write* (subpath ");
            profile.push_str(&quote_seatbelt(&bind.path)?);
            profile.push_str("))\n");
        }
    }
    let mut args = vec!["-p".to_owned(), profile, user_program.to_owned()];
    args.extend(user_args.iter().cloned());
    Ok(args)
}

struct Bind {
    writable: bool,
    path: String,
}

fn collect_binds(policy: &OsSandboxPolicy) -> Result<Vec<Bind>, ToolError> {
    let mut collector = BindCollector::default();
    for path in &policy.extra_readable {
        collector.push(path, false, false)?;
    }
    let writable = policy.write == SandboxWrite::Workspace;
    for path in temp_roots() {
        collector.push(&path, writable, false)?;
    }
    collector.push(&policy.cwd, writable, true)?;
    let extras = if writable {
        &policy.extra_writable
    } else {
        &policy.extra_readable
    };
    for path in extras {
        collector.push(path, writable, false)?;
    }
    let mut binds = collector.binds;
    binds.sort_by(|left, right| {
        left.path
            .len()
            .cmp(&right.path.len())
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(binds)
}

#[derive(Default)]
struct BindCollector {
    binds: Vec<Bind>,
    seen: BTreeSet<String>,
}

impl BindCollector {
    fn push(&mut self, path: &Path, writable: bool, required: bool) -> Result<(), ToolError> {
        let text = path_text(path)?;
        if is_fs_root(Path::new(&text)) {
            if writable {
                return Err(sandbox_error(
                    "a writable path is the filesystem root, so writes cannot be confined.",
                ));
            }
            return Ok(());
        }
        if !self.seen.insert(text.clone()) {
            if writable && let Some(existing) = self.binds.iter_mut().find(|bind| bind.path == text)
            {
                existing.writable = true;
            }
            return Ok(());
        }
        let is_dir = std::fs::metadata(path).is_ok_and(|meta| meta.is_dir());
        if !is_dir {
            self.seen.remove(&text);
            if required {
                return Err(sandbox_error(
                    "the working directory is missing or is not a directory.",
                ));
            }
            return Ok(());
        }
        self.binds.push(Bind {
            writable,
            path: text,
        });
        Ok(())
    }
}

fn temp_roots() -> Vec<PathBuf> {
    let mut roots = vec![std::env::temp_dir()];
    for key in ["TMPDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                roots.push(path);
            }
        }
    }
    roots
}

fn path_text(path: &Path) -> Result<String, ToolError> {
    let Some(text) = path.to_str() else {
        return Err(sandbox_error("a path is not Unicode."));
    };
    if text.is_empty() || text.contains(['"', '\\', '(', ')', '\n', '\r', '\0']) {
        return Err(sandbox_error(
            "a path cannot be quoted in the seatbelt profile.",
        ));
    }
    let trimmed = if text.len() > 1 {
        text.trim_end_matches(['/', '\\'])
    } else {
        text
    };
    if trimmed.is_empty() {
        return Err(sandbox_error("a path is empty."));
    }
    Ok(trimmed.to_owned())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn quote_seatbelt(path: &str) -> Result<String, ToolError> {
    if path.contains(['"', '\\', '(', ')', '\n', '\r']) {
        return Err(sandbox_error(
            "a path cannot be quoted in the seatbelt profile.",
        ));
    }
    Ok(format!("\"{path}\""))
}

fn is_fs_root(path: &Path) -> bool {
    let mut saw_root = false;
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => saw_root = true,
            Component::CurDir => {}
            _ => return false,
        }
    }
    saw_root
}

fn sandbox_error(detail: &str) -> ToolError {
    ToolError::Execution(format!("os sandbox: {detail} {NO_FALLBACK}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mycode-sandbox-{label}-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("scratch");
        path
    }

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn has_mount(args: &[String], flag: &str, path: &str) -> bool {
        args.windows(3)
            .any(|window| window[0] == flag && window[1] == path && window[2] == path)
    }

    #[cfg(unix)]
    #[test]
    fn bubblewrap_workspace_binds_cwd_and_temp_and_keeps_the_root_read_only() {
        let dir = Scratch(scratch("rw"));
        let cwd = dir.0.display().to_string();
        let policy = OsSandboxPolicy::workspace(dir.0.clone(), Vec::new(), Vec::new());
        let args = bubblewrap_args(&policy, "/bin/echo", &["hi".to_owned()]).expect("args");
        assert!(has_mount(&args, "--ro-bind", "/"), "{args:?}");
        assert!(has_mount(&args, "--bind", &cwd), "{args:?}");
        assert!(
            has_mount(&args, "--bind", &path_text(&std::env::temp_dir()).unwrap()),
            "{args:?}"
        );
        assert!(!has_mount(&args, "--bind", "/"), "{args:?}");
        assert!(args.iter().any(|arg| arg == "--die-with-parent"));
        assert!(!args.iter().any(|arg| arg.contains("unshare")));
        assert_eq!(
            args[args.len() - 3..],
            ["--".to_owned(), "/bin/echo".to_owned(), "hi".to_owned()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn bubblewrap_read_only_does_not_bind_the_workspace_writable() {
        let dir = Scratch(scratch("ro"));
        let cwd = dir.0.display().to_string();
        let policy = OsSandboxPolicy::read_only(dir.0.clone(), Vec::new());
        let args = bubblewrap_args(&policy, "/usr/bin/python3", &["-I".to_owned()]).expect("args");
        assert!(has_mount(&args, "--ro-bind", &cwd), "{args:?}");
        assert!(
            !args.iter().any(|arg| arg == "--bind"),
            "read-only policy used a writable bind: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|window| window == ["--", "/usr/bin/python3"])
        );
    }

    #[test]
    fn a_writable_filesystem_root_is_rejected() {
        let policy = OsSandboxPolicy::workspace(PathBuf::from("/"), Vec::new(), Vec::new());
        let error = bubblewrap_args(&policy, "/bin/true", &[]).expect_err("root");
        let text = error.to_string();
        assert!(text.contains("filesystem root"), "{text}");
        assert!(text.contains("does not fall back"), "{text}");
    }

    #[test]
    fn a_quoted_metacharacter_is_rejected() {
        let policy = OsSandboxPolicy::workspace(PathBuf::from("/tmp/a\"b"), Vec::new(), Vec::new());
        let error = bubblewrap_args(&policy, "/bin/true", &[]).expect_err("quote");
        assert!(error.to_string().contains("does not fall back"));
    }

    #[cfg(unix)]
    #[test]
    fn seatbelt_profile_denies_writes_except_the_workspace() {
        let dir = Scratch(scratch("seat"));
        let cwd = path_text(&dir.0).unwrap();
        let policy = OsSandboxPolicy::workspace(dir.0.clone(), Vec::new(), Vec::new());
        let args = seatbelt_args(&policy, "/bin/echo", &["hi".to_owned()]).expect("profile");
        assert_eq!(args[0], "-p");
        let profile = &args[1];
        assert!(profile.contains("(allow default)"), "{profile}");
        assert!(profile.contains("(deny file-write*)"), "{profile}");
        assert!(
            profile.contains(&format!("(subpath \"{cwd}\")")),
            "{profile}"
        );
        assert!(!profile.contains("(subpath \"/\")"), "{profile}");
        assert_eq!(args[2], "/bin/echo");
        assert_eq!(args[3], "hi");
    }

    #[cfg(unix)]
    #[test]
    fn seatbelt_read_only_has_no_workspace_write_rule() {
        let dir = Scratch(scratch("seat-ro"));
        let cwd = path_text(&dir.0).unwrap();
        let policy = OsSandboxPolicy::read_only(dir.0.clone(), Vec::new());
        let args = seatbelt_args(&policy, "/bin/echo", &[]).expect("profile");
        let profile = &args[1];
        assert!(profile.contains("(deny file-write*)"), "{profile}");
        assert!(
            !profile.contains(&format!("subpath \"{cwd}\"")),
            "{profile}"
        );
    }

    #[test]
    fn the_prompt_note_does_not_call_the_helper_a_boundary() {
        let note = os_sandbox_prompt_note();
        assert!(note.contains("os_sandbox: on"));
        assert!(note.contains("not a security boundary"));
        assert!(note.contains("best-effort"));
        assert!(note.contains("audit hook"));
    }

    #[test]
    fn the_windows_note_says_not_supported_yet_and_does_not_claim_the_helper_is_on() {
        let note = os_sandbox_unsupported_prompt_note();
        assert!(note.contains("not supported yet"));
        assert!(!note.contains("os_sandbox: on"));
        assert!(note.contains("fails the call"));
        assert!(note.contains("silent unsandboxed"));
        assert_eq!(
            os_sandbox_status_note(true, true),
            Some(os_sandbox_prompt_note())
        );
        assert_eq!(
            os_sandbox_status_note(false, false).map(|text| text.contains("not supported yet")),
            Some(true)
        );
        assert!(os_sandbox_status_note(false, true).is_none());
        let forced = os_sandbox_status_note(true, false).expect("forced note");
        assert!(forced.contains("os_sandbox: on"));
        assert!(!forced.contains("not supported yet"));
    }

    #[test]
    fn a_disabled_sandbox_does_not_build_a_policy() {
        let ctx = ToolCtx::new(std::env::temp_dir());
        assert!(!ctx.os_sandbox);
        assert!(OsSandboxPolicy::for_tool(&ctx, SandboxWrite::Workspace).is_none());
    }

    #[test]
    fn a_forced_sandbox_flag_still_builds_a_policy() {
        let ctx = ToolCtx::new(std::env::temp_dir()).with_os_sandbox(true);
        assert!(OsSandboxPolicy::for_tool(&ctx, SandboxWrite::Workspace).is_some());
    }

    #[test]
    fn forcing_the_helper_on_an_unsupported_platform_fails_closed() {
        let text = unsupported_platform_error().to_string();
        assert!(text.contains("no bubblewrap or seatbelt"));
        assert!(text.contains("does not fall back"));
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn plan_on_this_platform_fails_closed() {
        let policy = OsSandboxPolicy::workspace(PathBuf::from("."), Vec::new(), Vec::new());
        let error = plan(&policy, "cmd.exe", &[]).expect_err("no helper");
        let text = error.to_string();
        assert!(text.contains("does not fall back"));
        assert!(text.contains("no bubblewrap or seatbelt"));
    }

    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    struct ProbeDirs {
        workspace: PathBuf,
        outside: PathBuf,
    }

    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    impl Drop for ProbeDirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.workspace);
            let _ = std::fs::remove_dir_all(&self.outside);
        }
    }

    /// Workspace and the outside path sit on different directories so the
    /// writable temp bind cannot hide an escape into `/var/tmp`.
    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    fn probe_dirs(label: &str) -> ProbeDirs {
        let stamp = format!(
            "mycode-{label}-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        );
        let workspace = PathBuf::from("/dev/shm").join(&stamp);
        let outside = PathBuf::from("/var/tmp").join(&stamp);
        std::fs::create_dir_all(&workspace).expect("workspace probe dir");
        std::fs::create_dir_all(&outside).expect("outside probe dir");
        ProbeDirs { workspace, outside }
    }

    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    fn bash_quote(path: &Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
    }

    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    fn bash_tool() -> crate::builtin::shell::ShellTool {
        crate::builtin::shell::ShellTool::forcing(crate::builtin::shell::DetectedShell {
            kind: crate::builtin::shell::ShellKind::Bash,
            program: PathBuf::from("/bin/bash"),
        })
    }

    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    fn result_text(result: &ToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|block| match block {
                mycode_core::message::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Without the setting, shell can write outside the workspace. That is the
    /// residual this probe keeps visible.
    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    #[tokio::test]
    async fn shell_without_os_sandbox_can_write_outside_the_workspace() {
        let dirs = probe_dirs("unsandboxed");
        let inside = dirs.workspace.join("inside.txt");
        let outside = dirs.outside.join("nope.txt");
        let command = format!(
            "echo in > {} && echo out > {} && echo OUTSIDE_WRITE_OK",
            bash_quote(&inside),
            bash_quote(&outside)
        );
        let result = crate::tool::Tool::execute(
            &bash_tool(),
            crate::builtin::shell::ShellArgs {
                mode: crate::builtin::shell::ShellMode::Script,
                command: Some(command),
                program: None,
                args: Vec::new(),
                timeout_secs: Some(20),
            },
            &ToolCtx::new(&dirs.workspace),
            &mut crate::stream::ToolStream::closed(),
        )
        .await
        .expect("unsandboxed shell");
        let text = result_text(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("OUTSIDE_WRITE_OK"), "{text}");
        assert_eq!(std::fs::read_to_string(&inside).unwrap_or_default(), "in\n");
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap_or_default(),
            "out\n",
            "unsandboxed shell should be able to write outside the workspace"
        );
        assert!(
            result
                .details
                .as_ref()
                .is_none_or(|details| details.get("os_sandbox").is_none())
        );
    }

    /// With the setting, the same outside write is denied, or the helper's
    /// absence fails the call before the command runs.
    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    #[tokio::test]
    async fn opt_in_os_sandbox_denies_shell_writes_outside_or_fails_closed() {
        let dirs = probe_dirs("sandboxed");
        let inside = dirs.workspace.join("inside.txt");
        let outside = dirs.outside.join("nope.txt");
        let command = format!(
            "echo in > {} || exit 2; if echo out > {}; then echo OUTSIDE_WRITE_OK; else echo OUTSIDE_WRITE_DENIED; fi",
            bash_quote(&inside),
            bash_quote(&outside)
        );
        let ctx = ToolCtx::new(&dirs.workspace).with_os_sandbox(true);
        let result = crate::tool::Tool::execute(
            &bash_tool(),
            crate::builtin::shell::ShellArgs {
                mode: crate::builtin::shell::ShellMode::Script,
                command: Some(command),
                program: None,
                args: Vec::new(),
                timeout_secs: Some(20),
            },
            &ctx,
            &mut crate::stream::ToolStream::closed(),
        )
        .await;
        if !helper_available() {
            let error = result.expect_err("missing helper");
            let text = error.to_string();
            assert!(text.contains("does not fall back"), "{text}");
            assert!(!inside.exists());
            assert!(!outside.exists());
            return;
        }
        let result = result.expect("sandboxed shell");
        let text = result_text(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("OUTSIDE_WRITE_DENIED"), "{text}");
        assert!(!text.contains("OUTSIDE_WRITE_OK"), "{text}");
        assert_eq!(std::fs::read_to_string(&inside).unwrap_or_default(), "in\n");
        assert!(
            !outside.exists(),
            "os sandbox let a write escape to {}",
            outside.display()
        );
        let details = result.details.expect("details");
        assert_eq!(details["os_sandbox"], "bubblewrap");
        assert!(
            details["os_sandbox_note"]
                .as_str()
                .unwrap_or("")
                .contains("not a security boundary"),
            "{details}"
        );
    }

    /// The sandbox flag has to survive `run_code`'s inner preflight. Otherwise
    /// a scout or main program can call `tools.shell` unsandboxed.
    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    #[tokio::test]
    async fn opt_in_os_sandbox_applies_to_shell_inside_run_code() {
        let dirs = probe_dirs("inner-shell");
        let inside = dirs.workspace.join("inside.txt");
        let outside = dirs.outside.join("nope.txt");
        let command = format!(
            "echo in > {} || exit 2; if echo out > {}; then echo OUTSIDE_WRITE_OK; else echo OUTSIDE_WRITE_DENIED; fi",
            bash_quote(&inside),
            bash_quote(&outside)
        );
        let registry = crate::registry::ToolRegistry::new();
        registry.register(std::sync::Arc::new(bash_tool()));
        let ctx = ToolCtx::new(&dirs.workspace)
            .with_catalog(crate::registry::ToolCatalog::from_registry(&registry))
            .with_os_sandbox(true);
        let code = format!(
            "text = await tools.shell(command={})\nreturn text\n",
            serde_json::to_string(&command).unwrap()
        );
        let (mut stream, mut rx) = crate::stream::ToolStream::channel();
        let result = crate::tool::Tool::execute(
            &crate::builtin::RunCodeTool,
            crate::builtin::ptc::RunCodeArgs {
                description: "inner sandbox probe".to_owned(),
                code,
            },
            &ctx,
            &mut stream,
        )
        .await
        .expect("run_code");
        drop(stream);
        while rx.recv().await.is_some() {}
        let text = result_text(&result);
        if !helper_available() {
            assert!(result.is_error, "{text}");
            assert!(text.contains("does not fall back"), "{text}");
            assert!(!outside.exists());
            return;
        }
        assert!(!result.is_error, "{text}");
        assert!(text.contains("OUTSIDE_WRITE_DENIED"), "{text}");
        assert!(!text.contains("OUTSIDE_WRITE_OK"), "{text}");
        assert!(!outside.exists(), "inner shell escaped the os sandbox");
        assert_eq!(std::fs::read_to_string(&inside).unwrap_or_default(), "in\n");
    }
}
