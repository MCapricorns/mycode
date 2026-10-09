//! Linux x86_64 GNU spawn: `execveat(AT_EMPTY_PATH)` from a retained fd.
//!
//! This path is Linux x86_64 GNU only; musl, Android, and BSD stay
//! unsupported. The child is forked
//! with `process_group(0)` and a `pre_exec` hook that fail-closed marks
//! every fd ≥ 3 `FD_CLOEXEC` via raw
//! `close_range`, then launches the already-opened descriptor. There is no
//! verify-then-path reopen and no `execvp`/`ENOEXEC` shell fallback.
//! Re-hashing happens in the parent immediately before `spawn`. A same-uid
//! writer that already holds the vnode can still rewrite bytes in the
//! fork-to-`execveat` window; public APIs cannot close that race without
//! allocating in the child.
#![cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]

use std::ffi::{CString, OsString};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::Path;
use std::process::Stdio;
use std::ptr;

use tokio::process::{Child, Command};

use super::resolve::{PinnedImage, verify_pinned_digest};
use super::spawn::{
    SpawnFailure, SpawnGate, finish_pending_spawn_cleanup, wait_tokio_child_blocking,
};
use super::unix::{build_cstring_vec, build_env_cstrings};
use crate::builtin::process::{ExecutionLease, ProcessTree};
use crate::tool::ToolError;

/// First descriptor outside the standard streams and macOS hold-fd range.
const MIN_LAUNCH_FD: libc::c_int = 4;
/// First fd past stdin/stdout/stderr. `close_range` must start here so fd 3
/// (std's exec-error pipe and any other inherited capability) is sealed.
const FIRST_NONSTD_FD: libc::c_uint = 3;

/// Spawns `pinned` with `args` in `cwd` via `execveat` from the retained fd.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] when digest re-check, spawn, or
/// process-group enrollment fails.
pub(super) fn spawn_linux(
    pinned: PinnedImage,
    argv0: &str,
    args: &[String],
    cwd: &Path,
    env: &[(OsString, OsString)],
    lease: ExecutionLease,
    gate: &SpawnGate,
) -> Result<(Child, ProcessTree, PinnedImage, ExecutionLease), SpawnFailure> {
    spawn_linux_with_enroller(
        pinned,
        argv0,
        args,
        cwd,
        env,
        lease,
        gate,
        ProcessTree::enroll_unix,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the enroller is the only extra argument over the shared spawn shape"
)]
fn spawn_linux_with_enroller<F>(
    mut pinned: PinnedImage,
    argv0: &str,
    args: &[String],
    cwd: &Path,
    env: &[(OsString, OsString)],
    lease: ExecutionLease,
    gate: &SpawnGate,
    enroll: F,
) -> Result<(Child, ProcessTree, PinnedImage, ExecutionLease), SpawnFailure>
where
    F: FnOnce(&Child) -> std::io::Result<ProcessTree>,
{
    verify_pinned_digest(&mut pinned, gate)?;

    let argv = ExecvePointerTable::new(build_cstring_vec(argv0, args)?);
    let env = ExecvePointerTable::new(build_env_cstrings(env)?);
    let launch_fd = duplicate_launch_fd(&pinned.file)?;

    let mut process = Command::new(&pinned.canonical_path);
    process
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .kill_on_drop(true)
        .process_group(0);
    // SAFETY: `pre_exec` is Command's documented child hook. The closure only
    // performs async-signal-safe kernel operations (`close_range` then
    // `execveat`) with pointers into `argv` / `env` that are moved into the
    // closure and remain valid until the call. `launch_fd` is owned by the
    // closure and remains open across fork; `CLOSE_RANGE_CLOEXEC` does not
    // close it until a successful exec, so `execveat(AT_EMPTY_PATH)` still
    // sees the retained image. No allocation, lock, formatting, or
    // environment access happens inside the closure.
    unsafe {
        process.pre_exec(move || {
            mark_nonstandard_fds_cloexec()?;
            execveat_empty_path(&launch_fd, &argv, &env)
        });
    }

    gate.begin_spawn()?;
    let child = process.spawn().map_err(|err| {
        ToolError::Execution(format!(
            "failed to spawn {}: {err}",
            pinned.canonical_path.display()
        ))
    })?;
    gate.mark_launched();

    let mut pending = PendingLinuxSpawn::new(child, pinned, lease);
    match enroll(pending.child()) {
        Ok(process_tree) => pending.set_process_tree(process_tree),
        Err(err) => {
            let error = ToolError::Execution(format!(
                "failed to enroll the process group for {}: {err}",
                pending.canonical_path().display()
            ));
            let teardown = pending.cleanup();
            return Err(SpawnFailure::new(error, teardown));
        }
    }
    Ok(pending.into_parts())
}

/// Owns a launched Linux child, optional process tree, and image pin until
/// group enrollment completes or cleanup reaps the leader.
///
/// Containment (enrollment, then process-group terminate) must succeed before
/// the leader is reaped. A failed attempt keeps the unreaped child, any enrolled
/// tree, and the image pin so Drop can retry against the same identities.
struct PendingLinuxSpawn {
    child: Option<Child>,
    process_tree: Option<ProcessTree>,
    pinned: Option<PinnedImage>,
    lease: Option<ExecutionLease>,
}

impl PendingLinuxSpawn {
    fn new(child: Child, pinned: PinnedImage, lease: ExecutionLease) -> Self {
        Self {
            child: Some(child),
            process_tree: None,
            pinned: Some(pinned),
            lease: Some(lease),
        }
    }

    fn child(&self) -> &Child {
        self.child.as_ref().expect("pending child must be present")
    }

    fn set_process_tree(&mut self, process_tree: ProcessTree) {
        self.process_tree = Some(process_tree);
    }

    fn canonical_path(&self) -> &Path {
        &self
            .pinned
            .as_ref()
            .expect("pending image pin must be present")
            .canonical_path
    }

    fn into_parts(mut self) -> (Child, ProcessTree, PinnedImage, ExecutionLease) {
        let child = self.child.take().expect("pending child must be present");
        let process_tree = self
            .process_tree
            .take()
            .expect("process-group enrollment completed");
        let pinned = self
            .pinned
            .take()
            .expect("pending image pin must be present");
        let lease = self
            .lease
            .take()
            .expect("pending execution lease must be present");
        (child, process_tree, pinned, lease)
    }

    fn cleanup(&mut self) -> std::io::Result<()> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        if self.process_tree.is_none() {
            self.process_tree = Some(ProcessTree::enroll_unix(child)?);
        }
        self.process_tree
            .as_ref()
            .expect("pending process tree must be present after enrollment")
            .terminate(Some(child))?;
        wait_tokio_child_blocking(child)?;
        drop(self.child.take());
        self.process_tree = None;
        Ok(())
    }
}

impl Drop for PendingLinuxSpawn {
    fn drop(&mut self) {
        finish_pending_spawn_cleanup(|| self.cleanup());
    }
}

fn duplicate_launch_fd(file: &std::fs::File) -> Result<OwnedFd, ToolError> {
    // SAFETY: `file` is live. F_DUPFD_CLOEXEC duplicates it to the first
    // available descriptor at or above `MIN_LAUNCH_FD`.
    let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, MIN_LAUNCH_FD) };
    if fd == -1 {
        return Err(ToolError::Execution(format!(
            "failed to duplicate the pinned executable descriptor: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: fcntl returned a fresh descriptor uniquely owned here.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Replaces the child image with `execveat(AT_EMPTY_PATH)` on `launch_fd`.
///
/// A successful call does not return. The helper performs only the syscall
/// and reads `errno`, so it stays async-signal-safe for `pre_exec`.
fn execveat_empty_path(
    launch_fd: &OwnedFd,
    argv: &ExecvePointerTable,
    env: &ExecvePointerTable,
) -> std::io::Result<()> {
    // SAFETY: `launch_fd` is still open, pathname is empty with
    // AT_EMPTY_PATH, and argv/envp are NUL-terminated `*mut c_char`
    // arrays matching execveat's ABI. The pointed-to CString bytes stay
    // immutable for the life of `argv` / `env`.
    let rc = unsafe {
        libc::execveat(
            launch_fd.as_raw_fd(),
            c"".as_ptr(),
            argv.as_ptr(),
            env.as_ptr(),
            libc::AT_EMPTY_PATH,
        )
    };
    if rc == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Marks every descriptor above stderr `FD_CLOEXEC` via raw `close_range`.
///
/// `CLOSE_RANGE_CLOEXEC` does not close the descriptors, so std's exec-error
/// pipe remains writable if `execveat` fails. The launch fd is included and
/// stays usable until a successful exec. `ENOSYS`, `EINVAL`, and any other
/// kernel error fail-close the spawn; there is no inheritance fallback.
fn mark_nonstandard_fds_cloexec() -> std::io::Result<()> {
    // SAFETY: `SYS_close_range` with `CLOSE_RANGE_CLOEXEC` is an
    // async-signal-safe kernel operation. It does not allocate, take locks,
    // inspect the environment, or close descriptors. `first` is 3 and
    // `last` is `c_uint::MAX`, so `first <= last`.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            FIRST_NONSTD_FD,
            libc::c_uint::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if rc == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Owns immutable C strings and their NUL-terminated pointer table.
///
/// libc 0.2.189 `execveat` takes `argv`/`envp` as `*const *mut c_char`.
/// The table stores those ABI pointers without offering a Rust-side write
/// path: elements are derived from `CString::as_ptr()` and only the kernel
/// observes them. The trailing null pointer is retained.
struct ExecvePointerTable {
    pointers: Vec<*mut libc::c_char>,
    _entries: Vec<CString>,
}

impl ExecvePointerTable {
    fn new(entries: Vec<CString>) -> Self {
        let mut pointers: Vec<*mut libc::c_char> = entries
            .iter()
            .map(|entry| entry.as_ptr().cast_mut())
            .collect();
        pointers.push(ptr::null_mut());
        Self {
            pointers,
            _entries: entries,
        }
    }

    fn as_ptr(&self) -> *const *mut libc::c_char {
        self.pointers.as_ptr()
    }
}

// SAFETY: every pointer targets CString heap storage owned by `_entries`.
// Moving the table cannot relocate those allocations. The `*mut` element
// type matches execveat's argv/envp ABI (`char *const []`); this table never
// writes through the pointers, and no safe method returns a mutable view.
unsafe impl Send for ExecvePointerTable {}
// SAFETY: after construction the CString bytes and pointer vector are
// immutable. Concurrent reads of those bytes are sound.
unsafe impl Sync for ExecvePointerTable {}
