//! Windows x64 and ARM64 spawn: `CreateProcessW` from a retained executable
//! handle.
//!
//! Both targets use this path. `CreateProcessW` asks the OS loader to accept
//! a native PE (AMD64 on x64, ARM64 or ARM64X on ARM64) and an emulated image
//! when that host provides the emulator. This module does not filter COFF
//! machine types itself.
//!
//! Sequence: pin (`FILE_SHARE_READ` only) → spawn suspended with Unicode W
//! APIs → enroll a dedicated kill-on-close Job → verify the actual process
//! image against the retained path/file-id/digest → resume the
//! `CreateProcessW` thread handle. Replacement or rewrite before verification
//! rejects and reaps. Resume never uses a pid lookup. Same-account writers
//! that already hold the file remain outside the security boundary.
#![cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::io::{AsRawHandle, FromRawHandle as _, OwnedHandle};
use std::os::windows::process::ExitStatusExt as _;
use std::path::Path;
use std::process::ExitStatus;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CompareObjectHandles, ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, GENERIC_READ,
    HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, GetProcessId, INFINITE,
    InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION,
    PROCESS_NAME_WIN32, QueryFullProcessImageNameW, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    STARTUPINFOW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::argv::windows_command_line_utf16;
use super::resolve::{PinnedImage, identity_of, rehash_image_cancellable, verify_pinned_digest};
use super::spawn::{SpawnFailure, SpawnFailureKind, SpawnGate, finish_pending_spawn_cleanup};
use crate::builtin::process::{
    ProcessTree, WindowsJob, combine_teardown_results, current_process_is_in_job,
    resume_thread_handle,
};
use crate::tool::ToolError;

/// Legacy Win32 path budget, including the terminating UTF-16 NUL.
///
/// `CreateProcessW` needs the verbatim prefix beyond this budget. Keeping it
/// also preserves names whose trailing characters or DOS-device spelling have
/// meaning only under verbatim parsing.
const MAX_PATH_UTF16_UNITS_WITH_NUL: usize = 260;

/// A resumed Windows child plus its redirected pipes.
pub(super) struct WindowsChild {
    process: OwnedHandle,
    stdout: Option<tokio::fs::File>,
    stderr: Option<tokio::fs::File>,
}

impl WindowsChild {
    pub(super) fn take_stdout(&mut self) -> Option<tokio::fs::File> {
        self.stdout.take()
    }

    pub(super) fn take_stderr(&mut self) -> Option<tokio::fs::File> {
        self.stderr.take()
    }

    /// Waits for the process handle. Does not use a pid.
    ///
    /// # Errors
    ///
    /// Returns a wait or `GetExitCodeProcess` error.
    pub(super) async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        let wait_handle = duplicate_handle(&self.process)?;
        let code = tokio::task::spawn_blocking(move || wait_exit_code(wait_handle)).await?;
        Ok(ExitStatus::from_raw(code?))
    }

    /// Waits synchronously when no Tokio runtime can own cleanup.
    ///
    /// # Errors
    ///
    /// Returns a wait or `GetExitCodeProcess` error.
    pub(super) fn wait_blocking(&self) -> std::io::Result<ExitStatus> {
        let code = wait_exit_code(duplicate_handle(&self.process)?)?;
        Ok(ExitStatus::from_raw(code))
    }

    pub(super) fn terminate(&self) -> std::io::Result<()> {
        terminate(&self.process)
    }
}

/// Spawns `pinned` with MSVC-quoted `args` in `cwd`.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArgs`] when the command line exceeds the
/// `CreateProcessW` budget and [`ToolError::Execution`] when spawn, Job
/// enrollment, image verification, or resume fails.
pub(super) fn spawn_windows(
    mut pinned: PinnedImage,
    argv0: &str,
    args: &[String],
    cwd: &Path,
    env: &[(OsString, OsString)],
    gate: &SpawnGate,
) -> Result<(WindowsChild, ProcessTree, PinnedImage), SpawnFailure> {
    verify_pinned_digest(&mut pinned, gate)?;

    let parent_job = current_process_is_in_job().map_err(|err| {
        ToolError::Execution(format!("failed to query the host's Job membership: {err}"))
    })?;
    gate.begin_spawn()?;
    let spawned = launch_with_nested_job_fallback(parent_job, |breakaway| {
        spawn_attempt(&pinned, argv0, args, cwd, env, breakaway, gate)
    })?;
    let child = WindowsChild {
        process: spawned.process,
        stdout: Some(tokio::fs::File::from_std(File::from(spawned.stdout))),
        stderr: Some(tokio::fs::File::from_std(File::from(spawned.stderr))),
    };
    Ok((child, ProcessTree::from_windows_job(spawned.job), pinned))
}

/// Runs one nested-Job-aware spawn attempt.
///
/// The first attempt never requests breakaway. Exactly one
/// `CREATE_BREAKAWAY_FROM_JOB` retry is allowed when the host is already in a
/// Job, the first failure is a nested-Job enrollment rejection, and that
/// attempt's teardown succeeded. CreateProcessW, image verification, Job
/// creation or configuration, resume, cleanup, and unrelated failures are
/// returned unchanged and never relabeled as nested-Job rejection.
///
/// # Errors
///
/// Returns the first non-retryable [`SpawnFailure`], or the breakaway
/// attempt's failure with that attempt's teardown result.
pub(super) fn launch_with_nested_job_fallback<T>(
    parent_in_job: bool,
    mut attempt: impl FnMut(bool) -> Result<T, SpawnFailure>,
) -> Result<T, SpawnFailure> {
    match attempt(false) {
        Ok(spawned) => Ok(spawned),
        Err(first) if should_retry_breakaway(parent_in_job, &first) => {
            let first_error = first.error.to_string();
            attempt(true).map_err(|second| SpawnFailure {
                error: ToolError::Execution(format!(
                    "parent Job rejected nested dedicated Job enrollment ({first_error}); \
                     CREATE_BREAKAWAY_FROM_JOB fallback failed ({}); refusing \
                     to run without enforceable descendant containment",
                    second.error
                )),
                teardown: second.teardown,
                kind: SpawnFailureKind::Unrelated,
            })
        }
        Err(err) => Err(err),
    }
}

fn should_retry_breakaway(parent_in_job: bool, first: &SpawnFailure) -> bool {
    parent_in_job
        && first.kind == SpawnFailureKind::NestedJobEnrollmentRejected
        && first.teardown.is_ok()
}

/// Classifies an `AssignProcessToJobObject` OS error.
///
/// `ERROR_ACCESS_DENIED` is the documented failure when the still-suspended
/// child already belongs to a Job that rejects nesting or breakaway.
#[must_use]
pub(super) fn nested_job_enrollment_kind(err: &std::io::Error) -> SpawnFailureKind {
    if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
        SpawnFailureKind::NestedJobEnrollmentRejected
    } else {
        SpawnFailureKind::Unrelated
    }
}

struct SpawnedSuspended {
    process: OwnedHandle,
    stdout: OwnedHandle,
    stderr: OwnedHandle,
    job: WindowsJob,
}

/// Owns a newly created suspended process until it is safely transferred.
struct SuspendedProcess {
    process: Option<OwnedHandle>,
    thread: Option<OwnedHandle>,
    job: Option<WindowsJob>,
    armed: bool,
}

impl SuspendedProcess {
    fn new(process: OwnedHandle, thread: OwnedHandle) -> Self {
        Self {
            process: Some(process),
            thread: Some(thread),
            job: None,
            armed: true,
        }
    }

    fn process(&self) -> &OwnedHandle {
        self.process.as_ref().expect("process ownership is live")
    }

    fn thread(&self) -> &OwnedHandle {
        self.thread.as_ref().expect("thread ownership is live")
    }

    fn set_job(&mut self, job: WindowsJob) {
        self.job = Some(job);
    }

    fn cleanup_inner(&self) -> std::io::Result<()> {
        let process = self.process();
        let containment = match self.job.as_ref() {
            Some(job) => job.terminate(),
            None => terminate(process),
        };
        let leader = if containment.is_ok() {
            wait_process(process)
        } else {
            let fallback = terminate(process);
            if fallback.is_ok() {
                wait_process(process)
            } else {
                fallback
            }
        };
        combine_teardown_results(containment, leader)
    }

    fn fail(self, error: ToolError) -> SpawnFailure {
        self.fail_classified(error, SpawnFailureKind::Unrelated)
    }

    fn fail_classified(mut self, error: ToolError, kind: SpawnFailureKind) -> SpawnFailure {
        let teardown = self.cleanup_inner();
        self.armed = teardown.is_err();
        match kind {
            SpawnFailureKind::NestedJobEnrollmentRejected => {
                SpawnFailure::nested_job_enrollment_rejected(error, teardown)
            }
            SpawnFailureKind::Unrelated => SpawnFailure::new(error, teardown),
        }
    }

    fn into_spawned(mut self, stdout: OwnedHandle, stderr: OwnedHandle) -> SpawnedSuspended {
        self.armed = false;
        drop(self.thread.take());
        SpawnedSuspended {
            process: self.process.take().expect("process ownership is live"),
            stdout,
            stderr,
            job: self.job.take().expect("Job enrollment completed"),
        }
    }
}

impl Drop for SuspendedProcess {
    fn drop(&mut self) {
        if self.armed {
            finish_pending_spawn_cleanup(|| self.cleanup_inner());
            self.armed = false;
        }
    }
}

/// RAII storage for one initialized process-thread attribute list.
struct ProcThreadAttributeList {
    list: windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST,
    _storage: Vec<usize>,
}

impl ProcThreadAttributeList {
    fn with_handle_list(
        handles: &[windows_sys::Win32::Foundation::HANDLE],
    ) -> Result<Self, ToolError> {
        let mut bytes = 0_usize;
        // SAFETY: a null list is the documented sizing call; `bytes` is valid
        // output storage. The call must fail with ERROR_INSUFFICIENT_BUFFER.
        let sized = unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &raw mut bytes) };
        let sizing_error = std::io::Error::last_os_error();
        if sized != 0
            || bytes == 0
            || sizing_error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        {
            return Err(ToolError::Execution(format!(
                "failed to size the inherited-handle attribute list: {sizing_error}"
            )));
        }

        let units = bytes.div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; units];
        let list = storage.as_mut_ptr().cast();
        // SAFETY: `storage` is aligned, writable, and at least `bytes` long.
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &raw mut bytes) } == 0 {
            return Err(ToolError::Execution(format!(
                "failed to initialize the inherited-handle attribute list: {}",
                std::io::Error::last_os_error()
            )));
        }
        let attributes = Self {
            list,
            _storage: storage,
        };
        // SAFETY: `attributes.list` is initialized for one attribute and
        // `handles` remains live through CreateProcessW. The byte count uses
        // HANDLE units, not UTF-16 or character units.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(handles),
                null_mut(),
                null(),
            )
        };
        if updated == 0 {
            return Err(ToolError::Execution(format!(
                "failed to restrict inherited child handles: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(attributes)
    }

    fn as_ptr(&self) -> windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST {
        self.list
    }
}

impl Drop for ProcThreadAttributeList {
    fn drop(&mut self) {
        // SAFETY: `list` was initialized once and is deleted exactly once
        // before its backing storage is released.
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

fn spawn_attempt(
    pinned: &PinnedImage,
    argv0: &str,
    args: &[String],
    cwd: &Path,
    env: &[(OsString, OsString)],
    breakaway: bool,
    gate: &SpawnGate,
) -> Result<SpawnedSuspended, SpawnFailure> {
    gate.check_pending()?;
    let mut cmd = windows_command_line_utf16(OsStr::new(argv0), args)?;
    cmd.push(0);
    let launch_path = win32_launch_path(&pinned.canonical_path);
    let application = wide_os(launch_path.as_ref())?;
    let directory = wide_os(cwd.as_os_str())?;
    let env_block = unicode_env_block(env);

    let stdin = open_nul()?;
    let (stdout_read, stdout_write) = inheritable_pipe()?;
    let (stderr_read, stderr_write) = inheritable_pipe()?;
    clear_inherit(&stdout_read)?;
    clear_inherit(&stderr_read)?;

    let mut flags = CREATE_UNICODE_ENVIRONMENT
        | CREATE_NEW_PROCESS_GROUP
        | CREATE_SUSPENDED
        | CREATE_NO_WINDOW
        | EXTENDED_STARTUPINFO_PRESENT;
    if breakaway {
        flags |= CREATE_BREAKAWAY_FROM_JOB;
    }

    let inherited = [
        stdin.as_raw_handle(),
        stdout_write.as_raw_handle(),
        stderr_write.as_raw_handle(),
    ];
    let attributes = ProcThreadAttributeList::with_handle_list(&inherited)?;
    let startup = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: u32::try_from(size_of::<STARTUPINFOEXW>()).expect("STARTUPINFOEXW fits u32"),
            dwFlags: STARTF_USESTDHANDLES,
            hStdInput: inherited[0],
            hStdOutput: inherited[1],
            hStdError: inherited[2],
            ..STARTUPINFOW::default()
        },
        lpAttributeList: attributes.as_ptr(),
    };
    let mut information = PROCESS_INFORMATION::default();
    // SAFETY: application/directory/env/cmd are NUL-terminated UTF-16.
    // STARTUPINFOW.cb is set. Handles in STARTUPINFOW are live inheritable
    // pipe/NUL handles. On FALSE, no process handles are returned.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            cmd.as_mut_ptr(),
            null(),
            null(),
            1,
            flags,
            env_block.as_ptr().cast(),
            directory.as_ptr(),
            (&raw const startup).cast::<STARTUPINFOW>(),
            &raw mut information,
        )
    };
    drop(stdin);
    drop(stdout_write);
    drop(stderr_write);
    if created == 0 {
        return Err(ToolError::Execution(format!(
            "failed to spawn {}: {}",
            pinned.canonical_path.display(),
            std::io::Error::last_os_error()
        ))
        .into());
    }
    // SAFETY: CreateProcessW succeeded; both handles are uniquely owned.
    let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
    let thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
    let mut suspended = SuspendedProcess::new(process, thread);

    if let Err(err) = validate_suspended(suspended.process(), information.dwProcessId) {
        return Err(suspended.fail(err));
    }

    let job = match WindowsJob::new() {
        Ok(job) => job,
        Err(err) => {
            return Err(suspended.fail(ToolError::Execution(format!(
                "failed to create the child Job Object: {err}"
            ))));
        }
    };
    if let Err(err) = job.assign_handle(suspended.process().as_raw_handle()) {
        return Err(suspended.fail_classified(
            ToolError::Execution(format!(
                "failed to enroll the suspended child in its dedicated Job Object: {err}"
            )),
            nested_job_enrollment_kind(&err),
        ));
    }
    suspended.set_job(job);
    if let Err(err) = verify_process_image(suspended.process(), pinned, gate) {
        return Err(suspended.fail(err));
    }
    if let Err(err) = gate.check_pending() {
        return Err(suspended.fail(err));
    }
    if let Err(err) = resume_thread_handle(suspended.thread()) {
        return Err(suspended.fail(ToolError::Execution(format!(
            "failed to resume the enrolled child: {err}"
        ))));
    }
    gate.mark_launched();
    Ok(suspended.into_spawned(stdout_read, stderr_read))
}

fn validate_suspended(process: &OwnedHandle, created_pid: u32) -> Result<(), ToolError> {
    // SAFETY: `process` is the live CreateProcessW process handle.
    let handle_pid = unsafe { GetProcessId(process.as_raw_handle()) };
    if handle_pid == 0 {
        return Err(ToolError::Execution(format!(
            "failed to read the suspended process id: {}",
            std::io::Error::last_os_error()
        )));
    }
    if handle_pid != created_pid {
        return Err(ToolError::Execution(format!(
            "suspended process handle identifies {handle_pid}, but CreateProcessW returned {created_pid}"
        )));
    }
    // SAFETY: zero timeout only observes termination of this owned process.
    let wait_status = unsafe { WaitForSingleObject(process.as_raw_handle(), 0) };
    if wait_status == WAIT_FAILED {
        return Err(ToolError::Execution(format!(
            "failed to observe the suspended process: {}",
            std::io::Error::last_os_error()
        )));
    }
    if wait_status != WAIT_TIMEOUT {
        return Err(ToolError::Execution(
            "suspended child terminated before Job enrollment".into(),
        ));
    }
    Ok(())
}

fn verify_process_image(
    process: &OwnedHandle,
    pinned: &PinnedImage,
    gate: &SpawnGate,
) -> Result<(), ToolError> {
    let image_path = query_image_path(process)?;
    let image_file = open_image_for_compare(&image_path)?;
    // SAFETY: both handles are live files opened by this process.
    let same =
        unsafe { CompareObjectHandles(pinned.file.as_raw_handle(), image_file.as_raw_handle()) };
    let image_identity = identity_of(&image_file)?;
    if same == 0 && image_identity != pinned.identity {
        return Err(ToolError::Execution(
            "process image identity does not match the retained executable \
             (the path was replaced before verification; the child was reaped)"
                .into(),
        ));
    }
    let mut pin = pinned.file.try_clone().map_err(|err| {
        ToolError::Execution(format!(
            "failed to clone the pinned executable handle: {err}"
        ))
    })?;
    let digest = rehash_image_cancellable(&mut pin, || gate.check_pending())?;
    if digest != pinned.digest {
        return Err(ToolError::Execution(
            "pinned executable digest changed before verification \
             (a same-account writer rewrote the file; the child was reaped)"
                .into(),
        ));
    }
    Ok(())
}

fn query_image_path(process: &OwnedHandle) -> Result<OsString, ToolError> {
    let mut buf = vec![0_u16; 512];
    loop {
        let mut size = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        // SAFETY: `process` is live; `buf` is writable UTF-16 storage; `size`
        // is the documented in/out length. FALSE + ERROR_INSUFFICIENT_BUFFER
        // is the documented grow path.
        let ok = unsafe {
            QueryFullProcessImageNameW(
                process.as_raw_handle(),
                PROCESS_NAME_WIN32,
                buf.as_mut_ptr(),
                &raw mut size,
            )
        };
        if ok != 0 {
            buf.truncate(size as usize);
            return Ok(OsString::from_wide(&buf));
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
            return Err(ToolError::Execution(format!(
                "failed to query the process image path: {err}"
            )));
        }
        let needed = (size as usize)
            .saturating_add(1)
            .max(buf.len().saturating_mul(2));
        buf.resize(needed, 0);
    }
}

fn open_image_for_compare(path: &std::ffi::OsStr) -> Result<File, ToolError> {
    let mut wide: Vec<u16> = path.encode_wide().collect();
    if wide.contains(&0) {
        return Err(ToolError::Execution(
            "process image path contains an interior NUL".into(),
        ));
    }
    wide.push(0);
    // SAFETY: `wide` is NUL-terminated UTF-16. Share-read matches the pin.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(ToolError::Execution(format!(
            "failed to reopen the process image: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: CreateFileW returned a fresh owned HANDLE.
    Ok(File::from(unsafe { OwnedHandle::from_raw_handle(raw) }))
}

fn inheritable_pipe() -> Result<(OwnedHandle, OwnedHandle), ToolError> {
    let mut sa = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .expect("SECURITY_ATTRIBUTES fits u32"),
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let mut read = null_mut();
    let mut write = null_mut();
    // SAFETY: `sa.nLength` is set; both out-handles are writable HANDLE slots.
    let ok = unsafe { CreatePipe(&raw mut read, &raw mut write, &raw mut sa, 0) };
    if ok == 0 {
        return Err(ToolError::Execution(format!(
            "failed to create stdio pipes: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: CreatePipe returned two uniquely owned handles.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}

fn clear_inherit(handle: &OwnedHandle) -> Result<(), ToolError> {
    // SAFETY: `handle` is live; clearing HANDLE_FLAG_INHERIT is documented.
    let ok = unsafe { SetHandleInformation(handle.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) };
    if ok == 0 {
        Err(ToolError::Execution(format!(
            "failed to clear handle inherit: {}",
            std::io::Error::last_os_error()
        )))
    } else {
        Ok(())
    }
}

fn open_nul() -> Result<OwnedHandle, ToolError> {
    let mut wide: Vec<u16> = OsString::from(r"\\.\NUL").encode_wide().collect();
    wide.push(0);
    let mut sa = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .expect("SECURITY_ATTRIBUTES fits u32"),
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    // SAFETY: `wide` is NUL-terminated UTF-16 for the NUL device.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &raw mut sa,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(ToolError::Execution(format!(
            "failed to open NUL: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: CreateFileW returned a fresh owned HANDLE.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// Returns an equivalent non-verbatim spelling when legacy parsing is safe.
///
/// PowerShell derives `PSHOME` from the `lpApplicationName` spelling. A local
/// `\\?\C:\...` spelling is then mistaken for UNC during module discovery.
/// Canonical identity remains on [`PinnedImage`]; only the string passed to
/// `CreateProcessW` changes, and the created process handle is still verified.
fn win32_launch_path(path: &Path) -> Cow<'_, OsStr> {
    const VERBATIM_DOS_PREFIX: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];

    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    let Some(candidate) = wide.strip_prefix(VERBATIM_DOS_PREFIX) else {
        return Cow::Borrowed(path.as_os_str());
    };
    if candidate.len().saturating_add(1) > MAX_PATH_UTF16_UNITS_WITH_NUL
        || !legacy_dos_path_is_equivalent(candidate)
    {
        return Cow::Borrowed(path.as_os_str());
    }
    Cow::Owned(OsString::from_wide(candidate))
}

fn legacy_dos_path_is_equivalent(path: &[u16]) -> bool {
    if path.len() < 4
        || !(u16::from(b'A')..=u16::from(b'Z')).contains(&path[0])
            && !(u16::from(b'a')..=u16::from(b'z')).contains(&path[0])
        || path[1] != b':' as u16
        || path[2] != b'\\' as u16
    {
        return false;
    }
    path[3..].split(|unit| *unit == b'\\' as u16).all(|component| {
        !component.is_empty()
            && !matches!(component.last(), Some(unit) if *unit == b' ' as u16 || *unit == b'.' as u16)
            && !component.iter().any(|unit| {
                *unit < b' ' as u16
                    || matches!(
                        *unit,
                        unit if unit == b'/' as u16
                            || unit == b':' as u16
                            || unit == b'*' as u16
                            || unit == b'?' as u16
                            || unit == b'"' as u16
                            || unit == b'<' as u16
                            || unit == b'>' as u16
                            || unit == b'|' as u16
                    )
            })
            && !is_reserved_dos_component(component)
    })
}

fn is_reserved_dos_component(component: &[u16]) -> bool {
    let stem = component
        .split(|unit| *unit == b'.' as u16)
        .next()
        .unwrap_or_default();
    ["CON", "PRN", "AUX", "NUL", "CLOCK$", "CONIN$", "CONOUT$"]
        .iter()
        .any(|name| utf16_eq_ascii_ignore_case(stem, name.as_bytes()))
        || (stem.len() == 4
            && (utf16_eq_ascii_ignore_case(&stem[..3], b"COM")
                || utf16_eq_ascii_ignore_case(&stem[..3], b"LPT"))
            && matches!(stem[3], unit if (b'1' as u16..=b'9' as u16).contains(&unit) || matches!(unit, 0x00B9 | 0x00B2 | 0x00B3)))
}

fn utf16_eq_ascii_ignore_case(wide: &[u16], ascii: &[u8]) -> bool {
    wide.len() == ascii.len()
        && wide.iter().zip(ascii).all(|(unit, byte)| {
            *unit == u16::from(byte.to_ascii_lowercase())
                || *unit == u16::from(byte.to_ascii_uppercase())
        })
}

fn wide_os(value: &std::ffi::OsStr) -> Result<Vec<u16>, ToolError> {
    let mut wide: Vec<u16> = value.encode_wide().collect();
    if wide.contains(&0) {
        return Err(ToolError::InvalidArgs(
            "path contains an interior NUL".into(),
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn unicode_env_block(entries: &[(OsString, OsString)]) -> Vec<u16> {
    let mut block = Vec::new();
    for (key, value) in entries {
        block.extend(key.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    if block.len() < 2 {
        block = vec![0, 0];
    }
    block
}

fn terminate(process: &impl AsRawHandle) -> std::io::Result<()> {
    let handle = process.as_raw_handle();
    // SAFETY: `handle` is a live process handle with PROCESS_TERMINATE access.
    if unsafe { TerminateProcess(handle, 1) } != 0 {
        return Ok(());
    }
    let terminate_error = std::io::Error::last_os_error();
    // TerminateProcess reports access denied after a process has terminated.
    // A signaled process handle proves that cleanup may continue to its wait.
    // SAFETY: `handle` is a live process handle with SYNCHRONIZE access; a zero
    // timeout only observes its current signaled state.
    if unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err(terminate_error)
    }
}

fn duplicate_handle(handle: &OwnedHandle) -> std::io::Result<OwnedHandle> {
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut duplicated = null_mut();
    // SAFETY: source is live; current-process pseudo-handles are not closed.
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            handle.as_raw_handle(),
            GetCurrentProcess(),
            &raw mut duplicated,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: DuplicateHandle returned a fresh owned HANDLE.
        Ok(unsafe { OwnedHandle::from_raw_handle(duplicated) })
    }
}

fn wait_process(process: &OwnedHandle) -> std::io::Result<()> {
    wait_exit_code(duplicate_handle(process)?).map(|_| ())
}

fn wait_exit_code(handle: OwnedHandle) -> std::io::Result<u32> {
    // SAFETY: `handle` is a duplicated process handle with SYNCHRONIZE.
    let status = unsafe { WaitForSingleObject(handle.as_raw_handle(), INFINITE) };
    if status == WAIT_FAILED {
        return Err(std::io::Error::last_os_error());
    }
    let mut code = 0_u32;
    // SAFETY: the process has signaled; `code` is writable documented storage.
    if unsafe { GetExitCodeProcess(handle.as_raw_handle(), &raw mut code) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(code)
}
