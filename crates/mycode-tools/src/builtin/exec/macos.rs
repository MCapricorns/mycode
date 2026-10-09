//! macOS spawn: public `posix_spawn` with `POSIX_SPAWN_START_SUSPENDED`.
//!
//! The child executes the pinned canonical pathname: Darwin cannot execute an
//! open descriptor (`execve` of `/dev/fd/<n>` fails with `EACCES` even for an
//! `O_EXEC` descriptor), so `posix_spawn` is the only launch primitive. The
//! retained readable pin keeps the digest recheck. Before `SIGCONT`, the child
//! must be stopped with this process as parent, `proc_pidpath` must equal the
//! retained canonical path, that path must still resolve to the pinned vnode
//! identity, the loaded architecture must be native or translated, and the
//! digest recheck must match. Public APIs cannot prove the mapped/running
//! image digest, and XNU does not enforce `ETXTBSY`. Identity is guaranteed
//! only at the suspended verification instant, and a same-account writer
//! remains outside the boundary.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::ffi::{CString, OsString};
use std::io;
use std::mem::size_of;
use std::os::fd::{AsFd as _, AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::pin::Pin;
use std::process::ExitStatus;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use rustix::fs::{FileType, Mode, OFlags, fstat, open};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, ReadBuf};

use super::resolve::{PinnedImage, rehash_image_cancellable, verify_pinned_digest};
use super::spawn::{
    ExecutionMetadata, LoadedArchitecture, SpawnFailure, SpawnGate, finish_pending_spawn_cleanup,
};
use super::unix::{build_cstring_vec, build_env_cstrings};
use crate::builtin::process::{ExecutionLease, ProcessTree};
use crate::tool::ToolError;

/// First descriptor outside the child-side stdio `dup2` targets 0 through 2.
const MIN_SPAWN_SOURCE_FD: RawFd = 3;

/// The pinned canonical pathname the child executes.
///
/// The post-spawn verification re-proves that this pathname still names the
/// retained vnode before the child is continued.
fn spawn_launch_path(pinned: &PinnedImage) -> Result<CString, ToolError> {
    CString::new(pinned.canonical_path.as_os_str().as_bytes()).map_err(|_| {
        ToolError::Execution("canonical executable path contains an interior NUL".into())
    })
}

/// CPU types from `<mach/machine.h>` (`CPU_ARCH_ABI64 | CPU_TYPE_*`).
const CPU_TYPE_X86_64: i32 = 0x0100_0007;
const CPU_TYPE_ARM64: i32 = 0x0100_000c;

/// Flavors from `<sys/proc_info.h>` that libc 0.2.189 does not export.
const PROC_PIDARCHINFO: libc::c_int = 19;

#[repr(C)]
struct ProcArchInfo {
    p_cputype: libc::cpu_type_t,
    p_cpusubtype: libc::cpu_subtype_t,
}

unsafe extern "C" {
    fn posix_spawn_file_actions_addfchdir_np(
        file_actions: *mut libc::posix_spawn_file_actions_t,
        fd: libc::c_int,
    ) -> libc::c_int;
}

/// A nonblocking pipe registered with the Tokio reactor.
pub(super) struct AsyncPipe {
    fd: AsyncFd<OwnedFd>,
}

impl AsyncPipe {
    fn new(fd: OwnedFd) -> io::Result<Self> {
        Ok(Self {
            fd: AsyncFd::new(fd)?,
        })
    }
}

impl AsyncRead for AsyncPipe {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let this = self.get_mut();
        loop {
            let mut guard = ready!(this.fd.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            match guard.try_io(|inner| {
                rustix::io::read(inner.get_ref(), unfilled).map_err(io::Error::from)
            }) {
                Ok(Ok(count)) => {
                    buf.advance(count);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(err)) => return Poll::Ready(Err(err)),
                Err(_would_block) => continue,
            }
        }
    }
}

/// Poll interval for a wait task that must remain interruptible by teardown.
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

struct MacProcessState {
    pid: Option<libc::pid_t>,
}

/// A posix_spawn child with one cancellation-safe wait operation.
pub(super) struct MacChild {
    state: Arc<Mutex<MacProcessState>>,
    waiter: Option<tokio::task::JoinHandle<io::Result<ExitStatus>>>,
    stdout: Option<OwnedFd>,
    stderr: Option<OwnedFd>,
}

impl MacChild {
    pub(super) fn take_stdout(&mut self) -> io::Result<Option<AsyncPipe>> {
        self.stdout.take().map(AsyncPipe::new).transpose()
    }

    pub(super) fn take_stderr(&mut self) -> io::Result<Option<AsyncPipe>> {
        self.stderr.take().map(AsyncPipe::new).transpose()
    }

    /// Waits for exit after the child has already been continued.
    ///
    /// # Errors
    ///
    /// Returns a `waitpid` or waiter join error.
    pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
        let state = Arc::clone(&self.state);
        let waiter = self
            .waiter
            .get_or_insert_with(|| tokio::task::spawn_blocking(move || wait_exit_shared(&state)));
        let joined = waiter.await;
        self.waiter = None;
        joined.map_err(io::Error::other)?
    }

    pub(super) fn terminate_tree(&self, process_tree: &ProcessTree) -> io::Result<()> {
        let state = lock_process_state(&self.state)?;
        let Some(pid) = state.pid else {
            return Ok(());
        };
        let containment = process_tree.terminate(None);
        let leader = if containment.is_ok() {
            Ok(())
        } else {
            kill_leader(pid)
        };
        crate::builtin::process::combine_teardown_results(containment, leader)
    }

    /// Reaps synchronously when no Tokio runtime can own cleanup.
    ///
    /// # Errors
    ///
    /// Returns a `waitpid` error other than an already-reaped `ECHILD`.
    pub(super) fn reap_blocking(&mut self) -> io::Result<()> {
        drop(self.waiter.take());
        match wait_exit_shared(&self.state) {
            Ok(_) => Ok(()),
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn lock_process_state(
    state: &Mutex<MacProcessState>,
) -> io::Result<std::sync::MutexGuard<'_, MacProcessState>> {
    state
        .lock()
        .map_err(|_| io::Error::other("macOS child process state lock was poisoned"))
}

fn kill_leader(pid: libc::pid_t) -> io::Result<()> {
    if send_signal(pid, libc::SIGKILL) == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Owns a created child PID until it is transferred to [`MacChild`].
struct SpawnedPid {
    pid: libc::pid_t,
    process_tree: Option<ProcessTree>,
    pinned: Option<PinnedImage>,
    lease: Option<ExecutionLease>,
    armed: bool,
}

/// Ownership remaining after `waitpid(..., WUNTRACED)`.
#[must_use]
#[derive(Debug)]
enum StopWait {
    /// Child is live, stopped, and still owned. The PID is not reusable yet.
    Stopped,
    /// This `waitpid` reaped an exited child. The PID may already be reused.
    ReapedExit,
    /// This `waitpid` reaped a signaled child. The PID may already be reused.
    ReapedSignal,
    /// `ECHILD`: this process has no child-wait authority for the PID.
    NoChild(io::Error),
    /// Wait failed without reaping; the child may still be live.
    WaitFailed(io::Error),
}

impl SpawnedPid {
    fn new(pid: libc::pid_t) -> Self {
        Self {
            pid,
            process_tree: None,
            pinned: None,
            lease: None,
            armed: true,
        }
    }

    fn retain_pin_and_lease(&mut self, pinned: PinnedImage, lease: ExecutionLease) {
        self.pinned = Some(pinned);
        self.lease = Some(lease);
    }

    fn set_process_tree(&mut self, process_tree: ProcessTree) {
        self.process_tree = Some(process_tree);
    }

    fn cleanup_inner(&self) -> io::Result<()> {
        // A still-suspended child with no process tree is reaped through the
        // leader only. After group enrollment, terminate the tree first and
        // reap only once that succeeds, so a retry cannot signal a reused PGID.
        match self.process_tree.as_ref() {
            None => kill_leader_and_reap(self.pid),
            Some(process_tree) => {
                process_tree.terminate(None)?;
                reap_pid(self.pid)
            }
        }
    }

    fn fail(mut self, error: ToolError) -> SpawnFailure {
        let teardown = self.cleanup_inner();
        self.armed = teardown.is_err();
        SpawnFailure::new(error, teardown)
    }

    /// Drops PID authority without `kill`, `killpg`, or `waitpid`.
    ///
    /// The child was already reaped or this process has no wait authority, so
    /// the numeric PID may already name an unrelated process.
    fn disarm(mut self, error: ToolError) -> SpawnFailure {
        self.armed = false;
        SpawnFailure::new(error, Ok(()))
    }

    fn finish_stop_wait(self, wait: StopWait) -> Result<Self, SpawnFailure> {
        match wait {
            StopWait::Stopped => Ok(self),
            StopWait::ReapedExit => Err(self.disarm(ToolError::Execution(
                "child exited before suspended verification; refusing to continue".into(),
            ))),
            StopWait::ReapedSignal => Err(self.disarm(ToolError::Execution(
                "child was signaled before suspended verification; refusing to continue".into(),
            ))),
            StopWait::NoChild(err) => Err(self.disarm(ToolError::Execution(format!(
                "failed to wait for the suspended child: {err}"
            )))),
            StopWait::WaitFailed(err) => Err(self.fail(ToolError::Execution(format!(
                "failed to wait for the suspended child: {err}"
            )))),
        }
    }

    fn into_spawn_parts(mut self) -> (ProcessTree, PinnedImage, ExecutionLease) {
        self.armed = false;
        (
            self.process_tree
                .take()
                .expect("process-group enrollment completed"),
            self.pinned
                .take()
                .expect("pending image pin must be present"),
            self.lease
                .take()
                .expect("pending execution lease must be present"),
        )
    }
}

impl Drop for SpawnedPid {
    fn drop(&mut self) {
        if self.armed {
            finish_pending_spawn_cleanup(|| self.cleanup_inner());
            self.armed = false;
        }
    }
}

/// Spawns `pinned` suspended, verifies identity, then continues it.
///
/// # Errors
///
/// Returns [`SpawnFailure`] when spawn, verification, or continue fails. Its
/// error records the [`ToolError`], and its teardown result records cleanup of
/// any live, unreaped child. If suspended waiting already reaped the child or
/// returns `ECHILD`, PID ownership is disarmed without another signal or wait.
pub(super) fn spawn_macos(
    mut pinned: PinnedImage,
    argv0: &str,
    args: &[String],
    cwd: &Path,
    env: &[(OsString, OsString)],
    lease: ExecutionLease,
    gate: &SpawnGate,
) -> Result<
    (
        MacChild,
        ProcessTree,
        PinnedImage,
        ExecutionLease,
        ExecutionMetadata,
    ),
    SpawnFailure,
> {
    verify_pinned_digest(&mut pinned, gate)?;

    let cwd_fd = normalize_spawn_source(open_cwd(cwd)?, "working directory")?;
    let (stdout_read, stdout_write) = cloexec_pipe()?;
    let stdout_write = normalize_spawn_source(stdout_write, "stdout pipe")?;
    let (stderr_read, stderr_write) = cloexec_pipe()?;
    let stderr_write = normalize_spawn_source(stderr_write, "stderr pipe")?;
    let stdin = normalize_spawn_source(open_dev_null()?, "stdin")?;
    let path = spawn_launch_path(&pinned)?;
    let argv = build_cstring_vec(argv0, args)?;
    let env = build_env_cstrings(env)?;
    let mut argv_ptrs = pointers(&argv);
    let mut env_ptrs = pointers(&env);

    // SAFETY: posix_spawnattr_t / file_actions_t are pointer-sized opaque
    // values; zero is a valid pre-init bit pattern that init replaces.
    let mut attr = unsafe { std::mem::zeroed::<libc::posix_spawnattr_t>() };
    let mut actions = unsafe { std::mem::zeroed::<libc::posix_spawn_file_actions_t>() };
    // SAFETY: attr/actions are zeroed storage that these init calls own.
    check_posix(
        unsafe { libc::posix_spawnattr_init(&raw mut attr) },
        "posix_spawnattr_init",
    )?;
    let mut attr_guard = AttrGuard {
        attr: &mut attr,
        actions: None,
    };
    check_posix(
        unsafe { libc::posix_spawn_file_actions_init(&raw mut actions) },
        "posix_spawn_file_actions_init",
    )?;
    attr_guard.actions = Some(&mut actions);

    let flags = (libc::POSIX_SPAWN_START_SUSPENDED
        | libc::POSIX_SPAWN_SETPGROUP
        | libc::POSIX_SPAWN_CLOEXEC_DEFAULT) as libc::c_short;
    // SAFETY: `attr` was initialized; flags are the public Darwin spawn bits.
    check_posix(
        unsafe { libc::posix_spawnattr_setflags(attr_guard.attr, flags) },
        "posix_spawnattr_setflags",
    )?;
    // SAFETY: `attr` was initialized; pgroup 0 makes the child its own leader.
    check_posix(
        unsafe { libc::posix_spawnattr_setpgroup(attr_guard.attr, 0) },
        "posix_spawnattr_setpgroup",
    )?;
    // SAFETY: file actions are initialized; the fd is a live parent descriptor.
    check_posix(
        unsafe {
            libc::posix_spawn_file_actions_adddup2(
                actions_ptr(&mut attr_guard),
                stdin.as_raw_fd(),
                0,
            )
        },
        "adddup2 stdin",
    )?;
    check_posix(
        unsafe {
            libc::posix_spawn_file_actions_adddup2(
                actions_ptr(&mut attr_guard),
                stdout_write.as_raw_fd(),
                1,
            )
        },
        "adddup2 stdout",
    )?;
    check_posix(
        unsafe {
            libc::posix_spawn_file_actions_adddup2(
                actions_ptr(&mut attr_guard),
                stderr_write.as_raw_fd(),
                2,
            )
        },
        "adddup2 stderr",
    )?;
    check_posix(
        unsafe {
            posix_spawn_file_actions_addfchdir_np(actions_ptr(&mut attr_guard), cwd_fd.as_raw_fd())
        },
        "posix_spawn_file_actions_addfchdir_np",
    )?;

    gate.begin_spawn()?;
    let mut pid: libc::pid_t = 0;
    // SAFETY: path/argv/envp/attr/actions are initialized. The pinned canonical
    // path is the only launch pathname; there is no descriptor launch.
    let rc = unsafe {
        libc::posix_spawn(
            &raw mut pid,
            path.as_ptr(),
            attr_guard
                .actions
                .as_deref()
                .map_or(ptr::null(), std::ptr::from_ref),
            attr_guard.attr,
            argv_ptrs.as_mut_ptr(),
            env_ptrs.as_mut_ptr(),
        )
    };
    drop(attr_guard);
    drop(stdout_write);
    drop(stderr_write);
    drop(stdin);
    drop(cwd_fd);
    check_posix(rc, "posix_spawn")?;
    let mut spawned = SpawnedPid::new(pid);
    spawned.retain_pin_and_lease(pinned, lease);

    let process_tree = match ProcessTree::enroll_leader_pid(pid as u32) {
        Ok(process_tree) => process_tree,
        Err(err) => {
            return Err(spawned.fail(ToolError::Execution(format!(
                "failed to enroll the process group: {err}"
            ))));
        }
    };
    spawned.set_process_tree(process_tree);

    let mut spawned = spawned.finish_stop_wait(wait_until_stopped(pid))?;
    let verified = {
        let pinned = spawned
            .pinned
            .as_mut()
            .expect("pending image pin must be present");
        verify_stopped_child(pid, pinned, gate)
    };
    let verified = match verified {
        Ok(verified) => verified,
        Err(err) => return Err(spawned.fail(err)),
    };
    if let Err(err) = gate.check_pending() {
        return Err(spawned.fail(err));
    }
    if send_signal(pid, libc::SIGCONT) != 0 {
        let os = io::Error::last_os_error();
        return Err(spawned.fail(ToolError::Execution(format!(
            "failed to continue the verified child: {os}"
        ))));
    }
    gate.mark_launched();
    let (process_tree, pinned, lease) = spawned.into_spawn_parts();

    let child = MacChild {
        state: Arc::new(Mutex::new(MacProcessState { pid: Some(pid) })),
        waiter: None,
        stdout: Some(stdout_read),
        stderr: Some(stderr_read),
    };
    Ok((child, process_tree, pinned, lease, verified.metadata))
}

struct VerifiedArch {
    metadata: ExecutionMetadata,
}

struct AttrGuard<'a> {
    attr: &'a mut libc::posix_spawnattr_t,
    actions: Option<&'a mut libc::posix_spawn_file_actions_t>,
}

impl Drop for AttrGuard<'_> {
    fn drop(&mut self) {
        if let Some(actions) = self.actions.take() {
            // SAFETY: actions was initialized and is destroyed once.
            let _ = unsafe { libc::posix_spawn_file_actions_destroy(actions) };
        }
        // SAFETY: attr was initialized and is destroyed once.
        let _ = unsafe { libc::posix_spawnattr_destroy(self.attr) };
    }
}

fn actions_ptr(guard: &mut AttrGuard<'_>) -> *mut libc::posix_spawn_file_actions_t {
    guard
        .actions
        .as_mut()
        .map_or(ptr::null_mut(), |actions| *actions as *mut _)
}

fn waitpid_status(
    pid: libc::pid_t,
    options: libc::c_int,
) -> io::Result<(libc::pid_t, libc::c_int)> {
    let mut status = 0;
    // SAFETY: `pid` is a specific child; `status` is waitpid output storage.
    let rc = unsafe { libc::waitpid(pid, &raw mut status, options) };
    if rc == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok((rc, status))
    }
}

fn send_signal(pid: libc::pid_t, sig: libc::c_int) -> libc::c_int {
    // SAFETY: the caller proves `pid` still names the unreaped child.
    unsafe { libc::kill(pid, sig) }
}

/// Waits for `POSIX_SPAWN_START_SUSPENDED` without assuming the child is live.
fn wait_until_stopped(pid: libc::pid_t) -> StopWait {
    loop {
        match waitpid_status(pid, libc::WUNTRACED) {
            Ok((_, status)) if libc::WIFSTOPPED(status) => return StopWait::Stopped,
            Ok((_, status)) if libc::WIFEXITED(status) => return StopWait::ReapedExit,
            Ok((_, status)) if libc::WIFSIGNALED(status) => return StopWait::ReapedSignal,
            Ok(_) => continue,
            Err(err) if err.raw_os_error() == Some(libc::EINTR) => continue,
            Err(err) if err.raw_os_error() == Some(libc::ECHILD) => {
                return StopWait::NoChild(err);
            }
            Err(err) => return StopWait::WaitFailed(err),
        }
    }
}

fn verify_stopped_child(
    pid: libc::pid_t,
    pinned: &mut PinnedImage,
    gate: &SpawnGate,
) -> Result<VerifiedArch, ToolError> {
    let info = bsd_shortinfo(pid)?;
    // SAFETY: getpid has no failure value and only reads the calling pid.
    if info.pbsi_ppid != unsafe { libc::getpid() } as u32 {
        return Err(ToolError::Execution(
            "stopped child parent pid does not match the launching process".into(),
        ));
    }
    if info.pbsi_status != libc::SSTOP {
        return Err(ToolError::Execution(
            "child is not in SSTOP at verification; refusing to continue".into(),
        ));
    }

    let mut path_buf = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: path_buf is writable storage of the documented max size.
    let path_len = unsafe {
        libc::proc_pidpath(
            pid,
            path_buf.as_mut_ptr().cast(),
            u32::try_from(path_buf.len()).unwrap_or(u32::MAX),
        )
    };
    if path_len <= 0 {
        return Err(ToolError::Execution(
            "proc_pidpath failed for the stopped child (renamed-away text vnode is rejected)"
                .into(),
        ));
    }
    path_buf.truncate(path_len as usize);
    let child_path = Path::new(
        std::str::from_utf8(&path_buf)
            .map_err(|_| ToolError::Execution("proc_pidpath returned non-UTF-8".into()))?,
    );
    if child_path != pinned.canonical_path.as_path() {
        return Err(ToolError::Execution(
            "proc_pidpath does not match the retained canonical path".into(),
        ));
    }
    verify_current_path_identity(pinned)?;

    let mut arch = ProcArchInfo {
        p_cputype: 0,
        p_cpusubtype: 0,
    };
    // SAFETY: `arch` is the documented PROC_PIDARCHINFO buffer.
    let arch_len = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDARCHINFO,
            0,
            (&raw mut arch).cast(),
            i32::try_from(size_of::<ProcArchInfo>()).expect("ProcArchInfo fits i32"),
        )
    };
    if arch_len < i32::try_from(size_of::<ProcArchInfo>()).expect("fits") {
        return Err(ToolError::Execution(
            "PROC_PIDARCHINFO is unavailable for the stopped child".into(),
        ));
    }
    if arch.p_cputype != CPU_TYPE_ARM64 && arch.p_cputype != CPU_TYPE_X86_64 {
        return Err(ToolError::Execution(
            "loaded architecture is neither arm64 nor x86_64; refusing to continue".into(),
        ));
    }
    let loaded_architecture = if arch.p_cputype == CPU_TYPE_ARM64 {
        LoadedArchitecture::Arm64
    } else {
        LoadedArchitecture::X86_64
    };
    let translated = loaded_architecture == LoadedArchitecture::X86_64;

    let digest = rehash_image_cancellable(&mut pinned.file, || gate.check_pending())?;
    if digest != pinned.digest {
        return Err(ToolError::Execution(
            "retained executable digest changed before SIGCONT \
             (XNU does not enforce ETXTBSY; the child was reaped)"
                .into(),
        ));
    }
    Ok(VerifiedArch {
        metadata: ExecutionMetadata::macos(loaded_architecture, translated),
    })
}

pub(super) fn verify_current_path_identity(pinned: &PinnedImage) -> Result<(), ToolError> {
    let current = open(
        &pinned.canonical_path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| {
        ToolError::Execution(format!(
            "current canonical executable path cannot be opened without following a link: {error}"
        ))
    })?;
    let stat = fstat(current.as_fd()).map_err(|error| {
        ToolError::Execution(format!(
            "current canonical executable path identity is unavailable: {error}"
        ))
    })?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(ToolError::Execution(
            "current canonical executable path is not a regular file".into(),
        ));
    }
    let device = crate::builtin::fs_search::unix_device_identity(stat.st_dev).map_err(|error| {
        ToolError::Execution(format!(
            "current canonical executable device identity is unavailable: {error}"
        ))
    })?;
    let inode = crate::builtin::fs_search::unix_inode_identity(stat.st_ino).map_err(|error| {
        ToolError::Execution(format!(
            "current canonical executable inode identity is unavailable: {error}"
        ))
    })?;
    if device != pinned.identity.device || inode != pinned.identity.inode {
        return Err(ToolError::Execution(
            "current canonical executable path identity does not match the retained executable"
                .into(),
        ));
    }
    Ok(())
}

fn bsd_shortinfo(pid: libc::pid_t) -> Result<libc::proc_bsdshortinfo, ToolError> {
    // SAFETY: `proc_bsdshortinfo` is a C POD filled by proc_pidinfo.
    let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdshortinfo>() };
    // SAFETY: `info` is the documented PROC_PIDT_SHORTBSDINFO buffer.
    let len = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&raw mut info).cast(),
            i32::try_from(size_of::<libc::proc_bsdshortinfo>()).expect("fits"),
        )
    };
    if len < i32::try_from(size_of::<libc::proc_bsdshortinfo>()).expect("fits") {
        return Err(ToolError::Execution(
            "PROC_PIDT_SHORTBSDINFO is unavailable for the stopped child".into(),
        ));
    }
    Ok(info)
}

fn kill_leader_and_reap(pid: libc::pid_t) -> io::Result<()> {
    let _ = send_signal(pid, libc::SIGKILL);
    reap_pid(pid)
}

fn reap_pid(pid: libc::pid_t) -> io::Result<()> {
    loop {
        match waitpid_status(pid, 0) {
            Ok((rc, _)) if rc == pid => return Ok(()),
            Err(err) if err.raw_os_error() == Some(libc::EINTR) => continue,
            Err(err) if err.raw_os_error() == Some(libc::ECHILD) => return Ok(()),
            Err(err) => return Err(err),
            Ok(_) => continue,
        }
    }
}

fn wait_exit_shared(state: &Mutex<MacProcessState>) -> io::Result<ExitStatus> {
    loop {
        let mut process = lock_process_state(state)?;
        let pid = process
            .pid
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ECHILD))?;
        match waitpid_status(pid, libc::WNOHANG) {
            Ok((rc, status)) if rc == pid => {
                process.pid = None;
                return Ok(ExitStatus::from_raw(status));
            }
            Err(err) if err.raw_os_error() == Some(libc::EINTR) => continue,
            Err(err) => {
                if err.raw_os_error() == Some(libc::ECHILD) {
                    process.pid = None;
                }
                return Err(err);
            }
            Ok(_) => {
                drop(process);
                std::thread::sleep(WAIT_POLL_INTERVAL);
            }
        }
    }
}

fn duplicate_spawn_source(fd: RawFd, what: &str) -> Result<OwnedFd, ToolError> {
    // SAFETY: `fd` is live. F_DUPFD_CLOEXEC duplicates it to the first
    // available descriptor outside the child-side targets 0 through 2.
    let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, MIN_SPAWN_SOURCE_FD) };
    if duplicated == -1 {
        return Err(ToolError::Execution(format!(
            "failed to duplicate the {what} descriptor: {}",
            io::Error::last_os_error()
        )));
    }
    // SAFETY: fcntl returned a fresh descriptor uniquely owned here.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
}

fn normalize_spawn_source(fd: OwnedFd, what: &str) -> Result<OwnedFd, ToolError> {
    if fd.as_raw_fd() >= MIN_SPAWN_SOURCE_FD {
        Ok(fd)
    } else {
        duplicate_spawn_source(fd.as_raw_fd(), what)
    }
}

fn open_cwd(cwd: &Path) -> Result<OwnedFd, ToolError> {
    open(
        cwd,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY,
        Mode::empty(),
    )
    .map_err(|err| ToolError::InvalidArgs(format!("working directory . is unavailable: {err}")))
}

fn open_dev_null() -> Result<OwnedFd, ToolError> {
    open("/dev/null", OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
        .map_err(|err| ToolError::Execution(format!("failed to open /dev/null: {err}")))
}

fn cloexec_pipe() -> Result<(OwnedFd, OwnedFd), ToolError> {
    let mut fds = [0; 2];
    // SAFETY: `fds` is writable storage for pipe(2)'s two descriptors.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(ToolError::Execution(format!(
            "failed to create stdio pipes: {}",
            io::Error::last_os_error()
        )));
    }
    for fd in fds {
        // SAFETY: `fd` is a fresh pipe end; F_SETFD only sets FD_CLOEXEC.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            let err = io::Error::last_os_error();
            close_pipe(fds);
            return Err(ToolError::Execution(format!(
                "failed to set FD_CLOEXEC on a pipe: {err}"
            )));
        }
    }
    // SAFETY: F_GETFL reads flags from the live pipe read descriptor.
    let read_flags = unsafe { libc::fcntl(fds[0], libc::F_GETFL) };
    if read_flags == -1 {
        let err = io::Error::last_os_error();
        close_pipe(fds);
        return Err(ToolError::Execution(format!(
            "failed to read pipe status flags: {err}"
        )));
    }
    // SAFETY: F_SETFL updates only status flags on the live read descriptor.
    if unsafe { libc::fcntl(fds[0], libc::F_SETFL, read_flags | libc::O_NONBLOCK) } == -1 {
        let err = io::Error::last_os_error();
        close_pipe(fds);
        return Err(ToolError::Execution(format!(
            "failed to make a pipe nonblocking: {err}"
        )));
    }
    // SAFETY: pipe(2) returned two uniquely owned descriptors.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn close_pipe(fds: [RawFd; 2]) {
    // SAFETY: this helper is called only before ownership transfers to OwnedFd.
    unsafe {
        let _ = libc::close(fds[0]);
        let _ = libc::close(fds[1]);
    }
}

fn check_posix(rc: libc::c_int, what: &str) -> Result<(), ToolError> {
    if rc == 0 {
        Ok(())
    } else {
        Err(ToolError::Execution(format!(
            "{what} failed: {}",
            io::Error::from_raw_os_error(rc)
        )))
    }
}

fn pointers(entries: &[CString]) -> Vec<*mut libc::c_char> {
    let mut ptrs: Vec<*mut libc::c_char> = entries
        .iter()
        .map(|entry| entry.as_ptr().cast_mut())
        .collect();
    ptrs.push(ptr::null_mut());
    ptrs
}
