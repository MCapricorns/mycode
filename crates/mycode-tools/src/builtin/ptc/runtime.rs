//! One fresh system-Python process per `run_code` invocation.
//!
//! Tool calls arrive on a length-framed socket, not on stdout. Each call is
//! prepared and executed with [`crate::prepare_tool_ctx`], the same preflight
//! a direct call uses. Read-only calls may overlap; every other call waits
//! until the ones submitted before it have finished, then runs alone.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use mycode_core::message::ContentBlock;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

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

const SHELL_NAMES: &[&str] = &["powershell", "bash", "zsh", "sh", "cmd"];
const SAFE_TOOLS: &[&str] = &["read", "grep", "find", "web_search", "fetch_content"];

/// What the program printed and returned, or why it stopped.
pub(super) struct ProgramOutcome {
    pub logs: String,
    pub value: Option<String>,
    pub error: Option<ProgramError>,
    /// Inner calls, for the tool-result details object.
    pub calls: Vec<CallNote>,
}

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
        };
    };
    let restricted = catalog_is_restricted(catalog);
    let tools = sdk_names(catalog);
    match run_child(&interpreter, code, restricted, &tools, ctx, catalog, out).await {
        Ok(outcome) => outcome,
        Err(message) => ProgramOutcome {
            logs: String::new(),
            value: None,
            error: Some(ProgramError {
                kind: "exception",
                message,
            }),
            calls: Vec::new(),
        },
    }
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

async fn run_child(
    interpreter: &PythonInterpreter,
    code: &str,
    restricted: bool,
    tools: &[String],
    parent: &ToolCtx,
    catalog: &ToolCatalog,
    out: &ToolStream,
) -> Result<ProgramOutcome, String> {
    // Cancelling this token stops inner tools without cancelling the turn.
    // A parent interrupt cancels it too, because it is a child token.
    let program_cancel = parent.cancel.child_token();
    let mut ctx = parent.clone();
    ctx.cancel = program_cancel.clone();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("control socket: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("control socket: {error}"))?
        .port();
    let bootstrap = bootstrap_path()?;
    let env = crate::builtin::exec::snapshot_child_environment()
        .map_err(|error| format!("python environment: {error}"))?;
    let mut child = spawn_python(interpreter, restricted, &bootstrap, port, &ctx, &env)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let tree = enroll_spawned(&child).ok();
    let mut running = RunningChild { child, tree };
    let raw_out = Arc::new(std::sync::Mutex::new(String::new()));
    let raw_err = Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(pipe) = stdout {
        let slot = Arc::clone(&raw_out);
        tokio::spawn(drain_pipe(pipe, slot));
    }
    if let Some(pipe) = stderr {
        let slot = Arc::clone(&raw_err);
        tokio::spawn(drain_pipe(pipe, slot));
    }

    let wall = ctx.ptc_wall.unwrap_or(DEFAULT_WALL);
    let deadline = Instant::now() + wall;
    let startup = Instant::now() + STARTUP_TIMEOUT.min(wall);
    let socket = tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => {
            running.kill_tree();
            return Ok(stopped("abort", "program cancelled", String::new(), &[]));
        }
        _ = tokio::time::sleep_until(startup.into()) => {
            running.kill_tree();
            return Ok(stopped(
                "timeout",
                "python did not connect before the startup limit",
                String::new(),
                &[],
            ));
        }
        accepted = listener.accept() => {
            accepted.map(|(stream, _)| stream).map_err(|error| format!("python did not connect: {error}"))?
        }
    };
    socket
        .set_nodelay(true)
        .map_err(|error| format!("control socket: {error}"))?;
    let (mut reader, mut writer) = socket.into_split();
    let ready = read_child_frame(&mut reader, &ctx, deadline).await;
    let ChildFrame::Ready = ready? else {
        running.kill_tree();
        return Ok(stopped(
            "protocol",
            "python did not send ready",
            String::new(),
            &[],
        ));
    };
    write_host(
        &mut writer,
        &HostFrame::Boot {
            code: code.to_owned(),
            tools: tools.to_vec(),
            restricted,
            warn_process: !restricted,
        },
    )
    .await?;

    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<FinishedCall>();
    let mut queue: VecDeque<QueuedCall> = VecDeque::new();
    let mut inflight: Vec<(u64, bool)> = Vec::new();
    let calls = Arc::new(AtomicU32::new(0));
    let mut logs = String::new();
    let mut notes = Vec::new();
    let mut exclusive = false;

    let outcome = loop {
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
                let Some(finished) = finished else { continue };
                inflight.retain(|(id, _)| *id != finished.id);
                if finished.exclusive {
                    exclusive = false;
                }
                notes.push(finished.note);
                if write_host(&mut writer, &finished.reply).await.is_err() {
                    break stopped(
                        "worker-exit",
                        "lost the python process while answering a tool",
                        logs,
                        &notes,
                    );
                }
            }
            incoming = read_child_frame(&mut reader, &ctx, deadline) => {
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
                            if write_host(&mut writer, &reply).await.is_err() {
                                break stopped("worker-exit", "lost the python process", logs, &notes);
                            }
                            continue;
                        }
                        match accept_call(catalog, id, &name, args) {
                            CallDecision::Queue(queued) => queue.push_back(queued),
                            CallDecision::Reject(reply) => {
                                if write_host(&mut writer, &reply).await.is_err() {
                                    break stopped("worker-exit", "lost the python process", logs, &notes);
                                }
                            }
                        }
                    }
                    Ok(ChildFrame::Done { value, error }) => {
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
                        };
                    }
                    Ok(ChildFrame::Ready) => {
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
    };

    program_cancel.cancel();
    running.kill_tree();
    let _ = tokio::time::timeout(Duration::from_secs(2), running.child.wait()).await;
    let mut outcome = outcome;
    append_raw(&mut outcome.logs, &raw_out, "stdout");
    append_raw(&mut outcome.logs, &raw_err, "stderr");
    Ok(outcome)
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

async fn read_child_frame(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
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

async fn write_host(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    frame: &HostFrame,
) -> Result<(), String> {
    let bytes = encode_frame(frame)?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|_| "python closed the control channel".to_owned())
}

fn spawn_python(
    interpreter: &PythonInterpreter,
    restricted: bool,
    bootstrap: &PathBuf,
    port: u16,
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
    command.arg(port.to_string());
    command.current_dir(&ctx.cwd);
    command.env_clear();
    for (key, value) in env {
        command.env(key, value);
    }
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    command.kill_on_drop(true);
    #[cfg(unix)]
    {
        command.process_group(0);
        // SAFETY: runs in the forked child before exec. `prctl` only sets the
        // subreaper flag so grandchildren stay in this tree; it does not
        // allocate, lock, or touch the environment. No-op off Linux.
        unsafe {
            command.pre_exec(|| {
                #[cfg(target_os = "linux")]
                {
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

struct OnceLockBootstrap(std::sync::OnceLock<Result<PathBuf, String>>);

impl OnceLockBootstrap {
    const fn new() -> Self {
        Self(std::sync::OnceLock::new())
    }

    fn get(&self) -> Result<PathBuf, String> {
        self.0
            .get_or_init(|| {
                let path = std::env::temp_dir()
                    .join(format!("mycode-ptc-bootstrap-{}.py", std::process::id()));
                match std::fs::write(&path, include_str!("bootstrap.py")) {
                    Ok(()) => Ok(path),
                    Err(error) => Err(format!("bootstrap file: {error}")),
                }
            })
            .clone()
    }
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
