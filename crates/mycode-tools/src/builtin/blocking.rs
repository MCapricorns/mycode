//! Blocking-worker runtime shared by search, file IO, and shell.
//!
//! Startup first proves a usable interrupt authority. A supervisor owns the
//! actual worker join and platform authority for the complete lifetime.
//! Cancellation, timeout, or future drop publishes the worker token; Unix
//! pollable reads wake through a per-worker socket, while `SIGURG` and Windows
//! `CancelSynchronousIo` cover syscalls already in the kernel.
use std::fs::File;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::builtin::fs_search::{
    PreparedSearch, SEARCH_TIME_LIMIT, SearchAccess, prepare_search_with_access,
};
use crate::tool::ToolError;

#[cfg(unix)]
mod signal;
#[cfg(unix)]
pub(crate) use signal::acquire_interrupt_signal;
#[cfg(unix)]
use signal::{current_sigurg_handler, our_sigurg_handler, unblock_interrupt_signal};

/// Pause between cancel interrupts while a worker is still joining.
///
/// Unix pollable reads also have a per-worker wake socket, so they do not
/// depend on `SIGURG`. The signal and Windows `CancelSynchronousIo` are
/// retried for syscalls already in the kernel. Uninterruptible kernel `D`
/// state (some NFS waits) can still outlive this loop.
const INTERRUPT_RETRY: Duration = Duration::from_millis(10);

/// Runs deadline-bounded search work on a dedicated OS thread.
///
/// Startup first proves a usable interrupt authority. A supervisor owns the
/// actual worker join and platform authority for the complete lifetime.
/// Cancellation, timeout, or future drop publishes the worker token; Unix
/// pollable reads wake through a per-worker socket, while `SIGURG` and Windows
/// `CancelSynchronousIo` cover syscalls already in the kernel. The supervisor
/// uniquely joins the worker before releasing platform resources. A native
/// in-process component that replaces `SIGURG` during an active call can
/// defeat interruption of a non-pollable Unix syscall; uninterruptible kernel
/// waits can also delay the supervisor.
pub(crate) async fn run_blocking<F, T>(
    label: &str,
    cancel: &CancellationToken,
    time_limit: Duration,
    function: F,
) -> Result<T, ToolError>
where
    F: FnOnce(CancellationToken) -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    run_blocking_until(label, cancel, Instant::now() + time_limit, function).await
}

/// [`run_blocking`] driven by a shared absolute deadline.
pub(crate) async fn run_blocking_until<F, T>(
    label: &str,
    cancel: &CancellationToken,
    deadline: Instant,
    function: F,
) -> Result<T, ToolError>
where
    F: FnOnce(CancellationToken) -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    run_blocking_started(label, cancel, Some(deadline), function).await
}

/// Runs blocking work under a supervisor without an internal deadline.
///
/// Cancellation or future drop publishes the worker token. The detached
/// supervisor retains the unique worker join and interrupt authority.
///
/// # Errors
///
/// Returns worker, cancellation, startup, or interrupt-authority failures.
pub(crate) async fn run_blocking_supervised<F, T>(
    label: &str,
    cancel: &CancellationToken,
    function: F,
) -> Result<T, ToolError>
where
    F: FnOnce(CancellationToken) -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    run_blocking_started(label, cancel, None, function).await
}

/// Same as [`run_blocking`], with the deadline chosen by the caller
/// (`None` for no deadline).
///
/// # Errors
///
/// Same as [`run_blocking`].
pub(crate) async fn run_blocking_started<F, T>(
    label: &str,
    cancel: &CancellationToken,
    deadline: Option<Instant>,
    function: F,
) -> Result<T, ToolError>
where
    F: FnOnce(CancellationToken) -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    if cancel.is_cancelled() {
        return Err(ToolError::Execution(format!(
            "{label} cancelled before completion"
        )));
    }
    let worker_cancel = cancel.child_token();
    let function_cancel = worker_cancel.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    // The spawn handshake (`startup_rx.recv`) is a blocking wait on the
    // supervisor thread; running it inline would stall the calling runtime
    // whenever thread startup is slow.
    let spawn_cancel = worker_cancel.clone();
    let worker = tokio::task::spawn_blocking(move || {
        InterruptibleWorker::spawn(
            move || {
                if function_cancel.is_cancelled() {
                    return Err(ToolError::Execution(
                        "cancelled before completion".to_owned(),
                    ));
                }
                function(function_cancel)
            },
            tx,
            spawn_cancel,
        )
    })
    .await
    .map_err(|error| {
        ToolError::Execution(format!("{label} worker spawn task failed: {error}"))
    })??;
    let deadline_wait = async {
        match deadline {
            Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(deadline_wait);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            worker_cancel.cancel();
            let authority_error = worker.join().await;
            let suffix = authority_error
                .map(|error| format!("; interrupt error: {error}"))
                .unwrap_or_default();
            Err(ToolError::Execution(format!(
                "{label} cancelled before completion{suffix}"
            )))
        },
        _ = &mut deadline_wait => {
            worker_cancel.cancel();
            let authority_error = worker.join().await;
            let suffix = authority_error
                .map(|error| format!("; interrupt error: {error}"))
                .unwrap_or_default();
            Err(ToolError::Execution(format!("{label} time limit reached{suffix}")))
        },
        joined = rx => {
            let authority_error = worker.join().await;
            // A worker result and cancellation/deadline can become ready in
            // the same scheduler turn. Revalidate terminal state after the
            // unique join so a late-selected result never publishes partial
            // output after an already-observed stop condition.
            if cancel.is_cancelled() {
                let suffix = authority_error
                    .map(|error| format!("; interrupt error: {error}"))
                    .unwrap_or_default();
                return Err(ToolError::Execution(format!(
                    "{label} cancelled before completion{suffix}"
                )));
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                let suffix = authority_error
                    .map(|error| format!("; interrupt error: {error}"))
                    .unwrap_or_default();
                return Err(ToolError::Execution(format!(
                    "{label} time limit reached{suffix}"
                )));
            }
            if let Some(error) = authority_error {
                return Err(ToolError::Execution(format!(
                    "{label} interrupt authority failed: {error}"
                )));
            }
            match joined {
                Ok(inner) => inner,
                Err(_) => Err(ToolError::Execution(format!(
                    "{label} worker dropped its result"
                ))),
            }
        },
    }
}

/// Resolves a grep/find target on a cancellable worker thread.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] when the worker is cancelled or exceeds
/// the search time limit.
pub async fn prepare_search_async(
    cwd: std::path::PathBuf,
    path_arg: Option<String>,
    cancel: CancellationToken,
) -> Result<PreparedSearch, ToolError> {
    prepare_search_async_with_access(cwd, path_arg, cancel, SearchAccess::Content).await
}

/// [`prepare_search_async`] with an explicit content/metadata capability.
///
/// # Errors
///
/// Same as [`prepare_search_async`].
pub async fn prepare_search_async_with_access(
    cwd: std::path::PathBuf,
    path_arg: Option<String>,
    cancel: CancellationToken,
    access: SearchAccess,
) -> Result<PreparedSearch, ToolError> {
    run_blocking(
        "search dispatch preflight",
        &cancel,
        SEARCH_TIME_LIMIT,
        move |worker_cancel| {
            prepare_search_with_access(&cwd, path_arg.as_deref(), &worker_cancel, access)
        },
    )
    .await
}

/// Same as [`prepare_search_async`], with a per-call blocking I/O hook.
///
/// # Errors
///
/// Same as [`prepare_search_async`].
#[cfg(windows)]
struct SendHandle {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

// SAFETY: the value is a uniquely owned kernel handle integer. Only the
// supervisor thread uses it, and `Drop` closes it exactly once.
#[cfg(windows)]
unsafe impl Send for SendHandle {}

#[cfg(windows)]
impl Drop for SendHandle {
    fn drop(&mut self) {
        // SAFETY: this is the owned duplicate returned by `DuplicateHandle`.
        let closed = unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
        debug_assert_ne!(closed, 0, "owned worker thread handle must close");
    }
}

#[cfg(unix)]
std::thread_local! {
    static WORKER_WAKE_FD: std::cell::Cell<std::os::fd::RawFd> = const {
        std::cell::Cell::new(-1)
    };
}

#[cfg(unix)]
struct WorkerWake {
    reader: std::os::unix::net::UnixStream,
}

#[cfg(windows)]
struct WorkerWake;

#[cfg(not(any(unix, windows)))]
struct WorkerWake;

#[cfg(unix)]
struct WorkerWakeGuard {
    previous: std::os::fd::RawFd,
}

#[cfg(not(unix))]
struct WorkerWakeGuard;

#[cfg(unix)]
impl Drop for WorkerWakeGuard {
    fn drop(&mut self) {
        WORKER_WAKE_FD.with(|slot| slot.set(self.previous));
    }
}

impl WorkerWake {
    #[cfg(unix)]
    fn enter(&self) -> WorkerWakeGuard {
        use std::os::fd::AsRawFd;

        let previous = WORKER_WAKE_FD.with(|slot| slot.replace(self.reader.as_raw_fd()));
        WorkerWakeGuard { previous }
    }

    #[cfg(not(unix))]
    fn enter(&self) -> WorkerWakeGuard {
        WorkerWakeGuard
    }
}

/// Waits until a file descriptor is readable or the current worker is woken.
///
/// Outside a supervised Unix worker there is no wake descriptor, so the
/// caller proceeds directly. Windows uses `CancelSynchronousIo` instead.
pub(crate) fn wait_for_worker_readable(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        let wake = WORKER_WAKE_FD.with(std::cell::Cell::get);
        if wake < 0 {
            return Ok(());
        }
        let mut descriptors = [
            libc::pollfd {
                fd: file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            // SAFETY: `descriptors` contains two initialized pollfd values for
            // live descriptors owned by this worker. The call mutates only
            // their `revents` fields.
            let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            let wake_events = descriptors[1].revents;
            if wake_events != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "search worker was cancelled",
                ));
            }
            let file_events = descriptors[0].revents;
            if file_events & libc::POLLNVAL != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "search file descriptor became invalid",
                ));
            }
            if file_events != 0 {
                return Ok(());
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(())
    }
}

#[cfg(unix)]
struct InterruptAuthority {
    pthread: libc::pthread_t,
    wake: std::os::unix::net::UnixStream,
}

#[cfg(windows)]
struct InterruptAuthority {
    thread: SendHandle,
}

#[cfg(not(any(unix, windows)))]
struct InterruptAuthority;

impl InterruptAuthority {
    fn establish() -> io::Result<(Self, WorkerWake)> {
        #[cfg(unix)]
        {
            unblock_interrupt_signal()?;
            let (reader, writer) = std::os::unix::net::UnixStream::pair()?;
            reader.set_nonblocking(true)?;
            writer.set_nonblocking(true)?;
            // SAFETY: called on the worker; its `pthread_t` stays valid until
            // the supervisor uniquely joins that worker.
            Ok((
                Self {
                    pthread: unsafe { libc::pthread_self() },
                    wake: writer,
                },
                WorkerWake { reader },
            ))
        }
        #[cfg(windows)]
        {
            Ok((
                Self {
                    thread: duplicate_current_thread()?,
                },
                WorkerWake,
            ))
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "search workers require an interrupt authority",
            ))
        }
    }

    fn interrupt(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::io::Write;

            // A per-worker socket wakes pollable reads without relying on the
            // process-global signal disposition. Repeated nonblocking writes
            // may fill the socket; that still means a wake byte is pending.
            let mut wake = &self.wake;
            match wake.write(&[1]) {
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::BrokenPipe
                    ) => {}
                Err(error) => return Err(error),
            }

            let replaced = current_sigurg_handler()? != our_sigurg_handler();
            // Never invoke a foreign process-global handler. The socket wake
            // above remains authoritative for pollable read waits.
            if replaced {
                return Err(io::Error::other(
                    "SIGURG handler was replaced during an active search",
                ));
            }
            // SIGURG remains a best-effort wakeup for non-pollable filesystem
            // syscalls while this crate still owns the disposition.
            // SAFETY: the supervisor owns the worker join handle, so this
            // published `pthread_t` cannot be reclaimed or reused yet.
            let status = unsafe { libc::pthread_kill(self.pthread, libc::SIGURG) };
            if status == 0 || status == libc::ESRCH {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(status))
            }
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::{ERROR_INVALID_HANDLE, ERROR_NOT_FOUND};
            // SAFETY: `thread` is the live owned duplicate for the worker.
            let cancelled =
                unsafe { windows_sys::Win32::System::IO::CancelSynchronousIo(self.thread.handle) };
            if cancelled != 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            let code = error.raw_os_error().map(|value| value as u32);
            if matches!(code, Some(ERROR_NOT_FOUND | ERROR_INVALID_HANDLE)) {
                return Ok(());
            }
            Err(error)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "search workers require an interrupt authority",
            ))
        }
    }
}

/// Cancellation-safe owner for one worker supervisor.
///
/// The supervisor owns the actual worker join handle and platform interrupt
/// authority from startup. Dropping this value publishes cancellation and
/// detaches only the supervisor; that supervisor continues interrupting and
/// uniquely joins the actual worker before releasing platform handles.
struct InterruptibleWorker {
    cancel: CancellationToken,
    supervisor: Option<std::thread::JoinHandle<()>>,
    supervisor_error: Arc<Mutex<Option<String>>>,
}

impl InterruptibleWorker {
    fn spawn<T: Send + 'static>(
        work: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
        tx: tokio::sync::oneshot::Sender<Result<T, ToolError>>,
        cancel: CancellationToken,
    ) -> Result<Self, ToolError> {
        let (startup_tx, startup_rx) = std::sync::mpsc::sync_channel(1);
        let (go_tx, go_rx) = std::sync::mpsc::sync_channel(1);
        let supervisor_cancel = cancel.clone();
        let supervisor_error = Arc::new(Mutex::new(None));
        let error_slot = Arc::clone(&supervisor_error);
        let supervisor = std::thread::Builder::new()
            .name("mycode-search-supervisor".to_owned())
            .spawn(move || {
                #[cfg(unix)]
                let _signal = match acquire_interrupt_signal() {
                    Ok(guard) => guard,
                    Err(error) => {
                        let _ = startup_tx.send(Err(error));
                        return;
                    }
                };

                let (authority_tx, authority_rx) = std::sync::mpsc::sync_channel(1);
                let worker = std::thread::Builder::new()
                    .name("mycode-search-worker".to_owned())
                    .spawn(move || {
                        let (authority, wake) = match InterruptAuthority::establish() {
                            Ok(established) => established,
                            Err(error) => {
                                let _ = authority_tx.send(Err(error));
                                return;
                            }
                        };
                        if authority_tx.send(Ok(authority)).is_err() {
                            return;
                        }
                        if go_rx.recv().is_err() {
                            return;
                        }
                        let _wake = wake.enter();
                        let result = work();
                        let _ = tx.send(result);
                    });
                let worker = match worker {
                    Ok(worker) => worker,
                    Err(error) => {
                        let _ = startup_tx.send(Err(error));
                        return;
                    }
                };
                let authority = match authority_rx.recv() {
                    Ok(Ok(authority)) => authority,
                    Ok(Err(error)) => {
                        let _ = startup_tx.send(Err(error));
                        let _ = worker.join();
                        return;
                    }
                    Err(_) => {
                        let _ = startup_tx.send(Err(io::Error::other(
                            "worker dropped interrupt startup handshake",
                        )));
                        let _ = worker.join();
                        return;
                    }
                };
                if startup_tx.send(Ok(())).is_err() {
                    let _ = worker.join();
                    return;
                }

                while !worker.is_finished() {
                    if supervisor_cancel.is_cancelled()
                        && let Err(error) = authority.interrupt()
                    {
                        let mut slot = error_slot
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        if slot.is_none() {
                            *slot = Some(error.to_string());
                        }
                    }
                    std::thread::sleep(INTERRUPT_RETRY);
                }
                let _ = worker.join();
            })
            .map_err(|error| {
                ToolError::Execution(format!("search supervisor cannot start: {error}"))
            })?;

        match startup_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                drop(go_tx);
                let _ = supervisor.join();
                return Err(ToolError::Execution(format!(
                    "search interrupt authority cannot be established: {error}"
                )));
            }
            Err(_) => {
                drop(go_tx);
                let _ = supervisor.join();
                return Err(ToolError::Execution(
                    "search interrupt startup handshake was dropped".to_owned(),
                ));
            }
        }
        go_tx.send(()).map_err(|_| {
            ToolError::Execution("search worker dropped its startup gate".to_owned())
        })?;
        Ok(Self {
            cancel,
            supervisor: Some(supervisor),
            supervisor_error,
        })
    }

    async fn join(mut self) -> Option<String> {
        let supervisor = self.supervisor.take()?;
        let _ = tokio::task::spawn_blocking(move || {
            let _ = supervisor.join();
        })
        .await;
        self.supervisor_error
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

impl Drop for InterruptibleWorker {
    fn drop(&mut self) {
        self.cancel.cancel();
        // Dropping the supervisor join handle detaches only the supervisor.
        // It still owns and joins the actual worker and interrupt authority.
        let _ = self.supervisor.take();
    }
}

#[cfg(windows)]
fn duplicate_current_thread() -> io::Result<SendHandle> {
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, HANDLE};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread};

    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: duplicating the calling thread pseudo-handle into this process
    // yields a fresh owned handle on success.
    let ok = unsafe {
        windows_sys::Win32::Foundation::DuplicateHandle(
            GetCurrentProcess(),
            GetCurrentThread(),
            GetCurrentProcess(),
            &mut handle,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(SendHandle { handle })
}
