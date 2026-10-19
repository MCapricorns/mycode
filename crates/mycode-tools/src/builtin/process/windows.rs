//! Windows Job Object enrollment for suspended children.
//!
//! A child is created with `CREATE_SUSPENDED`, assigned to a dedicated
//! kill-on-close Job, and only then resumed. The Job does not grant
//! breakaway, so descendants stay in it when they call `CreateProcess`,
//! including Windows stand-ins for a detached Unix child. `TerminateJobObject`
//! ends that whole tree.
//!
//! Resume uses `PROCESS_INFORMATION.hThread` from that `CreateProcessW` call.
//! A Toolhelp snapshot is not a substitute: antivirus software can inject a
//! thread before the snapshot, so a "exactly one thread" check rejects a
//! still-suspended primary thread.
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::os::windows::process::ExitStatusExt as _;
use std::path::Path;
use std::process::ExitStatus;
use std::ptr::{null, null_mut};

use super::first_program_on_path;
use crate::builtin::exec::{
    is_batch_script, windows_batch_command_line_utf16, windows_command_line_utf16,
};

/// RAII owner for a Windows Job Object configured to kill all members when its
/// last handle closes. `OwnedHandle` provides the matching `CloseHandle`.
pub(crate) struct WindowsJob {
    handle: OwnedHandle,
}

impl WindowsJob {
    /// Creates an unnamed kill-on-close Job Object.
    ///
    /// # Errors
    ///
    /// Returns the `CreateJobObjectW` or `SetInformationJobObject` error.
    pub(crate) fn new() -> std::io::Result<Self> {
        use std::mem::size_of;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        // SAFETY: both optional pointers are null, requesting an unnamed,
        // non-inheritable Job Object. A non-null result is uniquely owned.
        let raw_job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw_job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a fresh owned HANDLE. OwnedHandle
        // closes it exactly once with CloseHandle on every later error/drop.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw_job) };
        let job = Self { handle };

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let info_len = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .expect("Job Object limit structure size fits u32");
        // SAFETY: the Job handle is live, `info` has the exact documented type
        // and byte length, and the API borrows it only for this call.
        let configured = unsafe {
            SetInformationJobObject(
                job.raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                info_len,
            )
        };
        if configured == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Assigns a still-suspended process to this Job.
    ///
    /// # Errors
    ///
    /// Returns the `AssignProcessToJobObject` error.
    pub(crate) fn assign_handle(
        &self,
        process_handle: windows_sys::Win32::Foundation::HANDLE,
    ) -> std::io::Result<()> {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        // SAFETY: both are borrowed, live kernel handles for the duration of
        // this call. The Job owns neither the process handle nor vice versa.
        if unsafe { AssignProcessToJobObject(self.raw_handle(), process_handle) } != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Terminates every process currently in the Job.
    ///
    /// # Errors
    ///
    /// Returns the `TerminateJobObject` error, including an invalid handle.
    pub(crate) fn terminate(&self) -> std::io::Result<()> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        // SAFETY: the owned Job handle remains live for this call.
        if unsafe { TerminateJobObject(self.raw_handle(), 1) } != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn raw_handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.handle.as_raw_handle()
    }
}

/// Returns whether the calling process is already inside any Job Object.
///
/// # Errors
///
/// Returns the `IsProcessInJob` error.
pub(crate) fn current_process_is_in_job() -> std::io::Result<bool> {
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut in_job = 0;
    // SAFETY: GetCurrentProcess returns a borrowed pseudo-handle. A null Job
    // handle asks about any containing Job, and `in_job` is valid output storage.
    let queried =
        unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &raw mut in_job) };
    if queried == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(in_job != 0)
    }
}

/// Resumes a thread that this process created suspended.
///
/// `thread` must be the `hThread` from `CreateProcessW` for that child, not a
/// handle looked up by pid. The previous suspend count must be exactly 1.
///
/// # Errors
///
/// Returns an error when `ResumeThread` fails or the suspend count is not 1.
pub(crate) fn resume_thread_handle(thread: &OwnedHandle) -> std::io::Result<()> {
    use windows_sys::Win32::System::Threading::ResumeThread;

    // SAFETY: `thread` is a live handle with THREAD_SUSPEND_RESUME from
    // CreateProcessW. ResumeThread borrows it only for this call.
    let previous_count = unsafe { ResumeThread(thread.as_raw_handle()) };
    if previous_count == u32::MAX {
        return Err(std::io::Error::last_os_error());
    }
    if previous_count != 1 {
        return Err(std::io::Error::other(format!(
            "suspended child had unexpected suspend count {previous_count}"
        )));
    }
    Ok(())
}

/// A suspended-child guard. Drop terminates the process unless disarmed after
/// the main thread has been resumed and the caller owns the handles.
struct UnresumedChild {
    process: Option<OwnedHandle>,
    thread: OwnedHandle,
    job: Option<WindowsJob>,
    armed: bool,
}

impl Drop for UnresumedChild {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(job) = &self.job {
            let _ = job.terminate();
        }
        if let Some(process) = &self.process {
            let _ = terminate_process(process);
        }
    }
}

/// Stdio child started with `CreateProcessW` while suspended.
///
/// The main thread is `PROCESS_INFORMATION.hThread` from that call. It is
/// resumed only after the process is inside a kill-on-close Job. Pipe ends
/// the parent keeps are tokio stdio types so callers can read and write them
/// the same way as `tokio::process::Child`.
pub struct WindowsStdioProcess {
    process: OwnedHandle,
    pid: u32,
    /// Parent write end when stdin was requested as a pipe.
    pub stdin: Option<tokio::process::ChildStdin>,
    /// Parent read end of the stdout pipe.
    pub stdout: Option<tokio::process::ChildStdout>,
    /// Parent read end of the stderr pipe.
    pub stderr: Option<tokio::process::ChildStderr>,
    job: Option<WindowsJob>,
    kill_on_drop: bool,
    exit: Option<ExitStatus>,
}

impl WindowsStdioProcess {
    /// Spawns `program` suspended, enrolls it in a kill-on-close Job, then
    /// resumes `PROCESS_INFORMATION.hThread`.
    ///
    /// A program without a directory is resolved against `PATH` in `env`, then
    /// the parent process `PATH`. A bare name is tried with each launchable
    /// `PATHEXT` extension in `PATHEXT` order (default `.com`, `.exe`, `.bat`,
    /// `.cmd`) in each directory before the next, so `npx` resolves to
    /// `npx.cmd`.
    /// A non-batch hit is passed as `lpApplicationName`. When neither search
    /// finds a file, `lpApplicationName` is null and `CreateProcessW` searches
    /// the parent process `PATH`, not the environment block. A non-batch
    /// program that already contains a directory is passed as
    /// `lpApplicationName` and is not searched. `.bat` and `.cmd` files are
    /// launched through `cmd.exe` with batch escaping, including `%` in the
    /// script path. Only the child's stdio handles are inheritable. stdout and
    /// stderr are pipes. `stdin_piped` selects a pipe or the NUL device.
    ///
    /// # Errors
    ///
    /// Returns an error when the command line, pipes, `CreateProcessW`, Job
    /// assignment, or resume fails. A process that was created is terminated
    /// before the error returns, so a suspended child is not left running.
    pub fn spawn(
        program: &Path,
        args: &[String],
        cwd: Option<&Path>,
        env: &[(OsString, OsString)],
        stdin_piped: bool,
        kill_on_drop: bool,
    ) -> io::Result<Self> {
        use std::mem::size_of;
        use windows_sys::Win32::System::Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            EXTENDED_STARTUPINFO_PRESENT, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
            STARTUPINFOEXW, STARTUPINFOW,
        };

        let resolved = if program_has_directory(program) {
            None
        } else {
            resolve_bare_program(program, env)
        };
        let launch = resolved.as_deref().unwrap_or(program);
        let batch = is_batch_script(launch.as_os_str());
        let mut command = if batch {
            windows_batch_command_line_utf16(launch.as_os_str(), args)
        } else {
            windows_command_line_utf16(program.as_os_str(), args)
        }
        .map_err(|error| io::Error::other(error.to_string()))?;
        command.push(0);
        let application = if batch {
            Some(command_prompt()?)
        } else if program_has_directory(launch) {
            Some(wide_nul(launch.as_os_str())?)
        } else {
            None
        };
        let directory = match cwd {
            Some(cwd) => Some(wide_nul(cwd.as_os_str())?),
            None => None,
        };
        let env_block = unicode_env_block(env);

        let (stdin_parent, stdin_child) = if stdin_piped {
            let (read, write) = inheritable_pipe()?;
            clear_inherit(&write)?;
            (Some(write), read)
        } else {
            (None, open_nul()?)
        };
        let (stdout_read, stdout_write) = inheritable_pipe()?;
        let (stderr_read, stderr_write) = inheritable_pipe()?;
        clear_inherit(&stdout_read)?;
        clear_inherit(&stderr_read)?;

        let inherited = [
            stdin_child.as_raw_handle(),
            stdout_write.as_raw_handle(),
            stderr_write.as_raw_handle(),
        ];
        let attributes = ProcThreadAttributeList::with_handles(&inherited)?;
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
        let application_ptr = application.as_ref().map_or(null(), |wide| wide.as_ptr());
        let directory_ptr = directory.as_ref().map_or(null(), |wide| wide.as_ptr());
        // SAFETY: `command` is NUL-terminated UTF-16. `application` and
        // `directory`, when present, are too. The environment block is
        // double-NUL terminated. The attribute list names the only three
        // handles the child may inherit, and those handles are live for this
        // call. On FALSE, no process handles are returned.
        let created = unsafe {
            CreateProcessW(
                application_ptr,
                command.as_mut_ptr(),
                null(),
                null(),
                1,
                CREATE_UNICODE_ENVIRONMENT
                    | CREATE_NO_WINDOW
                    | CREATE_SUSPENDED
                    | EXTENDED_STARTUPINFO_PRESENT,
                env_block.as_ptr().cast(),
                directory_ptr,
                (&raw const startup).cast(),
                &raw mut information,
            )
        };
        drop(attributes);
        drop(stdin_child);
        drop(stdout_write);
        drop(stderr_write);
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateProcessW succeeded. Both handles are uniquely owned.
        let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
        let thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
        let mut pending = UnresumedChild {
            process: Some(process),
            thread,
            job: None,
            armed: true,
        };
        if information.dwProcessId == 0 {
            return Err(io::Error::other(
                "CreateProcessW returned a zero process id",
            ));
        }
        let job = WindowsJob::new()?;
        let process_handle = pending
            .process
            .as_ref()
            .expect("suspended process handle")
            .as_raw_handle();
        job.assign_handle(process_handle)?;
        pending.job = Some(job);
        resume_thread_handle(&pending.thread)?;
        let process = pending.process.take().expect("suspended process handle");
        let job = pending.job.take();
        pending.armed = false;
        drop(pending);
        let reap = |error: io::Error| {
            if let Some(job) = &job {
                let _ = job.terminate();
            }
            let _ = terminate_process(&process);
            error
        };
        let stdin = stdin_parent.map(adopt_stdin).transpose().map_err(reap)?;
        let stdout = Some(adopt_stdout(stdout_read).map_err(reap)?);
        let stderr = Some(adopt_stderr(stderr_read).map_err(reap)?);
        Ok(Self {
            process,
            pid: information.dwProcessId,
            stdin,
            stdout,
            stderr,
            job,
            kill_on_drop,
            exit: None,
        })
    }

    /// Process id while the child has not been observed to exit.
    #[must_use]
    pub fn id(&self) -> Option<u32> {
        use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::WaitForSingleObject;

        if self.exit.is_some() {
            return None;
        }
        // SAFETY: `process` is a live process handle. A zero timeout only
        // observes whether it is already signaled.
        let waited = unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) };
        if waited == WAIT_TIMEOUT {
            Some(self.pid)
        } else if waited == WAIT_OBJECT_0 {
            None
        } else {
            Some(self.pid)
        }
    }

    /// Ends the kill-on-close Job, including descendants.
    ///
    /// # Errors
    ///
    /// Returns the `TerminateJobObject` error when the Job is still open.
    pub fn terminate_job(&self) -> io::Result<()> {
        match &self.job {
            Some(job) => job.terminate(),
            None => Ok(()),
        }
    }

    /// Terminates the process. An already-exited process is success.
    ///
    /// # Errors
    ///
    /// Returns the `TerminateProcess` error when the process is still running.
    pub fn start_kill(&mut self) -> io::Result<()> {
        terminate_process(&self.process)
    }

    /// Waits until the process exits, then returns its status.
    ///
    /// # Errors
    ///
    /// Returns an error when the wait or the exit-code query fails.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        use windows_sys::Win32::Foundation::WAIT_FAILED;
        use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};

        if let Some(status) = self.exit {
            return Ok(status);
        }
        let process = self.process.try_clone()?;
        let status = tokio::task::spawn_blocking(move || {
            // SAFETY: `process` is a live owned handle. `INFINITE` waits until
            // the process object is signaled.
            let waited = unsafe { WaitForSingleObject(process.as_raw_handle(), INFINITE) };
            if waited == WAIT_FAILED {
                return Err(io::Error::last_os_error());
            }
            exit_status(&process)
        })
        .await
        .map_err(|error| io::Error::other(format!("process wait task failed: {error}")))??;
        self.exit = Some(status);
        Ok(status)
    }

    /// Terminates the process and waits for it to exit.
    ///
    /// # Errors
    ///
    /// Returns an error when terminate or wait fails.
    pub async fn kill(&mut self) -> io::Result<()> {
        self.start_kill()?;
        self.wait().await.map(|_| ())
    }

    /// Exit status when the process has already exited.
    ///
    /// # Errors
    ///
    /// Returns an error when the process handle cannot be queried.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::WaitForSingleObject;

        if let Some(status) = self.exit {
            return Ok(Some(status));
        }
        // SAFETY: `process` is a live process handle. A zero timeout does not
        // block.
        let waited = unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) };
        if waited == WAIT_TIMEOUT {
            return Ok(None);
        }
        if waited != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let status = exit_status(&self.process)?;
        self.exit = Some(status);
        Ok(Some(status))
    }
}

impl Drop for WindowsStdioProcess {
    fn drop(&mut self) {
        if !self.kill_on_drop {
            return;
        }
        if let Some(job) = &self.job {
            let _ = job.terminate();
        }
        let _ = terminate_process(&self.process);
    }
}

fn adopt_stdin(handle: OwnedHandle) -> io::Result<tokio::process::ChildStdin> {
    tokio::process::ChildStdin::from_std(std::process::ChildStdin::from(handle))
}

fn adopt_stdout(handle: OwnedHandle) -> io::Result<tokio::process::ChildStdout> {
    tokio::process::ChildStdout::from_std(std::process::ChildStdout::from(handle))
}

fn adopt_stderr(handle: OwnedHandle) -> io::Result<tokio::process::ChildStderr> {
    tokio::process::ChildStderr::from_std(std::process::ChildStderr::from(handle))
}

fn program_has_directory(program: &Path) -> bool {
    use std::path::Component;
    program.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir
        )
    }) || program
        .parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty())
}

fn wide_nul(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = value.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains an interior NUL",
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

fn inheritable_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    use std::mem::size_of;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Pipes::CreatePipe;

    let mut sa = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .expect("SECURITY_ATTRIBUTES fits u32"),
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let mut read = null_mut();
    let mut write = null_mut();
    // SAFETY: `sa.nLength` is set. Both out-handles are writable HANDLE slots.
    let ok = unsafe { CreatePipe(&raw mut read, &raw mut write, &raw mut sa, 0) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreatePipe returned two uniquely owned handles.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}

fn clear_inherit(handle: &OwnedHandle) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};

    // SAFETY: `handle` is live. Clearing HANDLE_FLAG_INHERIT is documented.
    let ok = unsafe { SetHandleInformation(handle.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn open_nul() -> io::Result<OwnedHandle> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let mut wide: Vec<u16> = OsStr::new(r"\\.\NUL").encode_wide().collect();
    wide.push(0);
    let mut sa = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .expect("SECURITY_ATTRIBUTES fits u32"),
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    // SAFETY: `wide` is NUL-terminated UTF-16 for the NUL device. The
    // security attributes request an inheritable read handle.
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
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a fresh owned HANDLE.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

fn exit_status(process: &OwnedHandle) -> io::Result<ExitStatus> {
    use windows_sys::Win32::System::Threading::GetExitCodeProcess;

    let mut code = 0u32;
    // SAFETY: `process` is a live process handle and `code` is writable output.
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &raw mut code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ExitStatus::from_raw(code))
}

fn terminate_process(process: &OwnedHandle) -> io::Result<()> {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};

    // SAFETY: `process` is a live process handle with PROCESS_TERMINATE.
    if unsafe { TerminateProcess(process.as_raw_handle(), 1) } != 0 {
        return Ok(());
    }
    let terminate_error = io::Error::last_os_error();
    // TerminateProcess reports access denied after the process has exited.
    // A signaled process handle means there is nothing left to kill.
    // SAFETY: `process` is live and has SYNCHRONIZE. A zero timeout only
    // observes the current signaled state.
    if unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err(terminate_error)
    }
}

/// Inherited-handle list so a concurrent spawn cannot leak pipe ends into
/// this child. The storage must outlive `CreateProcessW`.
struct ProcThreadAttributeList {
    list: windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST,
    _storage: Vec<usize>,
}

impl ProcThreadAttributeList {
    fn with_handles(handles: &[windows_sys::Win32::Foundation::HANDLE]) -> io::Result<Self> {
        use std::mem::size_of;
        use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
        use windows_sys::Win32::System::Threading::{
            InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            UpdateProcThreadAttribute,
        };

        let mut bytes = 0_usize;
        // SAFETY: a null list is the documented sizing call. `bytes` is valid
        // output storage. The call fails with ERROR_INSUFFICIENT_BUFFER.
        let sized = unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &raw mut bytes) };
        let sizing_error = io::Error::last_os_error();
        if sized != 0
            || bytes == 0
            || sizing_error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        {
            return Err(io::Error::other(format!(
                "failed to size the inherited-handle attribute list: {sizing_error}"
            )));
        }
        let units = bytes.div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; units];
        let list = storage.as_mut_ptr().cast();
        // SAFETY: `storage` is aligned, writable, and at least `bytes` long.
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &raw mut bytes) } == 0 {
            return Err(io::Error::other(format!(
                "failed to initialize the inherited-handle attribute list: {}",
                io::Error::last_os_error()
            )));
        }
        let attributes = Self {
            list,
            _storage: storage,
        };
        // SAFETY: `attributes.list` is initialized for one attribute.
        // `handles` stays live through CreateProcessW. The byte count is the
        // size of the HANDLE array.
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
            return Err(io::Error::other(format!(
                "failed to restrict inherited child handles: {}",
                io::Error::last_os_error()
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
        use windows_sys::Win32::System::Threading::DeleteProcThreadAttributeList;
        // SAFETY: `list` was initialized once and is deleted exactly once
        // before its backing storage is released.
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

/// `System32\cmd.exe`, NUL-terminated, for `.bat` / `.cmd` launchers.
fn command_prompt() -> io::Result<Vec<u16>> {
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buf = vec![0u16; 261];
    // SAFETY: `buf` is writable for its length. A large enough buffer receives
    // a NUL-terminated system directory; the return value then excludes that
    // NUL. A too-small buffer returns the required size including the NUL.
    let written = unsafe {
        GetSystemDirectoryW(
            buf.as_mut_ptr(),
            u32::try_from(buf.len()).unwrap_or(u32::MAX),
        )
    };
    if written == 0 {
        return Err(io::Error::last_os_error());
    }
    let written = usize::try_from(written).unwrap_or(usize::MAX);
    if written >= buf.len() {
        buf.resize(written, 0);
        let written = unsafe {
            GetSystemDirectoryW(
                buf.as_mut_ptr(),
                u32::try_from(buf.len()).unwrap_or(u32::MAX),
            )
        };
        if written == 0 || usize::try_from(written).unwrap_or(usize::MAX) >= buf.len() {
            return Err(io::Error::last_os_error());
        }
        buf.truncate(usize::try_from(written).unwrap_or(0));
    } else {
        buf.truncate(written);
    }
    buf.extend("\\cmd.exe".encode_utf16());
    buf.push(0);
    Ok(buf)
}

/// First existing file for a bare program name.
///
/// Child `PATH` from `env` is searched before the parent process `PATH`.
/// A name with no `.` is tried with each launchable extension in `PATHEXT`
/// order (default `.com`, `.exe`, `.bat`, `.cmd`), so `npx` resolves to
/// `npx.cmd` when that is the file on disk.
fn resolve_bare_program(
    program: &Path,
    env: &[(OsString, OsString)],
) -> Option<std::path::PathBuf> {
    let name = program.file_name()?;
    let pathext = pathext_value(env);
    let mut paths = Vec::new();
    if let Some(value) = env_os(env, "PATH") {
        paths.push(value.to_os_string());
    }
    if let Some(parent) = std::env::var_os("PATH") {
        paths.push(parent);
    }
    for path_var in paths {
        if let Some(found) = first_program_on_path(&name.to_string_lossy(), &path_var, &pathext) {
            return Some(found);
        }
    }
    None
}

fn pathext_value(env: &[(OsString, OsString)]) -> String {
    if let Some(value) = env_os(env, "PATHEXT") {
        return value.to_string_lossy().into_owned();
    }
    std::env::var("PATHEXT").unwrap_or_default()
}

fn env_os<'a>(env: &'a [(OsString, OsString)], key: &str) -> Option<&'a OsStr> {
    env.iter()
        .find(|(name, _)| name.to_string_lossy().eq_ignore_ascii_case(key))
        .map(|(_, value)| value.as_os_str())
}

#[cfg(test)]
mod resolve_tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::resolve_bare_program;

    static SCRATCH: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("mycode-bare-{label}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"").expect("touch");
    }

    fn env_with(path: &Path, pathext: &str) -> Vec<(OsString, OsString)> {
        vec![
            (OsString::from("PATH"), path.as_os_str().to_os_string()),
            (OsString::from("PATHEXT"), OsString::from(pathext)),
        ]
    }

    #[test]
    fn bare_npx_resolves_to_npx_cmd() {
        let scratch = Scratch::new("cmd");
        touch(&scratch.0, "npx.cmd");
        let found =
            resolve_bare_program(Path::new("npx"), &env_with(&scratch.0, "")).expect("npx.cmd");
        assert_eq!(found, scratch.0.join("npx.cmd"));
    }

    #[test]
    fn pathext_order_picks_exe_before_cmd_and_honors_a_cmd_first_list() {
        let scratch = Scratch::new("order");
        touch(&scratch.0, "npx.exe");
        touch(&scratch.0, "npx.cmd");
        let exe = resolve_bare_program(
            Path::new("npx"),
            &env_with(&scratch.0, ".COM;.EXE;.BAT;.CMD"),
        )
        .expect("exe");
        assert_eq!(exe, scratch.0.join("npx.exe"));
        let cmd = resolve_bare_program(Path::new("npx"), &env_with(&scratch.0, ".CMD;.EXE"))
            .expect("cmd");
        assert_eq!(cmd, scratch.0.join("npx.cmd"));
    }
}
