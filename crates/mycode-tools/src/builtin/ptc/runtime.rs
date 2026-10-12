//! System-Python process for `run_code`.
//!
//! A call with a session key reuses a warm interpreter for that session and
//! isolation mode (main agent or scout). The first call still cold-starts:
//! sealing, the token handshake, and `-I` for scout happen at spawn and are
//! not repeated or relaxed on later calls. The token is checked before any
//! program is sent. On Unix it crosses the inherited socket and is not an
//! environment variable. On TCP it is a 0600 file, unlinked once the ready
//! frame arrives; only that path is placed in the environment. It is never
//! put on the command line. Calls with no session key still spawn and tear
//! down a process each time.
//!
//! Tool calls arrive on a length-framed control channel, not on stdout. The
//! channel is an inherited socket (Unix) or a loopback TCP socket that
//! requires the per-process token. Windows deliberately uses no named pipe:
//! Python opens a synchronous pipe handle, and such a handle serializes
//! pending I/O, so the blocked reader thread deadlocked every later frame
//! write from the event loop. A TCP socket carries the reader thread and the
//! writer concurrently. Each call is prepared and executed with
//! [`crate::prepare_tool_ctx`], the same preflight a direct call uses.
//! Read-only calls and `agent` may overlap; mutating calls wait until the
//! ones submitted before them have finished, then run alone. User globals do
//! not survive into the next program; the child restores them before the
//! next boot.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use mycode_core::message::ContentBlock;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;

use super::protocol::{ChildFrame, HostFrame, decode_child, encode_frame};
use crate::builtin::process::{ProcessTree, enroll_spawned};
use crate::builtin::python::{PythonInterpreter, PythonStatus, python_status};
use crate::ctx::ToolCtx;
use crate::registry::ToolCatalog;
use crate::stream::ToolStream;
use crate::tool::{ToolDyn, ToolResult};

/// Tool calls inside one program. The next call aborts the run.
const MAX_CALLS: u32 = 48;
/// Read-only calls that may overlap.
const MAX_PARALLEL: usize = 8;
/// Calls accepted but not yet answered. Further calls get a catchable error.
const MAX_PENDING: usize = 32;
/// Default wall clock, including tool waits and `ask_user`.
pub(super) const DEFAULT_WALL: Duration = Duration::from_secs(120);
/// How long to wait for the interpreter to connect.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
/// How long one unauthenticated TCP peer may sit before the listener moves on.
///
/// A local process that connects and stalls must not consume the whole startup
/// window. The real interpreter's ready frame is a few hundred bytes sent
/// immediately after connect.
const HANDSHAKE_ATTEMPT: Duration = Duration::from_millis(500);
/// Unauthenticated TCP accepts before the handshake fails closed.
const MAX_HANDSHAKE_ATTEMPTS: u32 = 32;

const SHELL_NAMES: &[&str] = &["powershell", "bash", "zsh", "sh", "cmd"];
/// Tools that may overlap. `agent` is included because write-capable children
/// use their own worktree and the host still caps how many run at once.
/// `write`, `edit`, and `shell` stay exclusive.
const SAFE_TOOLS: &[&str] = &[
    "read",
    "grep",
    "find",
    "web_search",
    "fetch_content",
    "agent",
];

/// What the program printed and returned, or why it stopped.
pub(super) struct ProgramOutcome {
    pub logs: String,
    pub value: Option<String>,
    pub error: Option<ProgramError>,
    /// Inner calls, for the tool-result details object.
    pub calls: Vec<CallNote>,
    /// True when this call skipped `spawn_python` and ran on an interpreter
    /// already warm for the same session and isolation mode.
    pub reused: bool,
}

#[derive(Debug)]
pub(super) struct ProgramError {
    pub kind: &'static str,
    pub message: String,
}

#[derive(Clone)]
pub(super) struct CallNote {
    pub tool: String,
    pub ok: bool,
    pub bytes: usize,
}

struct QueuedCall {
    id: u64,
    name: String,
    args: Value,
    safe: bool,
    tool: Arc<dyn ToolDyn>,
}

struct FinishedCall {
    id: u64,
    exclusive: bool,
    reply: HostFrame,
    note: CallNote,
}

struct RunningChild {
    child: Child,
    tree: Option<ProcessTree>,
}

impl RunningChild {
    fn kill_tree(&mut self) {
        if let Some(tree) = &self.tree {
            let _ = tree.terminate(Some(&self.child));
        } else if let Some(pid) = self.child.id() {
            // Platforms without enrollment still signal the process group
            // created by `process_group(0)`.
            #[cfg(unix)]
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            #[cfg(not(unix))]
            {
                let _ = pid;
            }
        }
        let _ = self.child.start_kill();
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        self.kill_tree();
    }
}

/// Runs `code` on the system Python. Inner calls go through the shared
/// preflight and emit nested UI events on `out`.
///
/// When `ctx` carries a session key, a warm interpreter for that session and
/// isolation mode is reused. Otherwise the process is spawned for this call
/// and torn down afterward.
pub(super) async fn execute_program(
    code: &str,
    ctx: &ToolCtx,
    catalog: &ToolCatalog,
    out: &ToolStream,
) -> ProgramOutcome {
    let PythonStatus::Ready(interpreter) = python_status() else {
        return ProgramOutcome {
            logs: String::new(),
            value: None,
            error: Some(ProgramError {
                kind: "exception",
                message: crate::builtin::python::python_unavailable_message()
                    .unwrap_or_else(|| "Python is not available".to_owned()),
            }),
            calls: Vec::new(),
            reused: false,
        };
    };
    let restricted = catalog_is_restricted(catalog);
    let tools = sdk_names(catalog);
    run_session(&interpreter, code, restricted, &tools, ctx, catalog, out).await
}

/// Idle interpreters kept per session. Two covers a parent turn and one
/// overlapping subagent without holding a process per call.
const MAX_IDLE_PER_KEY: usize = 2;
/// Checked when a later `run_code` touches the pool, and on session release.
const IDLE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    session: String,
    restricted: bool,
}

struct IdleWorker {
    worker: WarmWorker,
    since: Instant,
}

struct Driven {
    outcome: ProgramOutcome,
    recycle: bool,
}

struct WarmWorker {
    running: RunningChild,
    frames: mpsc::UnboundedReceiver<Result<ChildFrame, String>>,
    writer: ControlWrite,
    raw_out: Arc<Mutex<String>>,
    raw_err: Arc<Mutex<String>>,
    /// Programs parked on this process. Zero until the first one finishes.
    runs: u32,
}

impl WarmWorker {
    fn alive(&mut self) -> bool {
        matches!(self.running.child.try_wait(), Ok(None))
    }
}

fn pool() -> &'static Mutex<HashMap<PoolKey, Vec<IdleWorker>>> {
    static POOL: OnceLock<Mutex<HashMap<PoolKey, Vec<IdleWorker>>>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_pool() -> std::sync::MutexGuard<'static, HashMap<PoolKey, Vec<IdleWorker>>> {
    pool().lock().unwrap_or_else(|err| err.into_inner())
}

/// Sessions whose interpreters must not be parked again.
///
/// Delete can race a call that is about to return its worker. The set is
/// remembered for the process lifetime; session ids are not reused.
fn closed_sessions() -> &'static Mutex<HashSet<String>> {
    static CLOSED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    CLOSED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn lock_closed() -> std::sync::MutexGuard<'static, HashSet<String>> {
    closed_sessions()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
}

fn reap_expired(slots: &mut HashMap<PoolKey, Vec<IdleWorker>>) -> Vec<WarmWorker> {
    let now = Instant::now();
    let mut doomed = Vec::new();
    slots.retain(|_, idle| {
        let mut keep = Vec::new();
        for mut entry in idle.drain(..) {
            let stale = now.duration_since(entry.since) > IDLE_TTL;
            if stale || !entry.worker.alive() {
                doomed.push(entry.worker);
            } else {
                keep.push(entry);
            }
        }
        *idle = keep;
        !idle.is_empty()
    });
    doomed
}

fn take_idle(key: &PoolKey) -> (Option<WarmWorker>, Vec<WarmWorker>) {
    let mut slots = lock_pool();
    let doomed = reap_expired(&mut slots);
    let found = slots
        .get_mut(key)
        .and_then(|idle| idle.pop().map(|entry| entry.worker));
    if slots.get(key).is_some_and(|idle| idle.is_empty()) {
        slots.remove(key);
    }
    drop(slots);
    (found, doomed)
}

fn park(key: PoolKey, worker: WarmWorker) {
    // Closed lock before the pool lock. `release_session` uses the same order.
    let closed = lock_closed();
    if closed.contains(&key.session) {
        drop(closed);
        drop(worker);
        return;
    }
    let mut slots = lock_pool();
    let expired = reap_expired(&mut slots);
    let idle = slots.entry(key).or_default();
    idle.push(IdleWorker {
        worker,
        since: Instant::now(),
    });
    let mut overflow = Vec::new();
    if idle.len() > MAX_IDLE_PER_KEY {
        let extra = idle.len() - MAX_IDLE_PER_KEY;
        overflow.extend(idle.drain(..extra).map(|entry| entry.worker));
    }
    drop(slots);
    drop(closed);
    drop(expired);
    drop(overflow);
}

/// Drops every warm interpreter for `session` (both isolation modes).
///
/// A call that is still winding down will not park its process afterward.
pub(super) fn release_session(session: &str) {
    if session.is_empty() {
        return;
    }
    let mut closed = lock_closed();
    closed.insert(session.to_owned());
    let mut slots = lock_pool();
    let keys: Vec<PoolKey> = slots
        .keys()
        .filter(|key| key.session == session)
        .cloned()
        .collect();
    let mut doomed = Vec::new();
    for key in keys {
        if let Some(idle) = slots.remove(&key) {
            doomed.extend(idle.into_iter().map(|entry| entry.worker));
        }
    }
    drop(slots);
    drop(closed);
    drop(doomed);
}

async fn run_session(
    interpreter: &PythonInterpreter,
    code: &str,
    restricted: bool,
    tools: &[String],
    ctx: &ToolCtx,
    catalog: &ToolCatalog,
    out: &ToolStream,
) -> ProgramOutcome {
    let key = ctx
        .ptc_session
        .as_deref()
        .filter(|session| !session.is_empty())
        .map(|session| PoolKey {
            session: session.to_owned(),
            restricted,
        });
    let (mut worker, reused, doomed) = if let Some(key) = &key {
        let (idle, doomed) = take_idle(key);
        match idle {
            Some(worker) => (worker, true, doomed),
            None => match spawn_warm(interpreter, restricted, ctx).await {
                Ok(worker) => (worker, false, doomed),
                Err(outcome) => {
                    drop(doomed);
                    return outcome;
                }
            },
        }
    } else {
        match spawn_warm(interpreter, restricted, ctx).await {
            Ok(worker) => (worker, false, Vec::new()),
            Err(outcome) => return outcome,
        }
    };
    drop(doomed);
    let mut driven = drive_worker(&mut worker, code, restricted, tools, ctx, catalog, out).await;
    driven.outcome.reused = reused;
    if driven.recycle
        && let Some(key) = key
    {
        worker.runs = worker.runs.saturating_add(1);
        park(key, worker);
    }
    driven.outcome
}

fn catalog_is_restricted(catalog: &ToolCatalog) -> bool {
    !catalog.names().iter().any(|name| {
        name == "write" || name == "edit" || name == "agent" || SHELL_NAMES.contains(&name.as_str())
    })
}

fn sdk_names(catalog: &ToolCatalog) -> Vec<String> {
    let mut names: Vec<String> = catalog
        .names()
        .into_iter()
        .filter(|name| name != "run_code" && !SHELL_NAMES.contains(&name.as_str()))
        .collect();
    if catalog
        .names()
        .iter()
        .any(|name| SHELL_NAMES.contains(&name.as_str()))
    {
        names.push("shell".to_owned());
    }
    names.sort();
    names.dedup();
    names
}

fn is_safe(name: &str) -> bool {
    SAFE_TOOLS.contains(&name)
}

fn resolve_tool(catalog: &ToolCatalog, name: &str) -> Option<(String, Arc<dyn ToolDyn>)> {
    if name == "shell" {
        for shell in SHELL_NAMES {
            if let Some(tool) = catalog.get(shell) {
                return Some(("shell".to_owned(), tool));
            }
        }
        return None;
    }
    catalog.get(name).map(|tool| (name.to_owned(), tool))
}

fn boot_failure(message: String) -> ProgramOutcome {
    let kind = if message == "program cancelled" {
        "abort"
    } else if message.contains("startup") || message.contains("time limit") {
        "timeout"
    } else {
        "protocol"
    };
    stopped(kind, &message, String::new(), &[])
}

async fn spawn_warm(
    interpreter: &PythonInterpreter,
    restricted: bool,
    ctx: &ToolCtx,
) -> Result<WarmWorker, ProgramOutcome> {
    let mut control = match open_control().await {
        Ok(control) => control,
        Err(message) => return Err(stopped("exception", &message, String::new(), &[])),
    };
    let spec = control.env_spec();
    let bootstrap = match bootstrap_path() {
        Ok(path) => path,
        Err(message) => return Err(stopped("exception", &message, String::new(), &[])),
    };
    let env = match crate::builtin::exec::snapshot_child_environment() {
        Ok(env) => env,
        Err(error) => {
            return Err(stopped(
                "exception",
                &format!("python environment: {error}"),
                String::new(),
                &[],
            ));
        }
    };
    let mut child = match spawn_python(interpreter, restricted, &bootstrap, &spec, ctx, &env) {
        Ok(child) => child,
        Err(message) => return Err(stopped("exception", &message, String::new(), &[])),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let tree = enroll_spawned(&child).ok();
    let mut running = RunningChild { child, tree };
    let raw_out = Arc::new(Mutex::new(String::new()));
    let raw_err = Arc::new(Mutex::new(String::new()));
    if let Some(pipe) = stdout {
        let slot = Arc::clone(&raw_out);
        tokio::spawn(drain_pipe(pipe, slot));
    }
    if let Some(pipe) = stderr {
        let slot = Arc::clone(&raw_err);
        tokio::spawn(drain_pipe(pipe, slot));
    }

    control.release_local_end();
    let wall = ctx.ptc_wall.unwrap_or(DEFAULT_WALL);
    let startup = Instant::now() + STARTUP_TIMEOUT.min(wall);
    let (mut reader, writer) = match control.handshake(ctx, startup).await {
        Ok(io) => io,
        Err(message) => {
            running.kill_tree();
            return Err(boot_failure(message));
        }
    };
    let (frame_tx, frames) = mpsc::unbounded_channel::<Result<ChildFrame, String>>();
    tokio::spawn(async move {
        loop {
            let frame = read_frame_idle(&mut reader).await;
            let closed = frame.is_err();
            if frame_tx.send(frame).is_err() || closed {
                return;
            }
        }
    });
    Ok(WarmWorker {
        running,
        frames,
        writer,
        raw_out,
        raw_err,
        runs: 0,
    })
}

async fn drive_worker(
    worker: &mut WarmWorker,
    code: &str,
    restricted: bool,
    tools: &[String],
    parent: &ToolCtx,
    catalog: &ToolCatalog,
    out: &ToolStream,
) -> Driven {
    worker
        .raw_out
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clear();
    worker
        .raw_err
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clear();
    // Cancelling this token stops inner tools without cancelling the turn.
    // A parent interrupt cancels it too, because it is a child token.
    let program_cancel = parent.cancel.child_token();
    let mut ctx = parent.clone();
    ctx.cancel = program_cancel.clone();
    let wall = ctx.ptc_wall.unwrap_or(DEFAULT_WALL);
    let deadline = Instant::now() + wall;
    let cwd = ctx.cwd.display().to_string();
    if write_host(
        &mut worker.writer,
        &HostFrame::Boot {
            code: code.to_owned(),
            tools: tools.to_vec(),
            restricted,
            warn_process: !restricted,
            cwd,
        },
    )
    .await
    .is_err()
    {
        return Driven {
            outcome: stopped("worker-exit", "lost the python process", String::new(), &[]),
            recycle: false,
        };
    }

    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<FinishedCall>();
    let mut queue: VecDeque<QueuedCall> = VecDeque::new();
    let mut inflight: Vec<(u64, bool)> = Vec::new();
    let calls = Arc::new(AtomicU32::new(0));
    let mut logs = String::new();
    let mut notes = Vec::new();
    let mut exclusive = false;
    let mut recycle = false;
    let outcome = {
        let writer = &mut worker.writer;
        let frame_rx = &mut worker.frames;
        loop {
            pump(
                &mut queue,
                &mut inflight,
                &mut exclusive,
                &done_tx,
                &ctx,
                catalog,
                out,
            );
            tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => {
                    break stopped("abort", "program cancelled", logs, &notes);
                }
                _ = tokio::time::sleep_until(deadline.into()) => {
                    break stopped(
                        "timeout",
                        &format!("program exceeded {} seconds", wall.as_secs()),
                        logs,
                        &notes,
                    );
                }
                finished = done_rx.recv() => {
                    if ctx.cancel.is_cancelled() {
                        break stopped("abort", "program cancelled", logs, &notes);
                    }
                    let Some(finished) = finished else { continue };
                    inflight.retain(|(id, _)| *id != finished.id);
                    if finished.exclusive {
                        exclusive = false;
                    }
                    notes.push(finished.note);
                    if write_host(writer, &finished.reply).await.is_err() {
                        break stopped(
                            "worker-exit",
                            "lost the python process while answering a tool",
                            logs,
                            &notes,
                        );
                    }
                }
                incoming = frame_rx.recv() => {
                    let Some(incoming) = incoming else {
                        break stopped("worker-exit", "python closed the control channel", logs, &notes);
                    };
                    match incoming {
                        Ok(ChildFrame::Log { text }) => append_log(&mut logs, &text),
                        Ok(ChildFrame::Warn { text }) => {
                            append_log(&mut logs, &format!(
                                "warning: Python called {text}; use tools.shell(...) for external programs instead of subprocess or os.system\n"
                            ));
                        }
                        Ok(ChildFrame::Call { id, name, args }) => {
                            let ordinal = calls.fetch_add(1, Ordering::Relaxed);
                            if ordinal >= MAX_CALLS {
                                break stopped(
                                    "budget",
                                    &format!("program exceeded {MAX_CALLS} tool calls"),
                                    logs,
                                    &notes,
                                );
                            }
                            if inflight.len() + queue.len() >= MAX_PENDING {
                                let reply = HostFrame::Reply {
                                    id,
                                    ok: false,
                                    value: None,
                                    message: Some(format!(
                                        "too many tool calls are already waiting (limit {MAX_PENDING})"
                                    )),
                                    tool: Some(name),
                                };
                                if write_host(writer, &reply).await.is_err() {
                                    break stopped("worker-exit", "lost the python process", logs, &notes);
                                }
                                continue;
                            }
                            match accept_call(catalog, id, &name, args) {
                                CallDecision::Queue(queued) => queue.push_back(queued),
                                CallDecision::Reject(reply) => {
                                    if write_host(writer, &reply).await.is_err() {
                                        break stopped("worker-exit", "lost the python process", logs, &notes);
                                    }
                                }
                            }
                        }
                        Ok(ChildFrame::Done { value, error, warm }) => {
                            recycle = warm.unwrap_or(true)
                                && inflight.is_empty()
                                && queue.is_empty();
                            if let Some(error) = error {
                                let kind = match error.kind.as_str() {
                                    "invalid-output" => "invalid-output",
                                    "output-limit" => "output-limit",
                                    "timeout" => "timeout",
                                    _ => "exception",
                                };
                                break ProgramOutcome {
                                    logs,
                                    value: None,
                                    error: Some(ProgramError {
                                        kind,
                                        message: error.message,
                                    }),
                                    calls: notes.clone(),
                                    reused: false,
                                };
                            }
                            break ProgramOutcome {
                                logs,
                                value: value
                                    .as_ref()
                                    .map(render_return)
                                    .filter(|text| !text.is_empty()),
                                error: None,
                                calls: notes.clone(),
                                reused: false,
                            };
                        }
                        Ok(ChildFrame::Ready { .. }) => {
                            break stopped("protocol", "unexpected ready frame", logs, &notes);
                        }
                        Err(message) => {
                            let kind = if message == "program cancelled" {
                                "abort"
                            } else if message.contains("time limit") {
                                "timeout"
                            } else {
                                "worker-exit"
                            };
                            break stopped(kind, &message, logs, &notes);
                        }
                    }
                }
            }
        }
    };

    if recycle {
        loop {
            match worker.frames.try_recv() {
                Ok(_) => recycle = false,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    recycle = false;
                    break;
                }
            }
        }
    }
    program_cancel.cancel();
    if recycle && !worker.alive() {
        recycle = false;
    }
    if !recycle {
        worker.running.kill_tree();
        let _ = tokio::time::timeout(Duration::from_secs(2), worker.running.child.wait()).await;
    }
    let mut outcome = outcome;
    append_raw(&mut outcome.logs, &worker.raw_out, "stdout");
    append_raw(&mut outcome.logs, &worker.raw_err, "stderr");
    Driven { outcome, recycle }
}

fn stopped(kind: &'static str, message: &str, logs: String, calls: &[CallNote]) -> ProgramOutcome {
    ProgramOutcome {
        logs,
        value: None,
        error: Some(ProgramError {
            kind,
            message: message.to_owned(),
        }),
        calls: calls.to_vec(),
        reused: false,
    }
}

enum CallDecision {
    Queue(QueuedCall),
    Reject(HostFrame),
}

fn accept_call(catalog: &ToolCatalog, id: u64, name: &str, args: Value) -> CallDecision {
    if name == "run_code" {
        return CallDecision::Reject(HostFrame::Reply {
            id,
            ok: false,
            value: None,
            message: Some("run_code cannot be called from inside a program".to_owned()),
            tool: Some(name.to_owned()),
        });
    }
    let Some((display, tool)) = resolve_tool(catalog, name) else {
        let available = sdk_names(catalog).join(", ");
        return CallDecision::Reject(HostFrame::Reply {
            id,
            ok: false,
            value: None,
            message: Some(format!("unknown tool {name}; available: {available}")),
            tool: Some(name.to_owned()),
        });
    };
    let args = if args.is_object() { args } else { json!({}) };
    CallDecision::Queue(QueuedCall {
        id,
        name: display,
        args,
        safe: is_safe(name),
        tool,
    })
}

fn pump(
    queue: &mut VecDeque<QueuedCall>,
    inflight: &mut Vec<(u64, bool)>,
    exclusive: &mut bool,
    done_tx: &mpsc::UnboundedSender<FinishedCall>,
    ctx: &ToolCtx,
    _catalog: &ToolCatalog,
    out: &ToolStream,
) {
    while inflight.len() < MAX_PARALLEL {
        let Some(next) = queue.front() else {
            return;
        };
        let start_exclusive = !next.safe;
        if start_exclusive && !inflight.is_empty() {
            return;
        }
        if !start_exclusive && *exclusive {
            return;
        }
        let call = queue.pop_front().expect("front");
        let exclusive_call = !call.safe;
        inflight.push((call.id, exclusive_call));
        if exclusive_call {
            *exclusive = true;
        }
        let tx = done_tx.clone();
        let parent = ctx.clone();
        let stream = out.clone();
        tokio::spawn(async move {
            let finished = run_one(call, &parent, &stream).await;
            let _ = tx.send(finished);
        });
        if exclusive_call {
            return;
        }
    }
}

async fn run_one(call: QueuedCall, parent: &ToolCtx, out: &ToolStream) -> FinishedCall {
    let nested_id = format!("{}:ptc:{}", parent.call_id, call.id);
    let target = mycode_core::tool_target(&call.name, &call.args);
    let _ = out.nested_started(&nested_id, &call.name, &target);
    let ctx = match crate::prepare_tool_ctx(
        &parent.cwd,
        &parent.extra_roots,
        &parent.read_roots,
        parent.cancel.clone(),
        nested_id.clone(),
        call.tool.as_ref(),
        &call.args,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(message) => {
            return finish_call(call, out, &nested_id, ToolResult::error(message));
        }
    };
    let result = drive_tool(call.tool.clone(), call.args.clone(), ctx, out, &nested_id).await;
    finish_call(call, out, &nested_id, result)
}

fn finish_call(
    call: QueuedCall,
    out: &ToolStream,
    nested_id: &str,
    result: ToolResult,
) -> FinishedCall {
    let text = script_text(&call.name, &result);
    let ok = !result.is_error;
    let _ = out.nested_completed(nested_id, result);
    let reply = if ok {
        HostFrame::Reply {
            id: call.id,
            ok: true,
            value: Some(Value::String(text.clone())),
            message: None,
            tool: None,
        }
    } else {
        HostFrame::Reply {
            id: call.id,
            ok: false,
            value: None,
            message: Some(text.clone()),
            tool: Some(call.name.clone()),
        }
    };
    FinishedCall {
        id: call.id,
        exclusive: !call.safe,
        reply,
        note: CallNote {
            tool: call.name,
            ok,
            bytes: text.len(),
        },
    }
}

async fn drive_tool(
    tool: Arc<dyn ToolDyn>,
    args: Value,
    ctx: ToolCtx,
    out: &ToolStream,
    nested_id: &str,
) -> ToolResult {
    let (mut producer, mut consumer) = ToolStream::channel();
    let worker = tokio::spawn(async move { tool.execute_dyn(args, &ctx, &mut producer).await });
    tokio::pin!(worker);
    let mut result = None;
    let mut joined = false;
    loop {
        tokio::select! {
            biased;
            finished = &mut worker, if !joined => {
                joined = true;
                result = Some(map_join(finished));
            }
            item = consumer.recv() => {
                match item {
                    Some(crate::stream::ToolStreamItem::Progress(progress)) => {
                        let _ = out.nested_progress(nested_id, progress.message);
                    }
                    Some(crate::stream::ToolStreamItem::Terminal(value)) => {
                        result = Some(value);
                    }
                    Some(_) => {}
                    None => break,
                }
            }
        }
        if result.is_some() {
            while let Some(item) = consumer.try_recv() {
                if let crate::stream::ToolStreamItem::Progress(progress) = item {
                    let _ = out.nested_progress(nested_id, progress.message);
                }
            }
            break;
        }
    }
    if !joined {
        result = Some(map_join(worker.await));
    }
    result.unwrap_or_else(|| ToolResult::error("tool task ended without a result".to_owned()))
}

fn map_join(
    finished: Result<Result<ToolResult, crate::tool::ToolError>, tokio::task::JoinError>,
) -> ToolResult {
    match finished {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => ToolResult::error(error.to_string()),
        Err(error) if error.is_panic() => ToolResult::error("tool panicked".to_owned()),
        Err(_) => ToolResult::error("tool task ended before a result".to_owned()),
    }
}

fn script_text(name: &str, result: &ToolResult) -> String {
    let text = result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<String>();
    if name == "read" {
        strip_revision_tag(&text)
    } else {
        text
    }
}

fn strip_revision_tag(text: &str) -> String {
    let Some(index) = text.rfind("\n[revision ") else {
        if revision_tag_line(text) {
            return String::new();
        }
        return text.to_owned();
    };
    let tail = &text[index + 1..];
    if revision_tag_line(tail) {
        text[..index].to_owned()
    } else {
        text.to_owned()
    }
}

fn revision_tag_line(text: &str) -> bool {
    text.starts_with("[revision ") && text.ends_with(']') && !text.contains('\n')
}

fn render_return(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn append_log(logs: &mut String, text: &str) {
    if logs.len() >= 64 * 1024 {
        return;
    }
    let room = (64 * 1024_usize).saturating_sub(logs.len());
    let take = text.floor_char_boundary(room.min(text.len()));
    logs.push_str(&text[..take]);
}

fn append_raw(logs: &mut String, slot: &std::sync::Mutex<String>, label: &str) {
    let Ok(raw) = slot.lock() else {
        return;
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return;
    }
    if !logs.is_empty() && !logs.ends_with('\n') {
        logs.push('\n');
    }
    append_log(logs, &format!("[{label}]\n{raw}\n"));
}

async fn read_frame_idle(reader: &mut ControlRead) -> Result<ChildFrame, String> {
    let mut header = [0_u8; 4];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|_| "python closed the control channel".to_owned())?;
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 || len > super::protocol::MAX_FRAME_BYTES {
        return Err(format!("control frame length {len} is not allowed"));
    }
    let mut body = vec![0_u8; len];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|_| "python closed the control channel".to_owned())?;
    decode_child(&body)
}

async fn read_child_frame(
    reader: &mut ControlRead,
    ctx: &ToolCtx,
    deadline: Instant,
) -> Result<ChildFrame, String> {
    let mut header = [0_u8; 4];
    tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => return Err("program cancelled".to_owned()),
        _ = tokio::time::sleep_until(deadline.into()) => {
            return Err("program exceeded its time limit".to_owned());
        }
        read = reader.read_exact(&mut header) => {
            read.map_err(|_| "python closed the control channel".to_owned())?;
        }
    }
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 || len > super::protocol::MAX_FRAME_BYTES {
        return Err(format!("control frame length {len} is not allowed"));
    }
    let mut body = vec![0_u8; len];
    tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => return Err("program cancelled".to_owned()),
        _ = tokio::time::sleep_until(deadline.into()) => {
            return Err("program exceeded its time limit".to_owned());
        }
        read = reader.read_exact(&mut body) => {
            read.map_err(|_| "python closed the control channel".to_owned())?;
        }
    }
    decode_child(&body)
}

async fn write_host(writer: &mut ControlWrite, frame: &HostFrame) -> Result<(), String> {
    let bytes = encode_frame(frame)?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|_| "python closed the control channel".to_owned())
}

type ControlRead = Box<dyn AsyncRead + Unpin + Send>;
type ControlWrite = Box<dyn AsyncWrite + Unpin + Send>;

struct ControlEnv {
    fd: Option<i32>,
    port: Option<u16>,
    /// 0600 file holding the TCP token. Absent for the inherited socket.
    token_file: Option<PathBuf>,
}

/// Deletes the TCP token file when the handshake finishes or is abandoned.
struct TokenFile(PathBuf);

impl Drop for TokenFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

enum PendingControl {
    #[cfg(unix)]
    Unix {
        host: tokio::net::UnixStream,
        child: Option<std::os::unix::net::UnixStream>,
        fd: i32,
        token: String,
    },
    Tcp {
        listener: TcpListener,
        port: u16,
        token: String,
        token_file: TokenFile,
    },
}

fn fresh_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn tokens_match(got: &str, expected: &str) -> bool {
    if expected.is_empty() || got.len() != expected.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in got.bytes().zip(expected.bytes()) {
        diff |= left ^ right;
    }
    diff == 0
}

fn box_split(
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
) -> (ControlRead, ControlWrite) {
    let (reader, writer) = tokio::io::split(stream);
    (Box::new(reader), Box::new(writer))
}

async fn open_control() -> Result<PendingControl, String> {
    #[cfg(unix)]
    if let Ok(control) = unix_control() {
        return Ok(control);
    }
    tcp_control().await
}

#[cfg(unix)]
fn unix_control() -> std::io::Result<PendingControl> {
    use std::os::fd::AsRawFd;
    let (host, child) = std::os::unix::net::UnixStream::pair()?;
    host.set_nonblocking(true)?;
    child.set_nonblocking(false)?;
    let fd = child.as_raw_fd();
    let host = tokio::net::UnixStream::from_std(host)?;
    Ok(PendingControl::Unix {
        host,
        child: Some(child),
        fd,
        token: fresh_token(),
    })
}

async fn tcp_control() -> Result<PendingControl, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("control socket: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("control socket: {error}"))?
        .port();
    let token = fresh_token();
    let token_file = write_token_file(&token)?;
    Ok(PendingControl::Tcp {
        listener,
        port,
        token,
        token_file,
    })
}

fn write_token_file(token: &str) -> Result<TokenFile, String> {
    let bootstrap = bootstrap_path()?;
    let dir = bootstrap
        .parent()
        .ok_or_else(|| "bootstrap directory is missing".to_owned())?;
    let path = dir.join(format!("token-{}", uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|error| format!("control token file: {error}"))?;
    std::io::Write::write_all(&mut file, token.as_bytes())
        .map_err(|error| format!("control token file: {error}"))?;
    Ok(TokenFile(path))
}

impl PendingControl {
    fn env_spec(&self) -> ControlEnv {
        match self {
            #[cfg(unix)]
            Self::Unix { fd, .. } => ControlEnv {
                fd: Some(*fd),
                port: None,
                token_file: None,
            },
            Self::Tcp {
                port, token_file, ..
            } => ControlEnv {
                fd: None,
                port: Some(*port),
                token_file: Some(token_file.0.clone()),
            },
        }
    }

    fn release_local_end(&mut self) {
        #[cfg(unix)]
        if let Self::Unix { child, .. } = self {
            child.take();
        }
    }

    async fn handshake(
        self,
        ctx: &ToolCtx,
        deadline: Instant,
    ) -> Result<(ControlRead, ControlWrite), String> {
        match self {
            #[cfg(unix)]
            Self::Unix { host, token, .. } => {
                let (mut reader, mut writer) = box_split(host);
                // The token crosses the socketpair before exec, so it is not
                // part of the initial environment block /proc keeps forever.
                write_host(
                    &mut writer,
                    &HostFrame::Token {
                        token: token.clone(),
                    },
                )
                .await?;
                expect_ready(&mut reader, &token, ctx, deadline).await?;
                Ok((reader, writer))
            }
            Self::Tcp {
                listener,
                token,
                token_file,
                ..
            } => {
                let io = accept_tcp(listener, &token, ctx, deadline).await;
                drop(token_file);
                io
            }
        }
    }
}

async fn accept_tcp(
    listener: TcpListener,
    token: &str,
    ctx: &ToolCtx,
    deadline: Instant,
) -> Result<(ControlRead, ControlWrite), String> {
    let mut attempts = 0u32;
    loop {
        if attempts >= MAX_HANDSHAKE_ATTEMPTS {
            return Err("control token rejected".to_owned());
        }
        if Instant::now() >= deadline {
            return Err("python did not connect before the startup limit".to_owned());
        }
        let accepted = tokio::select! {
            biased;
            () = ctx.cancel.cancelled() => return Err("program cancelled".to_owned()),
            () = tokio::time::sleep_until(deadline.into()) => {
                return Err("python did not connect before the startup limit".to_owned());
            }
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) =
            accepted.map_err(|error| format!("python did not connect: {error}"))?;
        attempts += 1;
        if !peer.ip().is_loopback() {
            continue;
        }
        stream
            .set_nodelay(true)
            .map_err(|error| format!("control socket: {error}"))?;
        let (mut reader, writer) = box_split(stream);
        let slice = (Instant::now() + HANDSHAKE_ATTEMPT).min(deadline);
        match expect_ready(&mut reader, token, ctx, slice).await {
            Ok(()) => return Ok((reader, writer)),
            Err(message) if message == "program cancelled" => return Err(message),
            Err(_) => continue,
        }
    }
}

async fn expect_ready(
    reader: &mut ControlRead,
    token: &str,
    ctx: &ToolCtx,
    deadline: Instant,
) -> Result<(), String> {
    match read_child_frame(reader, ctx, deadline).await? {
        ChildFrame::Ready { token: got } if tokens_match(&got, token) => Ok(()),
        ChildFrame::Ready { .. } => Err("control token rejected".to_owned()),
        _ => Err("python did not send ready".to_owned()),
    }
}

fn spawn_python(
    interpreter: &PythonInterpreter,
    restricted: bool,
    bootstrap: &PathBuf,
    control: &ControlEnv,
    ctx: &ToolCtx,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<Child, String> {
    let mut command = Command::new(&interpreter.program);
    command.args(&interpreter.prefix);
    if restricted {
        command.arg("-I");
    }
    command.arg("-u");
    command.arg(bootstrap);
    command.current_dir(&ctx.cwd);
    command.env_clear();
    for (key, value) in env {
        command.env(key, value);
    }
    // The token itself is not an environment variable. On Unix the child
    // reads it from the inherited socket. On TCP it reads a 0600 file whose
    // path is the only value that enters the initial environment block.
    if let Some(fd) = control.fd {
        command.env("MYCODE_PTC_FD", fd.to_string());
    }
    if let Some(port) = control.port {
        command.env("MYCODE_PTC_PORT", port.to_string());
    }
    if let Some(path) = &control.token_file {
        command.env("MYCODE_PTC_TOKEN_FILE", path);
    }
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    command.kill_on_drop(true);
    #[cfg(windows)]
    {
        // py.exe is only a launcher; the real python.exe is its child. Both
        // inherit the creation flags, so no console window appears for either.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        command.process_group(0);
        let inherit = control.fd;
        // SAFETY: runs in the forked child before exec. Inherited descriptors
        // other than the control socket are marked close-on-exec so the
        // interpreter cannot use the parent's pipes, listeners, or databases.
        // CLOEXEC is then cleared on that one socket. `prctl` only sets the
        // subreaper flag so grandchildren stay in this tree. None of this
        // allocates, locks, or touches the environment. `prctl` is Linux-only.
        unsafe {
            command.pre_exec(move || {
                seal_inherited_fds()?;
                if let Some(fd) = inherit {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                #[cfg(target_os = "linux")]
                {
                    // A warm interpreter outlives one call. If this host
                    // exits, the kernel kills it even while user code is
                    // blocked. A parent that died before `prctl` has already
                    // reparented us; exit rather than stay up.
                    let rc = libc::prctl(
                        libc::PR_SET_PDEATHSIG,
                        libc::SIGKILL as libc::c_ulong,
                        0,
                        0,
                        0,
                    );
                    if rc == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() == 1 {
                        libc::_exit(1);
                    }
                    let rc = libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
                    if rc == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
    command
        .spawn()
        .map_err(|error| format!("failed to start {}: {error}", interpreter.program.display()))
}

fn bootstrap_path() -> Result<PathBuf, String> {
    static PATH: OnceLockBootstrap = OnceLockBootstrap::new();
    PATH.get()
}

/// Writes the bootstrap into a fresh 0700 directory.
///
/// The name includes a random suffix so a pre-created symlink at a predictable
/// temp path cannot redirect the write. `create_new` refuses to replace an
/// existing file. The script is not a secret (it is compiled into the binary);
/// the directory mode keeps other users from swapping it before exec.
fn install_bootstrap() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!(
        "mycode-ptc-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        // 0o700 survives a typical umask. `set_permissions` below covers a
        // umask that still cleared a bit.
        builder.mode(0o700);
    }
    builder
        .create(&dir)
        .map_err(|error| format!("bootstrap directory: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("bootstrap directory: {error}"))?;
    }
    let path = dir.join("bootstrap.py");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|error| format!("bootstrap file: {error}"))?;
    std::io::Write::write_all(&mut file, include_str!("bootstrap.py").as_bytes())
        .map_err(|error| format!("bootstrap file: {error}"))?;
    Ok(path)
}

struct OnceLockBootstrap(std::sync::OnceLock<Result<PathBuf, String>>);

impl OnceLockBootstrap {
    const fn new() -> Self {
        Self(std::sync::OnceLock::new())
    }

    fn get(&self) -> Result<PathBuf, String> {
        self.0.get_or_init(install_bootstrap).clone()
    }
}

#[cfg(all(unix, target_os = "linux"))]
fn seal_inherited_fds() -> std::io::Result<()> {
    // SAFETY: `close_range` with `CLOSE_RANGE_CLOEXEC` is async-signal-safe.
    // It does not allocate, lock, or close the descriptors, so std's
    // exec-error pipe still works if exec fails. The control fd is included
    // and unmarked by the caller after this returns.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3u32,
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

#[cfg(all(unix, not(target_os = "linux")))]
fn seal_inherited_fds() -> std::io::Result<()> {
    for fd in 3..256 {
        // SAFETY: `fcntl(F_GETFD/F_SETFD)` is async-signal-safe and only
        // toggles close-on-exec. A missing descriptor is skipped.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags == -1 {
            continue;
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

async fn drain_pipe(pipe: impl AsyncReadExt + Unpin, slot: Arc<std::sync::Mutex<String>>) {
    let mut pipe = pipe;
    let mut buf = [0_u8; 4096];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                if let Ok(mut guard) = slot.lock()
                    && guard.len() < 64 * 1024
                {
                    guard.push_str(&String::from_utf8_lossy(&buf[..count]));
                }
            }
        }
    }
}

#[cfg(test)]
mod channel_tests {
    use super::{box_split, encode_frame, expect_ready, tokens_match};
    use crate::ctx::ToolCtx;
    use std::time::{Duration, Instant};
    use tokio::io::AsyncWriteExt;

    #[test]
    fn control_tokens_must_match_exactly() {
        assert!(tokens_match("abc", "abc"));
        assert!(!tokens_match("abc", "abd"));
        assert!(!tokens_match("abc", "ab"));
        assert!(!tokens_match("", ""));
        assert!(!tokens_match("abc", ""));
    }

    #[tokio::test]
    async fn a_wrong_ready_token_is_rejected() {
        let (host, mut child) = tokio::io::duplex(256);
        let (mut reader, _writer) = box_split(host);
        let frame = encode_frame(&serde_json::json!({"type": "ready", "token": "nope"})).unwrap();
        tokio::spawn(async move {
            let _ = child.write_all(&frame).await;
        });
        let ctx = ToolCtx::new(std::env::temp_dir());
        let error = expect_ready(
            &mut reader,
            "secret",
            &ctx,
            Instant::now() + Duration::from_secs(2),
        )
        .await
        .expect_err("bad token");
        assert_eq!(error, "control token rejected");
    }

    #[tokio::test]
    async fn a_matching_ready_token_is_accepted() {
        let (host, mut child) = tokio::io::duplex(256);
        let (mut reader, _writer) = box_split(host);
        let frame = encode_frame(&serde_json::json!({"type": "ready", "token": "secret"})).unwrap();
        tokio::spawn(async move {
            let _ = child.write_all(&frame).await;
        });
        let ctx = ToolCtx::new(std::env::temp_dir());
        expect_ready(
            &mut reader,
            "secret",
            &ctx,
            Instant::now() + Duration::from_secs(2),
        )
        .await
        .expect("token");
    }

    #[tokio::test]
    async fn a_stalled_tcp_peer_does_not_block_the_real_client() {
        use tokio::net::TcpStream;

        let control = super::tcp_control().await.expect("bind");
        let super::PendingControl::Tcp {
            listener,
            port,
            token,
            token_file: _token_file,
        } = control
        else {
            panic!("tcp control did not bind a loopback listener");
        };
        let token_for_child = token.clone();
        let ctx = ToolCtx::new(std::env::temp_dir());
        let deadline = Instant::now() + Duration::from_secs(3);
        let host =
            tokio::spawn(async move { super::accept_tcp(listener, &token, &ctx, deadline).await });
        let _stall = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("stall connect");
        tokio::time::sleep(Duration::from_millis(30)).await;
        let mut good = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("client connect");
        let frame =
            encode_frame(&serde_json::json!({"type": "ready", "token": token_for_child})).unwrap();
        good.write_all(&frame).await.expect("ready");
        let joined = tokio::time::timeout(Duration::from_secs(2), host)
            .await
            .expect("stalled peer held the handshake")
            .expect("task");
        assert!(joined.is_ok(), "{}", joined.err().unwrap_or_default());
    }
}
