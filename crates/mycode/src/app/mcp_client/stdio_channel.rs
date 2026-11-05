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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(not(windows))]
use tokio::process::Child;
use tokio::sync::Mutex;

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

/// One live stdio server session.
///
/// The child is killed when the channel drops; stdout stays bounded by
/// [`MAX_MESSAGE_BYTES`] per line and the stderr tail is retained for error
/// messages.
pub struct StdioChannel {
    child: Mutex<HostChild>,
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    stdout: Mutex<BufReader<tokio::process::ChildStdout>>,
    stderr_tail: Arc<StdMutex<VecDeque<String>>>,
    timeout: Duration,
    tools_changed: AtomicBool,
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
        Ok(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(Some(stdin)),
            stdout: Mutex::new(BufReader::new(stdout)),
            stderr_tail,
            timeout,
            tools_changed: AtomicBool::new(false),
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

    async fn write_line(&self, line: &[u8]) -> Result<(), McpError> {
        let mut guard = self.stdin.lock().await;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| McpError::transport("server stdin is closed"))?;
        let write = async {
            stdin.write_all(line).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await
        };
        write
            .await
            .map_err(|error| McpError::transport(format!("write to server failed: {error}")))
    }

    async fn wait_for(&self, id: u64) -> Result<Value, McpError> {
        let mut raw = Vec::new();
        loop {
            let read =
                read_bounded_line(&mut *self.stdout.lock().await, &mut raw, MAX_MESSAGE_BYTES)
                    .await
                    .map_err(|error| {
                        McpError::transport(format!("read from server failed: {error}"))
                    })?;
            match read {
                LineRead::Eof => return Err(McpError::transport(self.closed_reason().await)),
                LineRead::Oversized => return Err(McpError::Oversized),
                LineRead::Line => {}
            }
            // Servers occasionally log to stdout; a non-JSON line is noise,
            // not a protocol failure.
            let Ok(text) = std::str::from_utf8(&raw) else {
                continue;
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            if value["method"].as_str() == Some("notifications/tools/list_changed")
                && value["id"].is_null()
            {
                self.tools_changed.store(true, Ordering::Relaxed);
            }
            if value["method"].is_string() && !value["id"].is_null() {
                self.answer_server_request(&value).await?;
                continue;
            }
            if value["id"].as_u64() != Some(id) {
                // Notifications and stale replies are skipped.
                continue;
            }
            if let Some(error) = value.get("error").filter(|error| error.is_object()) {
                return Err(McpError::server(error));
            }
            return Ok(value["result"].clone());
        }
    }

    /// Replies to a server-initiated request so the server never blocks on
    /// a capability this client does not offer.
    async fn answer_server_request(&self, request: &Value) -> Result<(), McpError> {
        let reply = if request["method"].as_str() == Some("ping") {
            json!({"jsonrpc": "2.0", "id": request["id"], "result": {}})
        } else {
            json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "error": {"code": -32601, "message": "method not supported by this client"},
            })
        };
        let line =
            serde_json::to_vec(&reply).map_err(|error| McpError::protocol(error.to_string()))?;
        self.write_line(&line).await
    }
}

impl Drop for StdioChannel {
    fn drop(&mut self) {
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
                self.write_line(&line).await?;
                self.wait_for(id).await
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
            tokio::time::timeout(self.timeout, self.write_line(&line))
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
        let newline = available.iter().position(|byte| *byte == b'\n');
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
