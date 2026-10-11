//! Shared process-containment authority for native execution builtins.
//!
//! Unix children are enrolled in a dedicated process group. On Linux the
//! child is also a subreaper, so grandchildren that lose their parent stay
//! in the tree instead of moving to init. Teardown signals every descendant
//! (including a `setsid` child in a new session) and then the process group
//! (including same-group orphans). Windows children are enrolled in a
//! kill-on-close Job Object that does not allow breakaway, before their
//! initial thread resumes. Teardown reports real Job or process-group
//! errors; an invalid Windows Job handle is not treated as evidence that
//! members exited.
mod output;
#[cfg(windows)]
mod windows;

use std::sync::OnceLock;

use tokio::process::Child;
use tokio::sync::{Mutex, MutexGuard};

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
pub(crate) use output::collect_child_output;
#[cfg(any(
    all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub(crate) use output::collect_until_exit;
#[cfg(any(
    all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub(crate) use output::drain_pipes;
pub(crate) use output::{CapturedStream, MAX_OUTPUT_BYTES, decode_captured_text};

use crate::tool::{ToolError, ToolResult};
use serde_json::json;

/// Cancelled-command error shared by the shell and program launch paths.
pub(crate) fn command_cancelled_error(teardown: Option<std::io::Error>) -> ToolError {
    match teardown {
        Some(err) => ToolError::Execution(format!(
            "command cancelled before completion; termination failed: {err}"
        )),
        None => ToolError::Execution("command cancelled before completion".into()),
    }
}

/// Output-collection failure shared by the shell and program launch paths.
pub(crate) fn collection_error(
    collection: &std::io::Error,
    teardown: Option<std::io::Error>,
) -> ToolError {
    match teardown {
        Some(err) => ToolError::Execution(format!(
            "failed to collect command output: {collection}; termination failed: {err}"
        )),
        None => ToolError::Execution(format!("failed to collect command output: {collection}")),
    }
}

/// Flags a tool result as a timeout in its details JSON.
pub(crate) fn mark_timed_out(mut result: ToolResult) -> ToolResult {
    result.details.as_mut().expect("details were populated")["timed_out"] = json!(true);
    result
}

/// Exit code spelling shared by the shell and program result builders.
pub(crate) fn display_exit(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

#[cfg(windows)]
pub(crate) use windows::{WindowsJob, current_process_is_in_job, resume_thread_handle};

/// Serializes host-controlled write/edit/shell operations so they cannot
/// race a retained executable pin. Same-account processes outside this process
/// are not covered and must not be described as isolated.
static EXECUTION_LEASE: OnceLock<Mutex<()>> = OnceLock::new();

/// Owned duration of process-wide write/edit/shell serialization.
pub(crate) type ExecutionLease = MutexGuard<'static, ()>;

/// Acquires the process-wide execution lease.
///
/// Hold the guard across host-controlled mutation or executable execution.
/// Side-effect-free validation and edit planning may finish before acquisition;
/// publication and process cleanup retain the guard until they finish. Dropping
/// it releases the lease.
pub(crate) async fn acquire_execution_lease() -> ExecutionLease {
    EXECUTION_LEASE.get_or_init(|| Mutex::new(())).lock().await
}

/// Platform teardown state kept alive until the child is reaped.
pub(crate) struct ProcessTree {
    #[cfg(unix)]
    group: UnixProcessGroupId,
    #[cfg(windows)]
    job: windows::WindowsJob,
}

impl ProcessTree {
    /// Enrolls a Unix child whose spawn requested `process_group(0)`.
    ///
    /// # Errors
    ///
    /// Returns an error when the child has already been reaped or the
    /// resulting group id is degenerate or equals the caller's group.
    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    pub(crate) fn enroll_unix(child: &Child) -> std::io::Result<Self> {
        Ok(Self {
            group: UnixProcessGroupId::for_child(child)?,
        })
    }

    /// Enrolls a Unix process-group leader identified by its pid.
    ///
    /// Used when the child is not a `tokio::process::Child` (raw `posix_spawn`).
    ///
    /// # Errors
    ///
    /// Returns an error when the pid is degenerate or equals the caller's group.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub(crate) fn enroll_leader_pid(pid: u32) -> std::io::Result<Self> {
        Ok(Self {
            group: UnixProcessGroupId::new(pid)?,
        })
    }

    /// Takes ownership of an already-assigned dedicated Windows Job.
    #[cfg(windows)]
    pub(crate) fn from_windows_job(job: WindowsJob) -> Self {
        Self { job }
    }

    /// Terminates the platform process-containment boundary synchronously.
    ///
    /// # Errors
    ///
    /// Returns a process-group or Job Object termination error.
    pub(crate) fn terminate(&self, child: Option<&Child>) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            let result = match child {
                Some(child) => self.group.kill(child),
                None => self.group.kill_saved(),
            };
            ignore_missing_process_group(result)
        }
        #[cfg(windows)]
        {
            let _ = child;
            self.job.terminate()
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = child;
            Ok(())
        }
    }
}

/// Enrolls a child started in its own process group, or on Windows assigns
/// it to a kill-on-close Job. Call this immediately after spawn, before the
/// child runs user code.
///
/// # Errors
///
/// Returns an error when the platform cannot build a process tree, the child
/// has already exited, or Job assignment fails.
pub(crate) fn enroll_spawned(child: &Child) -> std::io::Result<ProcessTree> {
    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    {
        ProcessTree::enroll_unix(child)
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        let pid = child
            .id()
            .ok_or_else(|| std::io::Error::other("child exited before enrollment"))?;
        ProcessTree::enroll_leader_pid(pid)
    }
    #[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        use std::os::windows::io::AsRawHandle as _;
        let job = windows::WindowsJob::new()?;
        job.assign_handle(child.as_raw_handle())?;
        Ok(ProcessTree::from_windows_job(job))
    }
    #[cfg(not(any(
        all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
    )))]
    {
        let _ = child;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "this target has no process-tree enrollment",
        ))
    }
}

#[cfg(unix)]
fn ignore_missing_process_group(result: std::io::Result<()>) -> std::io::Result<()> {
    result.or_else(|err| {
        if err.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(err)
        }
    })
}

#[cfg(any(
    all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub(crate) fn combine_teardown_results(
    containment: std::io::Result<()>,
    leader: std::io::Result<()>,
) -> std::io::Result<()> {
    containment.and(leader)
}

/// A process group tied to an unreaped child leader.
#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
struct UnixProcessGroupId {
    leader_pid: u32,
    group_id: libc::pid_t,
}

#[cfg(unix)]
impl UnixProcessGroupId {
    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    fn for_child(child: &Child) -> std::io::Result<Self> {
        let pid = child
            .id()
            .ok_or_else(|| std::io::Error::other("child exited before process-group enrollment"))?;
        // Command::process_group(0) performs setpgid(0, 0) before exec and
        // makes spawn fail if that setup fails. Do not require the child to
        // remain in the group here: a program can deliberately escape after
        // exec, and termination-time identity checks handle that safely.
        Self::new(pid)
    }

    fn new(pid: u32) -> std::io::Result<Self> {
        if pid <= 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing degenerate process-group id {pid}"),
            ));
        }
        if pid > libc::pid_t::MAX as u32 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("process-group id {pid} exceeds pid_t::MAX"),
            ));
        }

        let group_id = pid as libc::pid_t;
        // SAFETY: getpgrp has no arguments or failure value and only reads the
        // caller's process-group id.
        let own_group = unsafe { libc::getpgrp() };
        if group_id == own_group {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing the caller's own process-group id {pid}"),
            ));
        }
        Ok(Self {
            leader_pid: pid,
            group_id,
        })
    }

    fn current_leader(self, current_child_id: Option<u32>) -> std::io::Result<libc::pid_t> {
        match current_child_id {
            Some(pid) if pid == self.leader_pid => Ok(self.group_id),
            Some(pid) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "refusing process-group signal: Child::id is {pid}, saved leader is {}",
                    self.leader_pid
                ),
            )),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "refusing process-group signal after Child::id was cleared",
            )),
        }
    }

    fn validated_group(
        self,
        observed_group: libc::pid_t,
        own_group: libc::pid_t,
    ) -> std::io::Result<libc::pid_t> {
        if observed_group != self.group_id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "refusing process-group signal: leader now belongs to {observed_group}, \
                     saved group is {}",
                    self.group_id
                ),
            ));
        }
        if self.group_id <= 1 || self.group_id == own_group {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing broadcast or caller group {}", self.group_id),
            ));
        }
        Ok(self.group_id)
    }

    fn kill(self, child: &Child) -> std::io::Result<()> {
        let leader = self.current_leader(child.id())?;
        self.kill_leader(leader)
    }

    fn kill_saved(self) -> std::io::Result<()> {
        self.kill_leader(self.group_id)
    }

    fn kill_leader(self, leader: libc::pid_t) -> std::io::Result<()> {
        let observed_group = get_process_group(leader)?;
        // SAFETY: getpgrp has no arguments or failure value and only reads the
        // caller's current process-group id.
        let own_group = unsafe { libc::getpgrp() };
        let target = self.validated_group(observed_group, own_group)?;

        // The collection path has not waited on the child before timeout or
        // cancellation. Its live/zombie PID therefore cannot be reused between
        // this getpgid validation and the signals below, so `target` still
        // names only the original group. Descendants are signaled first, while
        // the leader is alive and parent links still point at this tree.
        // `killpg` then covers same-group processes that were reparented
        // outside the tree. Any validation failure skips both signals and
        // lets the Child-handle fallback below kill only the leader.
        let descendants = signal_descendants(leader);
        // SAFETY: `target` is positive, foreign, and was just observed as the
        // matching, still-reserved child leader's process group.
        let group = if unsafe { libc::killpg(target, libc::SIGKILL) } == 0 {
            Ok(())
        } else {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(err)
            }
        };
        descendants.and(group)
    }
}

/// How many descendant snapshots to take before `killpg`.
///
/// A child can fork between the walk and the signal. A few passes catch that
/// without waiting on a process that is spawning forever.
const DESCENDANT_KILL_PASSES: usize = 3;

/// `SIGKILL` every descendant of `root`, not `root` itself.
///
/// Process-group signals miss a `setsid` child. The walk uses parent links,
/// which on Linux still include orphans because the shell is a subreaper.
#[cfg(unix)]
fn signal_descendants(root: libc::pid_t) -> std::io::Result<()> {
    let mut error = None;
    for _ in 0..DESCENDANT_KILL_PASSES {
        for pid in list_descendants(root)? {
            if pid <= 1 || pid == root {
                continue;
            }
            // SAFETY: `pid` is a positive descendant id from the process
            // table, not this process and not pid 1. `SIGKILL` only requests
            // termination of that one process. `ESRCH` means it already exited.
            if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ESRCH) {
                    error = Some(err);
                }
            }
        }
    }
    match error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

#[cfg(target_os = "linux")]
fn list_descendants(root: libc::pid_t) -> std::io::Result<Vec<libc::pid_t>> {
    let mut children: std::collections::HashMap<libc::pid_t, Vec<libc::pid_t>> =
        std::collections::HashMap::new();
    for entry in std::fs::read_dir("/proc")?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<libc::pid_t>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(ppid) = ppid_from_stat(&stat) else {
            continue;
        };
        children.entry(ppid).or_default().push(pid);
    }
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut seen = std::collections::HashSet::new();
    seen.insert(root);
    while let Some(pid) = stack.pop() {
        let Some(kids) = children.get(&pid) else {
            continue;
        };
        for kid in kids {
            if seen.insert(*kid) {
                out.push(*kid);
                stack.push(*kid);
            }
        }
    }
    Ok(out)
}

/// Parent pid from `/proc/<pid>/stat`. `comm` may contain spaces and
/// parentheses, so the parse starts after the last `)`.
#[cfg(any(test, target_os = "linux"))]
fn ppid_from_stat(stat: &str) -> Option<libc::pid_t> {
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

#[cfg(target_os = "macos")]
fn list_descendants(root: libc::pid_t) -> std::io::Result<Vec<libc::pid_t>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut seen = std::collections::HashSet::new();
    seen.insert(root);
    while let Some(pid) = stack.pop() {
        for kid in direct_children(pid) {
            if kid > 1 && seen.insert(kid) {
                out.push(kid);
                stack.push(kid);
            }
        }
    }
    Ok(out)
}

#[cfg(target_os = "macos")]
fn direct_children(parent: libc::pid_t) -> Vec<libc::pid_t> {
    unsafe extern "C" {
        fn proc_listchildpids(
            ppid: libc::pid_t,
            buffer: *mut libc::pid_t,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }
    // SAFETY: a null buffer asks for the byte count and does not write.
    let bytes = unsafe { proc_listchildpids(parent, std::ptr::null_mut(), 0) };
    if bytes <= 0 {
        return Vec::new();
    }
    let width = std::mem::size_of::<libc::pid_t>();
    let mut buf = vec![0; (bytes as usize / width).saturating_add(8)];
    // SAFETY: `buf` is writable for `buf.len()` pids. The return value is a
    // byte count, not a pid count.
    let written =
        unsafe { proc_listchildpids(parent, buf.as_mut_ptr(), (buf.len() * width) as libc::c_int) };
    if written <= 0 {
        return Vec::new();
    }
    let count = (written as usize) / width;
    buf.truncate(count.min(buf.len()));
    buf.into_iter().filter(|pid| *pid > 1).collect()
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn list_descendants(_root: libc::pid_t) -> std::io::Result<Vec<libc::pid_t>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::ppid_from_stat;

    #[test]
    fn stat_ppid_ignores_parentheses_inside_comm() {
        assert_eq!(ppid_from_stat("12 (bash) S 7 12 12"), Some(7));
        assert_eq!(ppid_from_stat("9 (sleep 303) S 5 9 9"), Some(5));
        assert_eq!(ppid_from_stat("3 (a) b) S 42 1 1"), Some(42));
        assert!(ppid_from_stat("no paren").is_none());
    }
}

#[cfg(unix)]
fn get_process_group(pid: libc::pid_t) -> std::io::Result<libc::pid_t> {
    // SAFETY: `pid` was range-checked from Child::id and getpgid only observes
    // process metadata. A return value of -1 is the documented failure value.
    let group = unsafe { libc::getpgid(pid) };
    if group == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(group)
    }
}
