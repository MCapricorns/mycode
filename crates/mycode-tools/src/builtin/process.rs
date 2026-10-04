//! Shared process-containment authority for native execution builtins.
//!
//! Unix children are enrolled in a dedicated process group. Windows children
//! are enrolled in a kill-on-close Job Object before their initial thread
//! resumes. Teardown reports real Job or process-group errors; an invalid
//! Windows Job handle is not treated as evidence that members exited.
//! Windows ARM64 does not launch processes, so the Job helpers are unused
//! there and the warnings are expected.
#![cfg_attr(
    not(any(
        all(windows, target_arch = "x86_64"),
        all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )),
    allow(dead_code, unused_imports, unused_variables)
)]
mod output;
#[cfg(windows)]
mod windows;

use std::sync::OnceLock;

use tokio::process::Child;
use tokio::sync::{Mutex, MutexGuard};

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
pub(crate) use output::collect_child_output;
#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub(crate) use output::drain_pipes;
pub(crate) use output::{CapturedStream, MAX_OUTPUT_BYTES, decode_captured_text};

#[cfg(windows)]
pub(crate) use windows::{WindowsJob, current_process_is_in_job, resume_thread_handle};

/// Serializes host-controlled write/edit/shell/exec operations so they cannot
/// race a retained executable pin. Same-account processes outside this process
/// are not covered and must not be described as isolated.
static EXECUTION_LEASE: OnceLock<Mutex<()>> = OnceLock::new();

/// Owned duration of process-wide write/edit/shell/exec serialization.
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

    /// Process-tree storage for a fixture `LiveSpawn` that never terminates.
    ///
    /// Fixture cleanup never calls [`Self::terminate`], so this tree must not
    /// name a live process group or Job members.
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
    all(windows, target_arch = "x86_64"),
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
        // this getpgid validation and killpg, so `target` still names only the
        // original group. Any validation failure skips the group signal and
        // lets the Child-handle fallback below kill only the leader.
        // SAFETY: `target` is positive, foreign, and was just observed as the
        // matching, still-reserved child leader's process group.
        if unsafe { libc::killpg(target, libc::SIGKILL) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
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
