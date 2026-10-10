//! Windows Job Object enrollment for suspended children.
//!
//! A child is created with `CREATE_SUSPENDED`, assigned to a dedicated
//! kill-on-close Job, and only then resumed.
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

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
