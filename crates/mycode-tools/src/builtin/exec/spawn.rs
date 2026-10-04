//! Contained spawn, output collection, and teardown for structured exec.
use std::future::Future;
use std::pin::Pin;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use tokio::time::Sleep;
use tokio_util::sync::CancellationToken;

use super::prepare::PreparedInvocation;
use super::resolve::PinnedImage;
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
use crate::builtin::process::collect_child_output;
#[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
use crate::builtin::process::combine_teardown_results;
#[cfg(any(
    all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
    all(target_os = "macos", target_arch = "aarch64")
))]
use crate::builtin::process::drain_pipes;
use crate::builtin::process::{CapturedStream, ExecutionLease, ProcessTree};
use crate::tool::ToolError;

/// Architecture selected by the macOS kernel for a loaded image.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LoadedArchitecture {
    Arm64,
    X86_64,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl LoadedArchitecture {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64",
            Self::X86_64 => "x86_64",
        }
    }
}

/// Platform metadata proven after the kernel creates the process.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ExecutionMetadata {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    loaded_architecture: Option<LoadedArchitecture>,
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    translated: bool,
}

impl ExecutionMetadata {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub(super) const fn macos(loaded_architecture: LoadedArchitecture, translated: bool) -> Self {
        Self {
            loaded_architecture: Some(loaded_architecture),
            translated,
        }
    }

    pub(super) const fn loaded_architecture(self) -> Option<&'static str> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            match self.loaded_architecture {
                Some(architecture) => Some(architecture.as_str()),
                None => None,
            }
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        None
    }

    pub(super) const fn translated(self) -> Option<bool> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            if self.loaded_architecture.is_some() {
                Some(self.translated)
            } else {
                None
            }
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        None
    }
}

/// How a contained run ended.
pub(crate) enum RunOutcome {
    Done {
        status: ExitStatus,
        stdout: CapturedStream,
        stderr: CapturedStream,
        metadata: ExecutionMetadata,
    },
    Timeout {
        stdout: CapturedStream,
        stderr: CapturedStream,
        teardown: Result<(), std::io::Error>,
        started: bool,
        metadata: ExecutionMetadata,
    },
    Cancelled {
        teardown: Result<(), std::io::Error>,
    },
    CollectFailed {
        error: std::io::Error,
        teardown: Result<(), std::io::Error>,
    },
}

enum TeardownKind {
    Done(ExitStatus),
    Timeout,
    Cancelled,
    Failed(std::io::Error),
}

/// Spawns the pinned image and collects output until exit, timeout, or cancel.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] or [`ToolError::InvalidArgs`] when spawn
/// itself fails. Collection failures are returned as [`RunOutcome`].
pub(super) async fn run_pinned(
    prepared: PreparedInvocation,
    lease: ExecutionLease,
    cancel: &CancellationToken,
    deadline: &mut Pin<&mut Sleep>,
) -> Result<RunOutcome, ToolError> {
    #[cfg(any(
        all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
        all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    {
        let program = match wait_for_spawn(
            move |abort| spawn_program(prepared, lease, &abort),
            cancel,
            deadline,
        )
        .await?
        {
            SpawnWait::Spawned(program) => program,
            SpawnWait::Timeout { started, teardown } => {
                return Ok(RunOutcome::Timeout {
                    stdout: CapturedStream::default(),
                    stderr: CapturedStream::default(),
                    teardown,
                    started,
                    metadata: ExecutionMetadata::default(),
                });
            }
            SpawnWait::Cancelled { teardown } => return Ok(RunOutcome::Cancelled { teardown }),
        };
        Ok(program.run_until(cancel, deadline).await)
    }
    #[cfg(not(any(
        all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
        all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )))]
    {
        let _ = (prepared, lease, cancel, deadline);
        Err(ToolError::Execution(
            "exec is not supported on this platform".into(),
        ))
    }
}

#[cfg(any(
    all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
    test
))]
enum SpawnWait<T> {
    Spawned(T),
    Timeout {
        started: bool,
        teardown: Result<(), std::io::Error>,
    },
    Cancelled {
        teardown: Result<(), std::io::Error>,
    },
}

/// One launched-process cleanup owner: terminate, reap, then drop pin/lease.
trait SpawnCleanup: Send + 'static {
    fn cleanup(self) -> impl Future<Output = Result<(), std::io::Error>> + Send;
}

/// Classifies a contained spawn failure for Windows nested-Job retry.
#[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SpawnFailureKind {
    /// `AssignProcessToJobObject` rejected nested-Job / breakaway enrollment.
    NestedJobEnrollmentRejected,
    /// Any other spawn, verification, resume, or cleanup failure.
    Unrelated,
}

/// Spawn failure plus the result of cleaning any process already created.
pub(super) struct SpawnFailure {
    pub(super) error: ToolError,
    pub(super) teardown: Result<(), std::io::Error>,
    #[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
    pub(super) kind: SpawnFailureKind,
}

impl SpawnFailure {
    pub(super) fn new(error: ToolError, teardown: Result<(), std::io::Error>) -> Self {
        Self {
            error,
            teardown,
            #[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
            kind: SpawnFailureKind::Unrelated,
        }
    }

    /// Nested-Job enrollment rejection that may request one breakaway retry.
    #[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
    pub(super) fn nested_job_enrollment_rejected(
        error: ToolError,
        teardown: Result<(), std::io::Error>,
    ) -> Self {
        Self {
            error,
            teardown,
            kind: SpawnFailureKind::NestedJobEnrollmentRejected,
        }
    }

    fn into_tool_error(self) -> ToolError {
        match self.teardown {
            Ok(()) => self.error,
            Err(teardown) => {
                ToolError::Execution(format!("{}; termination failed: {teardown}", self.error))
            }
        }
    }
}

impl From<ToolError> for SpawnFailure {
    fn from(error: ToolError) -> Self {
        Self::new(error, Ok(()))
    }
}

/// Retry cadence for a failed process teardown.
///
/// Ten milliseconds avoids a hot loop while keeping transient wait failures
/// responsive. The owner remains live for every retry.
const CLEANUP_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// Retains a cleanup owner until one teardown attempt succeeds.
fn finish_owned_spawn_cleanup<T>(
    mut owner: T,
    mut cleanup: impl FnMut(&mut T) -> Result<(), std::io::Error>,
) {
    while cleanup(&mut owner).is_err() {
        std::thread::sleep(CLEANUP_RETRY_INTERVAL);
    }
}

/// Retries pending-process cleanup until ownership can be released safely.
///
/// This runs only from a platform pending owner's `Drop` path after its first
/// synchronous teardown attempt failed. Blocking preserves the surrounding
/// executable pin and execution lease until the child is confirmed reaped.
pub(super) fn finish_pending_spawn_cleanup(
    mut cleanup: impl FnMut() -> Result<(), std::io::Error>,
) {
    finish_owned_spawn_cleanup((), |_| cleanup());
}

impl SpawnCleanup for () {
    async fn cleanup(self) -> Result<(), std::io::Error> {
        Ok(())
    }
}

const SPAWN_PENDING: u8 = 0;
const SPAWN_CANCELLED: u8 = 1;
const SPAWN_STARTED: u8 = 2;

/// Coordinates the deadline with the platform's irreversible spawn call.
#[derive(Clone)]
pub(super) struct SpawnGate {
    state: Arc<AtomicU8>,
    cancelled: Arc<AtomicBool>,
    launched: Arc<AtomicBool>,
}

impl SpawnGate {
    pub(super) fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(SPAWN_PENDING)),
            cancelled: Arc::new(AtomicBool::new(false)),
            launched: Arc::new(AtomicBool::new(false)),
        }
    }

    fn cancel(&self) -> bool {
        self.cancelled.store(true, Ordering::Release);
        self.state
            .compare_exchange(
                SPAWN_PENDING,
                SPAWN_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn check_pending(&self) -> Result<(), ToolError> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(ToolError::Execution(
                "command cancelled before completion".into(),
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn begin_spawn(&self) -> Result<(), ToolError> {
        self.check_pending()?;
        self.state
            .compare_exchange(
                SPAWN_PENDING,
                SPAWN_STARTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| ToolError::Execution("command cancelled before completion".into()))?;
        self.check_pending()
    }

    /// Records that the program crossed the platform's runnable boundary.
    pub(super) fn mark_launched(&self) {
        self.launched.store(true, Ordering::Release);
    }

    fn was_launched(&self) -> bool {
        self.launched.load(Ordering::Acquire)
    }
}

struct CancelSpawnOnDrop(SpawnGate);

impl Drop for CancelSpawnOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(any(
    all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
    test
))]
async fn wait_for_spawn<T, F>(
    work: F,
    cancel: &CancellationToken,
    deadline: &mut Pin<&mut Sleep>,
) -> Result<SpawnWait<T>, ToolError>
where
    T: SpawnCleanup,
    F: FnOnce(SpawnGate) -> Result<T, SpawnFailure> + Send + 'static,
{
    let gate = SpawnGate::new();
    let cancel_on_drop = CancelSpawnOnDrop(gate.clone());
    let worker_gate = gate.clone();
    let worker = tokio::task::spawn_blocking(move || work(worker_gate));
    tokio::pin!(worker);
    let outcome = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            let teardown = if gate.cancel() {
                Ok(())
            } else {
                cleanup_started_spawn(&mut worker).await
            };
            SpawnWait::Cancelled { teardown }
        }
        _ = deadline.as_mut() => {
            let teardown = if gate.cancel() {
                Ok(())
            } else {
                cleanup_started_spawn(&mut worker).await
            };
            SpawnWait::Timeout {
                started: gate.was_launched(),
                teardown,
            }
        }
        result = &mut worker => {
            let result = result.map_err(|err| {
                ToolError::Execution(format!("exec spawn worker failed: {err}"))
            })?;
            SpawnWait::Spawned(result.map_err(SpawnFailure::into_tool_error)?)
        }
    };
    drop(cancel_on_drop);
    Ok(outcome)
}

#[cfg(any(
    all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
    test
))]
async fn cleanup_started_spawn<T: SpawnCleanup>(
    worker: &mut Pin<&mut tokio::task::JoinHandle<Result<T, SpawnFailure>>>,
) -> Result<(), std::io::Error> {
    match worker.await {
        Ok(Ok(spawned)) => spawned.cleanup().await,
        Ok(Err(failure)) => failure.teardown,
        Err(error) => Err(std::io::Error::other(format!(
            "exec spawn worker failed during cleanup: {error}"
        ))),
    }
}

#[cfg(any(
    all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn spawn_program(
    prepared: PreparedInvocation,
    lease: ExecutionLease,
    gate: &SpawnGate,
) -> Result<SpawnedProgram, SpawnFailure> {
    let (pinned, argv0, args, cwd, env) = prepared.into_spawn_parts();
    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    let (child, process_tree, pinned, lease) =
        super::linux::spawn_linux(pinned, &argv0, &args, &cwd, &env, lease, gate)?;
    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    let metadata = ExecutionMetadata::default();
    #[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
    let (child, process_tree, pinned) =
        super::windows::spawn_windows(pinned, &argv0, &args, &cwd, &env, gate)?;
    #[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
    let metadata = ExecutionMetadata::default();
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let (child, process_tree, pinned, lease, metadata) =
        super::macos::spawn_macos(pinned, &argv0, &args, &cwd, &env, lease, gate)?;

    // Once a platform crosses its launch boundary, always hand the cleanup
    // owner back so timeout/cancel can await terminate-and-reap.
    Ok(SpawnedProgram {
        metadata,
        live: Some(LiveSpawn {
            #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
            inner: Inner::Tokio(child),
            #[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
            inner: Inner::Windows(child),
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            inner: Inner::Mac(child),
            process_tree,
            _pin: pinned,
            _lease: lease,
        }),
    })
}

struct SpawnedProgram {
    metadata: ExecutionMetadata,
    live: Option<LiveSpawn>,
}

struct LiveSpawn {
    inner: Inner,
    process_tree: ProcessTree,
    _pin: PinnedImage,
    _lease: ExecutionLease,
}

enum Inner {
    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    Tokio(tokio::process::Child),
    #[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
    Windows(super::windows::WindowsChild),
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    Mac(super::macos::MacChild),
    /// Platform without a supported process runtime.
    #[cfg(not(any(
        all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
        all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )))]
    Unsupported,
}

impl Drop for SpawnedProgram {
    fn drop(&mut self) {
        if let Some(live) = self.live.take() {
            supervise_spawn_cleanup(live);
        }
    }
}

#[cfg(any(
    all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
impl SpawnCleanup for SpawnedProgram {
    async fn cleanup(mut self) -> Result<(), std::io::Error> {
        self.teardown().await
    }
}

fn supervise_spawn_cleanup(live: LiveSpawn) {
    let pending = Arc::new(std::sync::Mutex::new(Some(live)));
    let worker_pending = Arc::clone(&pending);
    let thread = std::thread::Builder::new()
        .name("mycode-exec-cleanup".into())
        .spawn(move || {
            let Some(live) = take_pending_cleanup(&worker_pending) else {
                return;
            };
            finish_owned_spawn_cleanup(live, teardown_live_blocking);
        });
    if thread.is_err()
        && let Some(live) = take_pending_cleanup(&pending)
    {
        finish_owned_spawn_cleanup(live, teardown_live_blocking);
    }
}

fn take_pending_cleanup(pending: &std::sync::Mutex<Option<LiveSpawn>>) -> Option<LiveSpawn> {
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

fn teardown_live_blocking(live: &mut LiveSpawn) -> Result<(), std::io::Error> {
    match &mut live.inner {
        #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
        Inner::Tokio(child) => {
            if let Err(error) = live.process_tree.terminate(Some(child)) {
                let _ = child.start_kill();
                return Err(error);
            }
            wait_tokio_child_blocking(child)
        }
        #[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
        Inner::Windows(child) => {
            let containment = live.process_tree.terminate(None);
            if containment.is_err() {
                let _ = child.terminate();
            }
            let leader = child.wait_blocking().map(|_| ());
            combine_teardown_results(containment, leader)
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        Inner::Mac(child) => {
            child.terminate_tree(&live.process_tree)?;
            child.reap_blocking()
        }
        #[cfg(not(any(
            all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
            all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        )))]
        Inner::Unsupported => Ok(()),
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
async fn wait_tokio_child(child: &mut tokio::process::Child) -> Result<(), std::io::Error> {
    loop {
        match child.wait().await {
            Ok(_) => return Ok(()),
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
pub(super) fn wait_tokio_child_blocking(
    child: &mut tokio::process::Child,
) -> Result<(), std::io::Error> {
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

async fn teardown_live(live: &mut LiveSpawn) -> Result<(), std::io::Error> {
    match &mut live.inner {
        #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
        Inner::Tokio(child) => {
            if let Err(error) = live.process_tree.terminate(Some(child)) {
                let _ = child.start_kill();
                return Err(error);
            }
            wait_tokio_child(child).await
        }
        #[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
        Inner::Windows(child) => {
            let containment = live.process_tree.terminate(None);
            let leader = if containment.is_ok() {
                child.wait().await.map(|_| ())
            } else {
                let termination = child.terminate();
                let wait = child.wait().await.map(|_| ());
                combine_teardown_results(termination, wait)
            };
            combine_teardown_results(containment, leader)
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        Inner::Mac(child) => {
            child.terminate_tree(&live.process_tree)?;
            match child.wait().await {
                Ok(_) => Ok(()),
                Err(wait_error) => child.reap_blocking().map_err(|reap_error| {
                    std::io::Error::other(format!(
                        "async child wait failed: {wait_error}; blocking reap failed: {reap_error}"
                    ))
                }),
            }
        }
        #[cfg(not(any(
            all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
            all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        )))]
        Inner::Unsupported => Ok(()),
    }
}

impl SpawnedProgram {
    async fn run_until(
        mut self,
        cancel: &CancellationToken,
        deadline: &mut Pin<&mut Sleep>,
    ) -> RunOutcome {
        let metadata = self.metadata;
        let mut stdout = CapturedStream::new();
        let mut stderr = CapturedStream::new();
        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => TeardownKind::Cancelled,
            _ = deadline.as_mut() => TeardownKind::Timeout,
            status = self.collect(&mut stdout, &mut stderr) => match status {
                Ok(status) => TeardownKind::Done(status),
                Err(err) => TeardownKind::Failed(err),
            },
        };
        match outcome {
            TeardownKind::Done(status) => {
                drop(self.live.take());
                RunOutcome::Done {
                    status,
                    stdout,
                    stderr,
                    metadata,
                }
            }
            TeardownKind::Failed(error) => {
                let teardown = self.teardown().await;
                RunOutcome::CollectFailed { error, teardown }
            }
            TeardownKind::Timeout => {
                let teardown = self.teardown().await;
                RunOutcome::Timeout {
                    stdout,
                    stderr,
                    teardown,
                    started: true,
                    metadata,
                }
            }
            TeardownKind::Cancelled => {
                let teardown = self.teardown().await;
                RunOutcome::Cancelled { teardown }
            }
        }
    }

    async fn collect(
        &mut self,
        stdout: &mut CapturedStream,
        stderr: &mut CapturedStream,
    ) -> std::io::Result<ExitStatus> {
        let live = self
            .live
            .as_mut()
            .ok_or_else(|| std::io::Error::other("spawned program already cleaned up"))?;
        match &mut live.inner {
            #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
            Inner::Tokio(child) => {
                let mut stdout_pipe = Some(child.stdout.take().expect("stdout was piped"));
                let mut stderr_pipe = Some(child.stderr.take().expect("stderr was piped"));
                collect_child_output(child, &mut stdout_pipe, &mut stderr_pipe, stdout, stderr)
                    .await
            }
            #[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
            Inner::Windows(child) => {
                let mut stdout_pipe = child.take_stdout();
                let mut stderr_pipe = child.take_stderr();
                drain_pipes(&mut stdout_pipe, &mut stderr_pipe, stdout, stderr).await?;
                child.wait().await
            }
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            Inner::Mac(child) => {
                let mut stdout_pipe = child.take_stdout()?;
                let mut stderr_pipe = child.take_stderr()?;
                drain_pipes(&mut stdout_pipe, &mut stderr_pipe, stdout, stderr).await?;
                child.wait().await
            }
            #[cfg(not(any(
                all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
                all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
                all(target_os = "macos", target_arch = "aarch64")
            )))]
            Inner::Unsupported => Err(std::io::Error::other("exec is not supported")),
        }
    }

    async fn teardown(&mut self) -> Result<(), std::io::Error> {
        let Some(live) = self.live.as_mut() else {
            return Ok(());
        };
        match teardown_live(live).await {
            Ok(()) => {
                drop(self.live.take());
                Ok(())
            }
            Err(error) => {
                if let Some(live) = self.live.take() {
                    supervise_spawn_cleanup(live);
                }
                Err(error)
            }
        }
    }
}
