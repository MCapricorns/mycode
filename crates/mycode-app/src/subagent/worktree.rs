//! Git worktree leases for write-capable roles: acquire a detached
//! checkout, hand its diff back with the result, and reclaim clean leases
//! a crashed process left behind. Modified checkouts remain recoverable.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use mycode_agent::HomeLayout;

/// `git` for a worktree lease. On Windows the desktop process has no
/// console, so a plain spawn of `git.exe` allocates a black window.
pub(super) fn git_command() -> std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: do not allocate a console for this child.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = std::process::Command::new("git");
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("git")
    }
}

/// One git worktree lease: a disposable checkout plus its manifest, so a
/// crashed process can recover leases on the next start.
pub(crate) struct WorktreeLease {
    pub(crate) path: PathBuf,
    pub(crate) manifest: PathBuf,
}

impl WorktreeLease {
    /// Creates a detached worktree of the current repository HEAD.
    pub(crate) fn acquire(home: &HomeLayout, repo: &Path) -> Result<Self, String> {
        static NEXT_LEASE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let stamp = crate::unix_now().as_nanos();
        let ordinal = NEXT_LEASE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("agent-{}-{stamp}-{ordinal}", std::process::id());
        let leases = home.root().join(WORKTREE_LEASE_DIR);
        std::fs::create_dir_all(&leases).map_err(|error| format!("lease dir: {error}"))?;
        let path = leases.join(&id);
        let manifest = leases.join(format!("{id}.json"));
        let record = serde_json::json!({
            "repo": repo.to_string_lossy(),
            "path": path.to_string_lossy(),
        });
        // Publish recovery metadata before git creates the checkout.
        std::fs::write(&manifest, record.to_string())
            .map_err(|error| format!("lease manifest: {error}"))?;
        let output = git_command()
            .arg("-C")
            .arg(repo)
            .args(["worktree", "add", "--detach"])
            .arg(&path)
            .output()
            .map_err(|error| format!("git worktree: {error}"))?;
        if !output.status.success() {
            let _ = std::fs::remove_file(&manifest);
            let _ = std::fs::remove_dir(&path);
            let reason = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git worktree add failed: {}", reason.trim()));
        }
        Ok(Self { path, manifest })
    }

    /// Releases a clean lease. A failed removal preserves both the checkout
    /// and its recovery manifest, including untracked and ignored files.
    pub(crate) fn release(self) -> bool {
        // git's own clean check omits ignored files. A build artifact may
        // also be user work, and a retained checkout must keep it intact.
        let clean = git_command()
            .arg("-C")
            .arg(&self.path)
            .args([
                "status",
                "--porcelain",
                "-z",
                "--untracked-files=all",
                "--ignored=matching",
            ])
            .output()
            .is_ok_and(|output| output.status.success() && output.stdout.is_empty());
        if !clean {
            return false;
        }
        // On Windows git cannot remove the checkout it is using as cwd.
        let repo = std::fs::read(&self.manifest)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| value["repo"].as_str().map(PathBuf::from));
        let Some(repo) = repo else {
            return false;
        };
        let removed = git_command()
            .arg("-C")
            .arg(repo)
            .args(["worktree", "remove"])
            .arg(&self.path)
            .output()
            .is_ok_and(|output| output.status.success());
        if removed {
            let _ = std::fs::remove_file(&self.manifest);
        }
        removed
    }
}

/// Releases a lease the run never handed off.
///
/// A parent cancel stops polling a running `agent` call and drops it
/// mid-await. Without this guard the checkout stayed registered in the
/// user's repository until the next app start.
pub(super) struct LeaseGuard(pub(super) Option<WorktreeLease>);

impl LeaseGuard {
    pub(super) fn path(&self) -> Option<&Path> {
        self.0.as_ref().map(|lease| lease.path.as_path())
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let Some(lease) = self.0.take() else {
            return;
        };
        // `git worktree remove` blocks; keep it off the runtime thread.
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn_blocking(move || lease.release())),
            Err(_) => {
                lease.release();
            }
        }
    }
}

/// Captured diff cap handed back with a subagent result.
const MAX_WORKTREE_DIFF_BYTES: usize = 256 * 1024;

/// Reads the worktree diff while the checkout still exists.
pub(crate) fn capture_worktree_diff(path: &Path) -> String {
    let mut out = String::new();
    match git_command()
        .arg("-C")
        .arg(path)
        .args(["diff", "--binary", "--no-ext-diff", "HEAD"])
        .output()
    {
        Ok(output) if output.status.success() => {
            out.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        Ok(output) => {
            out.push_str("worktree diff failed: ");
            out.push_str(String::from_utf8_lossy(&output.stderr).trim());
            out.push('\n');
        }
        Err(error) => {
            let _ = writeln!(out, "worktree diff failed: {error}");
        }
    }
    // `-z` prints raw NUL-terminated paths. The line format quotes names
    // with spaces or non-ASCII bytes, and the quoted spelling failed the
    // `path.join` lookup below.
    match git_command()
        .arg("-C")
        .arg(path)
        .args(["status", "--porcelain", "-z", "-uall"])
        .output()
    {
        Ok(output) if output.status.success() => {
            for record in String::from_utf8_lossy(&output.stdout).split('\0') {
                let Some(name) = record.strip_prefix("?? ") else {
                    continue;
                };
                if name.is_empty() || name.split('/').any(|part| part == "..") {
                    continue;
                }
                let file = path.join(name);
                let _ = writeln!(out, "diff --git a/{name} b/{name}");
                match std::fs::read_to_string(&file) {
                    Ok(body) => {
                        out.push_str("new file\n--- /dev/null\n+++ b/");
                        out.push_str(name);
                        out.push('\n');
                        out.push_str(&body);
                        if !body.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                    Err(error) => {
                        let _ = writeln!(out, "new file unreadable: {error}");
                    }
                }
            }
        }
        Ok(output) => {
            out.push_str("worktree status failed: ");
            out.push_str(String::from_utf8_lossy(&output.stderr).trim());
            out.push('\n');
        }
        Err(error) => {
            let _ = writeln!(out, "worktree status failed: {error}");
        }
    }
    cap_diff(out)
}

fn cap_diff(mut text: String) -> String {
    if text.len() <= MAX_WORKTREE_DIFF_BYTES {
        return text;
    }
    text.truncate(text.floor_char_boundary(MAX_WORKTREE_DIFF_BYTES));
    text.push_str("\n... diff truncated\n");
    text
}

/// Joins a subagent answer with the diff captured from its worktree.
#[must_use]
pub(crate) fn format_subagent_handoff(result: &str, diff: &str) -> String {
    if diff.trim().is_empty() {
        return result.to_owned();
    }
    format!("{result}\n\n<worktree-diff>\n{diff}</worktree-diff>")
}

/// Captures the diff and removes the checkout only when git can do so
/// without discarding files. A retained path precedes the answer so the
/// tool's answer cap cannot hide where to recover the full changes.
pub(crate) fn handoff_worktree(
    lease: WorktreeLease,
    result: Result<String, mycode_agent::ToolError>,
) -> Result<String, mycode_agent::ToolError> {
    let diff = capture_worktree_diff(&lease.path);
    let path = lease.path.clone();
    let retained = !lease.release();
    let with_location = |text: &str| {
        let text = format_subagent_handoff(text, &diff);
        if retained {
            format!(
                "Worktree retained at {}. Changes remain available there.\n\n{text}",
                path.display()
            )
        } else {
            text
        }
    };
    match result {
        Ok(text) => Ok(with_location(&text)),
        Err(error) => Err(mycode_agent::ToolError::Execution(with_location(
            &error.to_string(),
        ))),
    }
}

/// Directory for worktree leases created by this build.
const WORKTREE_LEASE_DIR: &str = "agent-worktrees";
/// Lease directory written when the delegation tool was still named `task`.
const LEGACY_WORKTREE_LEASE_DIR: &str = "task-worktrees";

/// Reclaims clean leases left behind by a crashed process. A modified or
/// unavailable checkout and its manifest are kept for manual recovery.
///
/// A lease is named after the process that took it, so a lease whose process
/// still runs is that instance's subagent at work and is kept.
///
/// Older builds stored leases under [`LEGACY_WORKTREE_LEASE_DIR`]. Both
/// directories are reclaimed so a rename does not orphan a checkout.
pub(crate) fn recover_agent_worktrees(home: &HomeLayout) {
    recover_worktree_lease_dir(home, WORKTREE_LEASE_DIR);
    recover_worktree_lease_dir(home, LEGACY_WORKTREE_LEASE_DIR);
}

fn recover_worktree_lease_dir(home: &HomeLayout, directory: &str) {
    let leases = home.root().join(directory);
    let Ok(entries) = std::fs::read_dir(&leases) else {
        return;
    };
    for entry in entries.flatten() {
        if lease_in_use(
            &entry.file_name(),
            crate::updates::apply::process_is_running,
        ) {
            continue;
        }
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
            && let Ok(bytes) = std::fs::read(&path)
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && let Some(lease) = value["path"].as_str()
        {
            WorktreeLease {
                path: PathBuf::from(lease),
                manifest: path,
            }
            .release();
        }
        // A directory without readable metadata may contain user work too.
        // Never erase it as a fallback for an unsuccessful git removal.
    }
}

/// Whether lease entry `name` (`agent-{pid}-{stamp}` or its `.json`
/// manifest) belongs to a running process other than this one.
fn lease_in_use(name: &std::ffi::OsStr, running: impl Fn(u32) -> bool) -> bool {
    name.to_str()
        .and_then(|name| name.split('-').nth(1))
        .and_then(|pid| pid.parse::<u32>().ok())
        .is_some_and(|pid| pid != std::process::id() && running(pid))
}

#[cfg(test)]
mod tests;
