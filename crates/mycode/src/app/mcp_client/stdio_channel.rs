//! Stdio transport: newline-delimited JSON-RPC over one child process.
//!
//! Windows needs two things a plain `Command::new(command)` does not do:
//! resolve script launchers such as `npx`/`uvx` (installed as `npx.cmd`,
//! which `CreateProcess` never finds because it only appends `.exe`) and
//! spawn without a console window (the desktop binary runs windowed, so a
//! child console would flash on every connect). Resolution lives in
//! [`resolve_program`]. The process is created suspended, placed in a
//! kill-on-close Job, and resumed from `PROCESS_INFORMATION.hThread`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Stdio;
#[cfg(unix)]
use std::sync::atomic::AtomicI32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
#[cfg(not(windows))]
use tokio::process::Child;
use tokio::sync::{Mutex, oneshot};

#[cfg(windows)]
use crate::tools::WindowsStdioProcess;

#[cfg(windows)]
type HostChild = WindowsStdioProcess;
#[cfg(not(windows))]
type HostChild = Child;

use super::{JsonRpcChannel, MAX_MESSAGE_BYTES, McpError};

/// Stderr lines kept for diagnostics when the server fails.
const STDERR_TAIL_LINES: usize = 12;
/// Longest stderr line kept verbatim.
const STDERR_LINE_CHARS: usize = 400;
/// Grace period between closing stdin and killing the child on shutdown.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(1500);

#[derive(Clone, Debug)]
enum ReaderFailure {
    Closed,
    Error(McpError),
}

type PendingResponse = oneshot::Sender<Result<Value, ReaderFailure>>;
type ResponseReceiver = oneshot::Receiver<Result<Value, ReaderFailure>>;

#[derive(Default)]
struct ReplyState {
    pending: Option<(u64, PendingResponse)>,
    failure: Option<ReaderFailure>,
}

/// Only the current request may retain a reply. Notifications and stale ids
/// never queue up behind an idle client or consume unbounded memory.
#[derive(Default)]
struct ReplyRouter {
    state: StdMutex<ReplyState>,
}

impl ReplyRouter {
    fn register(
        self: &Arc<Self>,
        id: u64,
    ) -> Result<(ResponseReceiver, PendingReply), ReaderFailure> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(failure) = &state.failure {
            return Err(failure.clone());
        }
        let (sender, receiver) = oneshot::channel();
        state.pending = Some((id, sender));
        Ok((
            receiver,
            PendingReply {
                id,
                router: Arc::clone(self),
            },
        ))
    }

    fn deliver(&self, value: Value) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .pending
            .as_ref()
            .is_some_and(|(id, _)| value["id"].as_u64() == Some(*id))
            && let Some((_, sender)) = state.pending.take()
        {
            let _ = sender.send(Ok(value));
        }
    }

    fn fail(&self, failure: ReaderFailure) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.failure.is_none() {
            state.failure = Some(failure.clone());
        }
        if let Some((_, sender)) = state.pending.take() {
            let _ = sender.send(Err(failure));
        }
    }
}

/// A timed-out, cancelled, or failed write cannot leave an abandoned waiter.
struct PendingReply {
    id: u64,
    router: Arc<ReplyRouter>,
}

impl Drop for PendingReply {
    fn drop(&mut self) {
        let mut state = self
            .router
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.pending.as_ref().is_some_and(|(id, _)| *id == self.id) {
            state.pending = None;
        }
    }
}

/// One live stdio server session.
///
/// The child is killed when the channel drops; stdout stays bounded by
/// [`MAX_MESSAGE_BYTES`] per line and the stderr tail is retained for error
/// messages.
pub struct StdioChannel {
    child: Mutex<HostChild>,
    stdin: Arc<Mutex<Option<tokio::process::ChildStdin>>>,
    /// One exchange at a time; notifications continue while no exchange runs.
    exchange: Mutex<()>,
    replies: Arc<ReplyRouter>,
    stdout_reader: tokio::task::JoinHandle<()>,
    stderr_tail: Arc<StdMutex<VecDeque<String>>>,
    timeout: Duration,
    tools_changed: Arc<AtomicBool>,
    /// Process-group id captured at spawn. Shutdown signals the group.
    #[cfg(unix)]
    group_id: AtomicI32,
}

impl StdioChannel {
    /// Spawns one server command with extra environment variables.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::Transport`] when the command cannot be resolved or
    /// started, or its pipes are missing.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<Self, McpError> {
        if !crate::config::is_mcp_executable(command) {
            return Err(McpError::transport(
                "MCP command must be an executable path, not a shell string".to_owned(),
            ));
        }
        let program = resolve_program(command).ok_or_else(|| {
            McpError::transport(format!(
                "command not found on PATH: {command} (install it or use an absolute path)"
            ))
        })?;
        let mut child = launch_stdio_process(&program, args, env)?;
        #[cfg(unix)]
        let group_id = AtomicI32::new(child.id().map_or(0, |pid| pid as i32));
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::transport("child stdin pipe missing"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::transport("child stdout pipe missing"))?;
        let stderr_tail = Arc::new(StdMutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                collect_stderr(BufReader::new(stderr), &tail).await;
            });
        }
        let stdin = Arc::new(Mutex::new(Some(stdin)));
        let replies = Arc::new(ReplyRouter::default());
        let tools_changed = Arc::new(AtomicBool::new(false));
        let stdout_reader = tokio::spawn(read_stdout(
            BufReader::new(stdout),
            Arc::clone(&stdin),
            Arc::clone(&replies),
            Arc::clone(&tools_changed),
            timeout,
        ));
        Ok(Self {
            child: Mutex::new(child),
            stdin,
            exchange: Mutex::new(()),
            replies,
            stdout_reader,
            stderr_tail,
            timeout,
            tools_changed,
            #[cfg(unix)]
            group_id,
        })
    }

    /// Closes stdin, waits briefly for a clean exit, then kills the child.
    ///
    /// The MCP stdio contract asks clients to close stdin first so the
    /// server can exit on its own; `.cmd` launchers on Windows spawn the
    /// real server as a grandchild that only a stdin EOF reaches.
    pub async fn shutdown(&self) {
        self.stdout_reader.abort();
        self.replies.fail(ReaderFailure::Closed);
        self.stdin.lock().await.take();
        let mut child = self.child.lock().await;
        let _ = tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await;
        #[cfg(unix)]
        self.kill_process_group();
        #[cfg(windows)]
        {
            let _ = child.terminate_job();
        }
        let _ = child.kill().await;
    }

    /// A pool replacement or failed/cancelled handshake drops the channel
    /// without calling `shutdown`. Child.kill_on_drop only kills the launcher,
    /// so the group must also be released on that path.
    #[cfg(unix)]
    fn kill_process_group(&self) {
        let group_id = self.group_id.swap(0, Ordering::Relaxed);
        if group_id > 0 {
            // SAFETY: `group_id` is the child's pid, and `configure_command`
            // placed that pid in its own process group. `killpg` signals
            // every member, including grandchildren the parent spawned.
            unsafe {
                libc::killpg(group_id, libc::SIGKILL);
            }
        }
    }

    /// Renders the retained stderr tail for an error message.
    fn stderr_context(&self) -> String {
        let Ok(tail) = self.stderr_tail.lock() else {
            return String::new();
        };
        if tail.is_empty() {
            return String::new();
        }
        let mut joined = String::new();
        for (index, line) in tail.iter().enumerate() {
            if index > 0 {
                joined.push_str(" | ");
            }
            joined.push_str(line);
        }
        format!("; stderr: {joined}")
    }

    /// Describes why stdout closed: exit status plus the stderr tail.
    async fn closed_reason(&self) -> String {
        let status = match self.child.lock().await.try_wait() {
            Ok(Some(status)) => format!("server exited with {status}"),
            Ok(None) => "server closed its stdout".to_owned(),
            Err(_) => "server state unknown".to_owned(),
        };
        format!("{status}{}", self.stderr_context())
    }
}

impl Drop for StdioChannel {
    fn drop(&mut self) {
        self.stdout_reader.abort();
        #[cfg(unix)]
        self.kill_process_group();
    }
}

impl JsonRpcChannel for StdioChannel {
    fn take_tools_changed(&self) -> bool {
        self.tools_changed.swap(false, Ordering::Relaxed)
    }

    fn request<'a>(
        &'a self,
        id: u64,
        method: &'a str,
        params: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpError>> + Send + 'a>> {
        Box::pin(async move {
            let line = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
            }))
            .map_err(|error| McpError::protocol(error.to_string()))?;
            if line.len() > MAX_MESSAGE_BYTES {
                return Err(McpError::Oversized);
            }
            // The deadline includes backpressure on stdin: a server that
            // stops reading must not hang the turn before reply waiting starts.
            let exchange = async {
                let _exchange = self.exchange.lock().await;
                let (reply, _pending) = match self.replies.register(id) {
                    Ok(pending) => pending,
                    Err(ReaderFailure::Closed) => {
                        return Err(McpError::transport(self.closed_reason().await));
                    }
                    Err(ReaderFailure::Error(error)) => return Err(error),
                };
                write_line(&self.stdin, &line).await?;
                match reply.await {
                    Ok(Ok(mut value)) => {
                        if let Some(error) = value.get("error").filter(|error| error.is_object()) {
                            Err(McpError::server(error))
                        } else {
                            Ok(value["result"].take())
                        }
                    }
                    Ok(Err(ReaderFailure::Error(error))) => Err(error),
                    Ok(Err(ReaderFailure::Closed)) | Err(_) => {
                        Err(McpError::transport(self.closed_reason().await))
                    }
                }
            };
            match tokio::time::timeout(self.timeout, exchange).await {
                Ok(outcome) => outcome,
                Err(_) => Err(McpError::Timeout),
            }
        })
    }

    fn notify<'a>(
        &'a self,
        method: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), McpError>> + Send + 'a>> {
        Box::pin(async move {
            let line = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": method,
            }))
            .map_err(|error| McpError::protocol(error.to_string()))?;
            tokio::time::timeout(self.timeout, write_line(&self.stdin, &line))
                .await
                .map_err(|_| McpError::Timeout)?
        })
    }

    fn shutdown<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            StdioChannel::shutdown(self).await;
        })
    }
}

async fn write_line<W>(stdin: &Mutex<Option<W>>, line: &[u8]) -> Result<(), McpError>
where
    W: AsyncWrite + Unpin,
{
    let mut guard = stdin.lock().await;
    // Keep the pipe out of the shared slot until a complete frame is written.
    // An error or cancellation drops it before another writer can append bytes.
    let mut stdin = guard
        .take()
        .ok_or_else(|| McpError::transport("server stdin is closed"))?;
    let write = async {
        stdin.write_all(line).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await
    };
    write
        .await
        .map_err(|error| McpError::transport(format!("write to server failed: {error}")))?;
    *guard = Some(stdin);
    Ok(())
}

/// Continuously consumes stdout, including notifications between RPCs.
/// Server-initiated requests receive bounded replies even while the client
/// is idle. A failed read/write wakes the waiter and rejects later requests.
async fn read_stdout<R, W>(
    mut stdout: R,
    stdin: Arc<Mutex<Option<W>>>,
    replies: Arc<ReplyRouter>,
    tools_changed: Arc<AtomicBool>,
    timeout: Duration,
) where
    R: tokio::io::AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut raw = Vec::new();
    let failure = loop {
        match read_bounded_line(&mut stdout, &mut raw, MAX_MESSAGE_BYTES).await {
            Ok(LineRead::Eof) => break ReaderFailure::Closed,
            Ok(LineRead::Oversized) => break ReaderFailure::Error(McpError::Oversized),
            Ok(LineRead::Line) => {}
            Err(error) => {
                break ReaderFailure::Error(McpError::transport(format!(
                    "read from server failed: {error}"
                )));
            }
        }
        // Some servers log to stdout; invalid JSON lines remain ignorable.
        let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
            continue;
        };
        if value["method"].as_str() == Some("notifications/tools/list_changed")
            && value["id"].is_null()
        {
            tools_changed.store(true, Ordering::Relaxed);
        }
        if value["method"].is_string() && !value["id"].is_null() {
            let reply = if value["method"].as_str() == Some("ping") {
                json!({"jsonrpc": "2.0", "id": value["id"], "result": {}})
            } else {
                json!({
                    "jsonrpc": "2.0",
                    "id": value["id"],
                    "error": {"code": -32601, "message": "method not supported by this client"},
                })
            };
            let line = match serde_json::to_vec(&reply) {
                Ok(line) => line,
                Err(error) => break ReaderFailure::Error(McpError::protocol(error.to_string())),
            };
            if line.len() > MAX_MESSAGE_BYTES {
                break ReaderFailure::Error(McpError::Oversized);
            }
            match tokio::time::timeout(timeout, write_line(&stdin, &line)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => break ReaderFailure::Error(error),
                Err(_) => break ReaderFailure::Error(McpError::Timeout),
            }
        } else {
            replies.deliver(value);
        }
    };
    replies.fail(failure);
}

/// Drain diagnostic lines without buffering an unbounded line from the child.
/// Four UTF-8 bytes per character preserve the displayed prefix for valid text.
async fn collect_stderr<R>(mut reader: R, tail: &StdMutex<VecDeque<String>>)
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut raw = Vec::new();
    while let Ok(LineRead::Line | LineRead::Oversized) =
        read_bounded_line(&mut reader, &mut raw, STDERR_LINE_CHARS * 4).await
    {
        let line: String = String::from_utf8_lossy(&raw)
            .trim_end_matches(['\r', '\n'])
            .chars()
            .take(STDERR_LINE_CHARS)
            .collect();
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(mut tail) = tail.lock() {
            if tail.len() == STDERR_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    }
}

/// Outcome of one [`read_bounded_line`].
#[derive(Debug, PartialEq, Eq)]
enum LineRead {
    /// `line` holds one line (its newline included when present).
    Line,
    /// The line was longer than the limit. It was consumed through its
    /// newline with only its prefix kept, so the next read starts on a fresh line.
    Oversized,
    /// The stream ended before any byte of a new line.
    Eof,
}

/// Reads one newline-terminated line of at most `limit` bytes into `line`.
///
/// `read_line` buffers a whole line before its length can be checked, so a
/// server that writes one huge line (or never a newline) could grow memory
/// for a full request timeout. This keeps at most `limit` bytes.
async fn read_bounded_line<R>(
    reader: &mut R,
    line: &mut Vec<u8>,
    limit: usize,
) -> std::io::Result<LineRead>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    line.clear();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if oversized {
                LineRead::Oversized
            } else if line.is_empty() {
                LineRead::Eof
            } else {
                LineRead::Line
            });
        }
        let newline = memchr::memchr(b'\n', available);
        let used = newline.map_or(available.len(), |index| index + 1);
        if !oversized {
            if line.len() + used > limit {
                line.extend_from_slice(&available[..limit - line.len()]);
                oversized = true;
            } else {
                line.extend_from_slice(&available[..used]);
            }
        }
        reader.consume(used);
        if newline.is_some() {
            return Ok(if oversized {
                LineRead::Oversized
            } else {
                LineRead::Line
            });
        }
    }
}

/// Resolves `command` to something the OS can launch.
///
/// A command carrying a path separator is used as written. A bare name is
/// searched on `PATH`; on Windows the launchable `PATHEXT` extensions are
/// tried per directory in order (`.com`, `.exe`, `.bat`, `.cmd`), so `npx`
/// resolves to `npx.cmd` and `uvx` to `uvx.exe`. `None` means nothing matched.
fn resolve_program(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    if command.contains(['/', '\\']) {
        return Some(PathBuf::from(command));
    }
    #[cfg(windows)]
    {
        resolve_on_path_windows(command)
    }
    #[cfg(not(windows))]
    {
        // Unix `execvp` searches PATH itself; keep the bare name so a
        // shell-installed shim behaves exactly as in a terminal.
        Some(PathBuf::from(command))
    }
}

#[cfg(windows)]
fn resolve_on_path_windows(command: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let pathext = std::env::var("PATHEXT").unwrap_or_default();
    crate::tools::first_program_on_path(command, &path_var, &pathext)
}

/// Environment variables a stdio MCP child may inherit: the MCP SDK's
/// `getDefaultEnvironment` set (npm and Python servers on Windows look up
/// `APPDATA` / `LOCALAPPDATA` for their caches) plus locale and temp dirs.
const STDIO_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "USERNAME",
    "LOGNAME",
    "SHELL",
    "TERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "TMPDIR",
    "TMP",
    "TEMP",
    "SYSTEMROOT",
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "SYSTEMDRIVE",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMFILES",
    "PROCESSOR_ARCHITECTURE",
];

/// Allowlisted process environment, then the server's own map.
fn stdio_child_env(extra: &[(String, String)]) -> Vec<(String, String)> {
    filter_stdio_env(&std::env::vars().collect::<Vec<_>>(), extra)
}

fn filter_stdio_env(
    process: &[(String, String)],
    extra: &[(String, String)],
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for key in STDIO_ENV_ALLOWLIST {
        if let Some((name, value)) = process
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
        {
            out.push((name.clone(), value.clone()));
        }
    }
    for (key, value) in extra {
        if let Some(slot) = out
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
        {
            slot.1.clone_from(value);
        } else {
            out.push((key.clone(), value.clone()));
        }
    }
    out
}

/// Platform launch flags.
///
/// Unix puts the child in its own process group so shutdown can signal the
/// whole group.
///
/// Windows does not use this function. [`launch_stdio_process`] creates the
/// process suspended, assigns a kill-on-close Job, and resumes
/// `PROCESS_INFORMATION.hThread`.
#[cfg(not(windows))]
fn configure_command(builder: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        builder.process_group(0);
    }
}

fn launch_stdio_process(
    program: &Path,
    args: &[String],
    env: &[(String, String)],
) -> Result<HostChild, McpError> {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        let child_env: Vec<(OsString, OsString)> = stdio_child_env(env)
            .into_iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect();
        // `.cmd` launchers spawn the real server as a grandchild. The Job is
        // assigned while the process is still suspended, then the main thread
        // from CreateProcessW is resumed.
        WindowsStdioProcess::spawn(program, args, None, &child_env, true, true).map_err(|error| {
            McpError::transport(format!("failed to start {}: {error}", program.display()))
        })
    }
    #[cfg(not(windows))]
    {
        let mut builder = tokio::process::Command::new(program);
        builder
            .args(args)
            .env_clear()
            .envs(stdio_child_env(env))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure_command(&mut builder);
        builder.spawn().map_err(|error| {
            McpError::transport(format!("failed to start {}: {error}", program.display()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::filter_stdio_env;

    #[tokio::test]
    async fn idle_notifications_and_server_pings_do_not_wait_for_an_rpc() {
        use super::{ReplyRouter, read_stdout};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

        let (client, server) = tokio::io::duplex(4096);
        let (stdout, stdin) = tokio::io::split(client);
        let replies = Arc::new(ReplyRouter::default());
        let changed = Arc::new(AtomicBool::new(false));
        let reader = tokio::spawn(read_stdout(
            BufReader::new(stdout),
            Arc::new(tokio::sync::Mutex::new(Some(stdin))),
            Arc::clone(&replies),
            Arc::clone(&changed),
            Duration::from_secs(2),
        ));
        let mut server = BufReader::new(server);
        server
            .get_mut()
            .write_all(
                concat!(
                    "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n",
                    "{\"jsonrpc\":\"2.0\",\"id\":\"server-ping\",\"method\":\"ping\"}\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), server.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let pong: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(pong["id"], "server-ping");
        assert!(pong["result"].is_object());
        assert!(changed.swap(false, Ordering::Relaxed));
        assert!(!changed.load(Ordering::Relaxed));

        // A normal reply still reaches its caller after idle notifications,
        // while unsolicited or stale ids never occupy a response queue.
        let (reply, _pending) = replies.register(41).unwrap();
        server
            .get_mut()
            .write_all(
                concat!(
                    "{\"jsonrpc\":\"2.0\",\"id\":40,\"result\":\"stale\"}\n",
                    "{\"jsonrpc\":\"2.0\",\"id\":41,\"result\":{\"ok\":true}}\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), reply)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result["result"]["ok"], true);
        reader.abort();
        let _ = reader.await;
    }

    #[tokio::test]
    async fn a_dropped_exchange_retires_its_waiter_and_skips_a_late_reply() {
        let replies = std::sync::Arc::new(super::ReplyRouter::default());
        let (first, pending) = replies.register(1).unwrap();
        drop(pending);
        assert!(first.await.is_err());
        let (second, _pending) = replies.register(2).unwrap();
        replies.deliver(serde_json::json!({"id": 1, "result": "cancelled"}));
        replies.deliver(serde_json::json!({"id": 2, "result": "current"}));
        assert_eq!(second.await.unwrap().unwrap()["result"], "current");
    }

    #[tokio::test]
    async fn a_cancelled_frame_write_closes_stdin() {
        use tokio::io::AsyncReadExt as _;

        let (client, mut server) = tokio::io::duplex(4);
        let stdin = tokio::sync::Mutex::new(Some(client));
        tokio::select! {
            biased;
            result = super::write_line(&stdin, b"first frame") => {
                panic!("write should block on a full pipe: {result:?}");
            }
            () = std::future::ready(()) => {}
        }
        assert!(stdin.try_lock().unwrap().is_none());
        let mut received = Vec::new();
        server.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, b"firs");
    }

    #[tokio::test]
    async fn an_oversized_idle_line_fails_pending_and_future_exchanges() {
        let replies = std::sync::Arc::new(super::ReplyRouter::default());
        let (reply, _pending) = replies.register(1).unwrap();
        let mut bytes = vec![b'x'; super::MAX_MESSAGE_BYTES + 1];
        bytes.push(b'\n');
        super::read_stdout(
            bytes.as_slice(),
            std::sync::Arc::new(tokio::sync::Mutex::new(Some(tokio::io::sink()))),
            std::sync::Arc::clone(&replies),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            std::time::Duration::from_secs(2),
        )
        .await;
        assert!(matches!(
            reply.await.unwrap(),
            Err(super::ReaderFailure::Error(super::McpError::Oversized))
        ));
        assert!(matches!(
            replies.register(2),
            Err(super::ReaderFailure::Error(super::McpError::Oversized))
        ));
    }

    #[tokio::test]
    async fn stderr_keeps_only_the_latest_nonempty_lines() {
        use super::{STDERR_TAIL_LINES, collect_stderr};
        use std::collections::VecDeque;
        use std::sync::Mutex;

        let input = (0..30).map(|n| format!("{n}\n\n")).collect::<String>();
        let tail = Mutex::new(VecDeque::new());
        collect_stderr(input.as_bytes(), &tail).await;
        let tail = tail.lock().unwrap();
        assert_eq!(tail.len(), STDERR_TAIL_LINES);
        assert_eq!(tail.front().unwrap(), "18");
        assert_eq!(tail.back().unwrap(), "29");
    }

    #[test]
    fn stdio_env_keeps_path_and_drops_secrets() {
        let process = vec![
            ("PATH".to_owned(), "/usr/bin".to_owned()),
            ("SECRET".to_owned(), "leak".to_owned()),
            ("Path".to_owned(), "unused".to_owned()),
        ];
        let filtered = filter_stdio_env(&process, &[("TOKEN".to_owned(), "ok".to_owned())]);
        assert!(
            filtered
                .iter()
                .any(|(key, value)| key == "PATH" && value == "/usr/bin")
        );
        assert!(
            filtered
                .iter()
                .all(|(key, _)| !key.eq_ignore_ascii_case("SECRET"))
        );
        assert!(
            filtered
                .iter()
                .any(|(key, value)| key == "TOKEN" && value == "ok")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tool_list_notifications_are_consumed_without_losing_the_reply() {
        use super::JsonRpcChannel as _;
        let script = r#"read request
printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}' '{"jsonrpc":"2.0","id":1,"result":{"ok":true}}'
read next
"#;
        let channel = super::StdioChannel::spawn(
            "/bin/sh",
            &["-c".to_owned(), script.to_owned()],
            &[],
            std::time::Duration::from_secs(2),
        )
        .await
        .unwrap();
        let reply = channel
            .request(1, "tools/call", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(reply["ok"], true);
        assert!(channel.take_tools_changed());
        assert!(!channel.take_tools_changed());
        channel.shutdown().await;
    }
}
