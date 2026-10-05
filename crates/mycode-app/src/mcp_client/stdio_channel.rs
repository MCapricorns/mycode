//! Stdio transport: newline-delimited JSON-RPC over one child process.
//!
//! Windows needs two things a plain `Command::new(command)` does not do:
//! resolve script launchers such as `npx`/`uvx` (installed as `npx.cmd`,
//! which `CreateProcess` never finds because it only appends `.exe`) and
//! spawn without a console window (the desktop binary runs windowed, so a
//! child console would flash on every connect). Both live in
//! [`configure_command`].

use std::collections::VecDeque;
#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::Mutex;

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
    child: Mutex<Child>,
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    stdout: Mutex<BufReader<tokio::process::ChildStdout>>,
    stderr_tail: Arc<StdMutex<VecDeque<String>>>,
    timeout: Duration,
    /// Process-group id captured at spawn. Shutdown signals the group.
    #[cfg(unix)]
    group_id: Option<i32>,
    /// Kill-on-close job that owns the child and its descendants.
    #[cfg(windows)]
    job: Option<WindowsJob>,
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
        if !mycode_config::is_mcp_executable(command) {
            return Err(McpError::transport(
                "MCP command must be an executable path, not a shell string".to_owned(),
            ));
        }
        let program = resolve_program(command).ok_or_else(|| {
            McpError::transport(format!(
                "command not found on PATH: {command} (install it or use an absolute path)"
            ))
        })?;
        let mut builder = tokio::process::Command::new(&program);
        builder
            .args(args)
            .env_clear()
            .envs(stdio_child_env(env))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure_command(&mut builder);
        let mut child = builder.spawn().map_err(|error| {
            McpError::transport(format!("failed to start {}: {error}", program.display()))
        })?;
        #[cfg(unix)]
        let group_id = child.id().map(|pid| pid as i32);
        #[cfg(windows)]
        let job = assign_windows_job(&mut child)?;
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
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let line: String = line.chars().take(STDERR_LINE_CHARS).collect();
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
            });
        }
        Ok(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(Some(stdin)),
            stdout: Mutex::new(BufReader::new(stdout)),
            stderr_tail,
            timeout,
            #[cfg(unix)]
            group_id,
            #[cfg(windows)]
            job: Some(job),
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
        if let Some(group_id) = self.group_id {
            // SAFETY: `group_id` is the child's pid, and `configure_command`
            // placed that pid in its own process group. `killpg` signals
            // every member, including grandchildren the parent spawned.
            unsafe {
                libc::killpg(group_id, libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            let _ = job.terminate();
        }
        let _ = child.kill().await;
    }

    /// Reads one stdout line. Tests use this to observe a grandchild pid.
    #[cfg(test)]
    async fn read_stdout_line(&self) -> Result<String, McpError> {
        let mut raw = String::new();
        self.stdout
            .lock()
            .await
            .read_line(&mut raw)
            .await
            .map_err(|error| McpError::transport(error.to_string()))?;
        Ok(raw)
    }

    /// Renders the retained stderr tail for an error message.
    fn stderr_context(&self) -> String {
        let Ok(tail) = self.stderr_tail.lock() else {
            return String::new();
        };
        if tail.is_empty() {
            return String::new();
        }
        format!(
            "; stderr: {}",
            tail.iter().cloned().collect::<Vec<_>>().join(" | ")
        )
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
        loop {
            let mut raw = String::new();
            let count = self
                .stdout
                .lock()
                .await
                .read_line(&mut raw)
                .await
                .map_err(|error| {
                    McpError::transport(format!("read from server failed: {error}"))
                })?;
            if count == 0 {
                return Err(McpError::transport(self.closed_reason().await));
            }
            if raw.len() > MAX_MESSAGE_BYTES {
                return Err(McpError::Oversized);
            }
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Servers occasionally log to stdout; a non-JSON line is noise,
            // not a protocol failure.
            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
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

#[async_trait::async_trait]
impl JsonRpcChannel for StdioChannel {
    async fn request(&self, id: u64, method: &str, params: Value) -> Result<Value, McpError> {
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
        self.write_line(&line).await?;
        match tokio::time::timeout(self.timeout, self.wait_for(id)).await {
            Ok(outcome) => outcome,
            Err(_) => Err(McpError::Timeout),
        }
    }

    async fn notify(&self, method: &str) -> Result<(), McpError> {
        let line = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": method,
        }))
        .map_err(|error| McpError::protocol(error.to_string()))?;
        self.write_line(&line).await
    }

    async fn shutdown(&self) {
        StdioChannel::shutdown(self).await;
    }
}

/// Resolves `command` to something the OS can launch.
///
/// A command carrying a path separator is used as written. A bare name is
/// searched on `PATH`; on Windows every `PATHEXT` extension is tried per
/// directory in order, so `npx` resolves to `npx.cmd` and `uvx` to
/// `uvx.exe`. `None` means nothing matched.
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
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    let extensions: Vec<String> = pathext
        .split(';')
        .filter(|ext| !ext.is_empty())
        .map(|ext| ext.to_ascii_lowercase())
        .collect();
    let has_extension = Path::new(command).extension().is_some();
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if has_extension {
            let candidate = dir.join(command);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        for ext in &extensions {
            let candidate = dir.join(format!("{command}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Environment variables a stdio MCP child may inherit.
const STDIO_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
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

/// Platform launch flags: own process group on Unix, no console on Windows.
fn configure_command(builder: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        builder.process_group(0);
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        builder.creation_flags(CREATE_NO_WINDOW);
    }
}

#[cfg(windows)]
struct WindowsJob {
    handle: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl WindowsJob {
    fn new() -> std::io::Result<Self> {
        use std::mem::size_of;
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        // SAFETY: both pointers are null, requesting an unnamed job. A
        // non-null result is uniquely owned.
        let raw_job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw_job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a fresh owned HANDLE.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw_job) };
        let job = Self { handle };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let info_len = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .expect("job limit structure fits u32");
        // SAFETY: the job handle is live and `info` matches the documented type.
        let configured = unsafe {
            SetInformationJobObject(
                job.raw(),
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

    fn assign(&self, process: windows_sys::Win32::Foundation::HANDLE) -> std::io::Result<()> {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        // SAFETY: both handles are live for this call.
        if unsafe { AssignProcessToJobObject(self.raw(), process) } != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn terminate(&self) -> std::io::Result<()> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        // SAFETY: the owned job handle remains live for this call.
        if unsafe { TerminateJobObject(self.raw(), 1) } != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn raw(&self) -> windows_sys::Win32::Foundation::HANDLE {
        use std::os::windows::io::AsRawHandle;
        self.handle.as_raw_handle()
    }
}

#[cfg(windows)]
fn assign_windows_job(child: &mut Child) -> Result<WindowsJob, McpError> {
    use std::os::windows::io::AsRawHandle;
    let job = WindowsJob::new().map_err(|error| {
        McpError::transport(format!("failed to create MCP job object: {error}"))
    })?;
    if let Err(error) = job.assign(child.as_raw_handle()) {
        let _ = child.start_kill();
        return Err(McpError::transport(format!(
            "failed to assign MCP process to a job: {error}"
        )));
    }
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::{StdioChannel, filter_stdio_env};
    use std::time::Duration;

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
    async fn shutdown_kills_the_process_group() {
        let channel = StdioChannel::spawn(
            "/bin/sh",
            &[
                "-c".to_owned(),
                "sleep 120 & echo $!; exec sleep 120".to_owned(),
            ],
            &[],
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        let line = channel.read_stdout_line().await.unwrap();
        let grandchild: i32 = line.trim().parse().unwrap();
        channel.shutdown().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{grandchild}/status")) {
            let state = status
                .lines()
                .find(|line| line.starts_with("State:"))
                .unwrap_or("");
            assert!(
                state.contains('Z'),
                "grandchild {grandchild} still running: {state}"
            );
        }
    }
}
