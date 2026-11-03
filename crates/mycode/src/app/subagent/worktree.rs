//! Git worktree leases for write-capable roles: acquire a detached
//! checkout, hand its diff back with the result, and reclaim clean leases
//! a crashed process left behind. Modified checkouts remain recoverable.

use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::config::HomeLayout;

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
        let stamp = crate::core::unix_now().as_nanos();
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

    /// Reopens only a recorded linked worktree from this home and repository.
    /// This performs no checkout, reset, clean, apply, merge or file replacement.
    pub(crate) fn reopen(home: &HomeLayout, repo: &Path, path: &Path) -> Result<Self, String> {
        let missing =
            |error| format!("retained worktree is unavailable; no HEAD fallback: {error}");
        let leases = home
            .root()
            .join(WORKTREE_LEASE_DIR)
            .canonicalize()
            .map_err(missing)?;
        let path = path.canonicalize().map_err(missing)?;
        if path.parent() != Some(leases.as_path()) || !path.join(".git").is_file() {
            return Err("retained path is not a linked worktree owned by this home".into());
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("invalid retained worktree name")?;
        let manifest = leases.join(format!("{name}.json"));
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).map_err(missing)?)
                .map_err(|e| format!("retained worktree manifest: {e}"))?;
        let recorded = |key: &str| -> Result<PathBuf, String> {
            PathBuf::from(
                record[key]
                    .as_str()
                    .ok_or("incomplete retained worktree manifest")?,
            )
            .canonicalize()
            .map_err(missing)
        };
        if recorded("repo")? != repo.canonicalize().map_err(missing)? || recorded("path")? != path {
            return Err("retained worktree belongs to another repository or path".into());
        }
        let git_path = |cwd: &Path, flag: &str| -> Result<PathBuf, String> {
            let output = git_command()
                .arg("-C")
                .arg(cwd)
                .args(["rev-parse", flag])
                .output()
                .map_err(missing)?;
            if !output.status.success() {
                return Err("retained worktree Git metadata is unavailable".into());
            }
            cwd.join(String::from_utf8_lossy(&output.stdout).trim())
                .canonicalize()
                .map_err(missing)
        };
        if git_path(&path, "--show-toplevel")? != path
            || git_path(&path, "--git-common-dir")? != git_path(repo, "--git-common-dir")?
        {
            return Err("retained worktree is not registered with the original repository".into());
        }
        Ok(Self { path, manifest })
    }

    /// Releases a clean lease. A failed removal preserves both the checkout
    /// and its recovery manifest, including untracked and ignored files.
    pub(crate) fn release(self) -> bool {
        // git's own clean check omits ignored files. A build artifact may
        // also be user work, and a retained checkout must keep it intact.
        let clean = capped_git_output(
            git_command().arg("-C").arg(&self.path).args([
                "status",
                "--porcelain",
                "-z",
                "--untracked-files=all",
                "--ignored=matching",
            ]),
            1,
        )
        .is_ok_and(|(output, clipped)| {
            !clipped && output.status.success() && output.stdout.is_empty()
        });
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

/// Reclaims a lease never handed to a child. Activated leases leave this
/// guard before workers can start; abandoning an active run preserves its
/// checkout and recovery manifest instead of racing native worker cleanup.
pub(super) struct LeaseGuard(pub(super) Option<WorktreeLease>);

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let Some(lease) = self.0.take() else {
            return;
        };
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
    match capped_git_output(
        git_command().arg("-C").arg(path).args([
            "diff",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
        ]),
        MAX_WORKTREE_DIFF_BYTES,
    ) {
        Ok((output, clipped)) if output.status.success() || clipped => {
            out.push_str(&String::from_utf8_lossy(&output.stdout));
            if clipped {
                return cap_diff(out);
            }
        }
        Ok((output, _)) => {
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
    match capped_git_output(
        git_command()
            .arg("-C")
            .arg(path)
            .args(["status", "--porcelain", "-z", "-uall"]),
        MAX_WORKTREE_DIFF_BYTES,
    ) {
        Ok((mut output, clipped)) if output.status.success() || clipped => {
            if clipped {
                // A partial final path must never become a different lookup.
                let complete = output
                    .stdout
                    .iter()
                    .rposition(|byte| *byte == 0)
                    .map_or(0, |position| position + 1);
                output.stdout.truncate(complete);
            }
            for record in String::from_utf8_lossy(&output.stdout).split('\0') {
                let Some(name) = record.strip_prefix("?? ") else {
                    continue;
                };
                if name.is_empty() || name.split('/').any(|part| part == "..") {
                    continue;
                }
                let file = path.join(name);
                let _ = writeln!(out, "diff --git a/{name} b/{name}");
                match untracked_file_text(&file, MAX_WORKTREE_DIFF_BYTES.saturating_sub(out.len()))
                {
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
                if out.len() > MAX_WORKTREE_DIFF_BYTES {
                    break;
                }
            }
            if clipped {
                out.push_str("\n... diff truncated\n");
            }
        }
        Ok((output, _)) => {
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

/// Bound output while Git runs, rather than allocating an entire binary
/// patch or huge status listing before clipping the handoff. Drain stderr
/// concurrently so a full error pipe cannot block stdout, retaining its prefix.
fn capped_git_output(
    command: &mut std::process::Command,
    limit: usize,
) -> std::io::Result<(std::process::Output, bool)> {
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = (&mut stderr).take(limit as u64).read_to_end(&mut bytes);
        if result.is_ok() {
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        }
        bytes
    });
    let mut bytes = Vec::new();
    let read = stdout
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes);
    let clipped = bytes.len() > limit;
    if clipped || read.is_err() {
        let _ = child.kill();
    }
    let status = child.wait();
    let errors = errors.join().unwrap_or_default();
    read?;
    Ok((
        std::process::Output {
            status: status?,
            stdout: bytes,
            stderr: errors,
        },
        clipped,
    ))
}

/// A symlink contributes its link text, as Git would, never the target's
/// contents. Non-regular files (for example FIFOs) must not block handoff.
/// Read at most the remaining preview budget plus one byte to mark clipping.
fn untracked_file_text(path: &Path, remaining: usize) -> std::io::Result<String> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(std::fs::read_link(path)?.to_string_lossy().into_owned());
    }
    if !metadata.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(remaining.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    // A clipped UTF-8 codepoint must not discard the entire otherwise valid
    // preview. Binary data keeps the existing unreadable-file behavior.
    let valid = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() && bytes.len() > remaining => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).expect("valid UTF-8 prefix")
        }
        Err(error) => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
    };
    let mut text = valid.to_owned();
    if bytes.len() > remaining {
        text.push_str("\n... diff truncated\n");
    }
    Ok(text)
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
    result: Result<String, crate::tools::ToolError>,
) -> Result<String, crate::tools::ToolError> {
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
        Err(error) => Err(crate::tools::ToolError::Execution(with_location(
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
            crate::app::updates::apply::process_is_running,
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
