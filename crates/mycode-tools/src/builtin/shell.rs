//! `shell` — the only process-launch tool.
//!
//! Two modes share one name, one schema, and one prompt entry:
//!
//! * `script` runs `command` in the platform shell (pipelines, redirection,
//!   expansion, compound scripts, and file edits through Python or another
//!   interpreter). Windows resolves one configured or detected shell (`pwsh`,
//!   then Git bash). When neither is available it falls back to
//!   `%SystemRoot%\System32\cmd.exe` at runtime only; that fallback is not
//!   written into settings. POSIX hosts use an explicit POSIX shell candidate
//!   list. There is no `bash` tool alias.
//! * `program` spawns `program` with an explicit `args` vector and does not
//!   start a shell. Only a kernel-loadable PE, ELF, or Mach-O image is
//!   accepted. Shebang scripts and batch files are rejected.
//!
//! Both modes pin the launched image, snapshot cwd/env/PATH once per call,
//! allowlist the child environment, and use contained spawn. Candidate
//! fallback is allowed only for a typed executable-not-found result on the
//! script path. Execution is unsandboxed current-user file and network
//! authority; environment filtering is not a sandbox. Valid calls run
//! directly with no Core permission prompt. `write` and `edit` stay
//! available. `read`, `grep`, and `find` stay in-process. File edits from
//! either path are not undone.
use std::path::Path;
use std::time::{Duration, Instant};

#[path = "shell_detect.rs"]
mod detect;

use detect::runtime_shell;

pub use detect::{
    DetectedShell, ShellKind, detect_default_shell, detect_shell_kind, set_runtime_shell,
};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

#[cfg(windows)]
use base64::Engine as _;
#[cfg(windows)]
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use crate::builtin::blocking::run_blocking_supervised;
use crate::builtin::exec::{
    ExecutionMetadata, PreparedIdentity, PreparedInvocation, ResolveError, RunOutcome,
    apply_execution_details, prepare_from_snapshot, prepared_identity, run_prepared,
    snapshot_child_environment,
};
use crate::builtin::process::{
    CapturedStream, ExecutionLease, MAX_OUTPUT_BYTES, acquire_execution_lease, decode_captured_text,
};
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

/// Default command timeout (seconds).
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Maximum `CreateProcessW` command-line length, including its terminator.
#[cfg(windows)]
const WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS: usize = 32_767;

/// PowerShell 7 arguments placed before the directly encoded user script.
#[cfg(windows)]
const POWERSHELL_ARGUMENTS: &[&str] = &[
    "-NoLogo",
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-EncodedCommand",
];

/// One executable line inserted after the script's statement-ordering
/// prologue (leading comments, `using` statements, and `param` block): pins
/// the hidden console's pipe encoding to UTF-8 so non-ASCII text survives on
/// hosts whose console code page cannot encode it (CI runners, other
/// locales). `try`/`catch` keeps locked-down hosts that forbid the .NET
/// property assignment running the user script unchanged.
#[cfg(windows)]
const POWERSHELL_UTF8_PRELUDE: &str = "try { $utf8 = New-Object System.Text.UTF8Encoding $false; [Console]::InputEncoding = $utf8; [Console]::OutputEncoding = $utf8; $OutputEncoding = $utf8 } catch { }";

#[cfg(windows)]
const WINDOWS_SHELL_EXECUTABLE: &str = "pwsh.exe";

#[cfg(not(windows))]
#[derive(Debug, Clone, Copy)]
struct ShellCandidate {
    executable: &'static str,
}

#[cfg(not(windows))]
const SHELL_CANDIDATES: &[ShellCandidate] = &[
    ShellCandidate {
        executable: "/bin/bash",
    },
    ShellCandidate { executable: "bash" },
    ShellCandidate { executable: "sh" },
];

/// Whether another shell candidate may be attempted.
#[cfg(not(windows))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShellCandidateAction {
    /// PATH or path lookup missed the executable.
    TryNext,
    /// Image, identity, digest, permission, or any other failure.
    FailClosed,
}

/// Classifies a prepare failure for shell candidate fallback.
#[cfg(not(windows))]
#[must_use]
pub(crate) fn shell_candidate_action(error: &ResolveError) -> ShellCandidateAction {
    if error.is_not_found() {
        ShellCandidateAction::TryNext
    } else {
        ShellCandidateAction::FailClosed
    }
}

/// Returns the identifier used before shell selection finishes.
pub(crate) fn preferred_identifier() -> &'static str {
    #[cfg(windows)]
    return WINDOWS_SHELL_EXECUTABLE;
    #[cfg(not(windows))]
    SHELL_CANDIDATES[0].executable
}

/// The `shell` builtin.
#[derive(Debug)]
pub struct ShellTool {
    default_timeout: Duration,
}

impl ShellTool {
    /// A shell tool with the default 120 s timeout.
    #[must_use]
    pub fn new() -> Self {
        Self {
            default_timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// A shell tool with a custom default timeout; per-call `timeout_secs`
    /// arguments still take precedence.
    #[must_use]
    pub fn with_default_timeout(secs: u64) -> Self {
        Self {
            default_timeout: Duration::from_secs(secs),
        }
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Which launch path `shell` uses. The tool name stays `shell` either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum ShellMode {
    /// Run `command` in the platform shell.
    Script,
    /// Spawn `program` with `args` and no shell.
    Program,
}

/// Arguments for [`ShellTool`].
///
/// `mode` chooses the path. `script` requires `command` and rejects `program`
/// and `args`. `program` requires `program`, accepts `args`, and rejects
/// `command`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShellArgs {
    /// `script` runs `command` in the platform shell. `program` spawns one
    /// kernel-loadable image with `program` and `args` and does not start a shell.
    pub mode: ShellMode,
    /// Shell script. Required when `mode` is `script`. Omit for `program`.
    #[serde(default)]
    pub command: Option<String>,
    /// Executable basename or path. Required when `mode` is `program`. A bare
    /// name is searched only in absolute host PATH entries. A path is resolved
    /// against the session cwd. Omit for `script`.
    #[serde(default)]
    pub program: Option<String>,
    /// Argument vector for `program` mode, passed verbatim with no shell parsing.
    #[serde(default)]
    pub args: Vec<String>,
    /// Timeout in seconds for this command (default: 120).
    pub timeout_secs: Option<u64>,
}

fn validate_shell_args(args: &ShellArgs) -> Result<(), ToolError> {
    match args.mode {
        ShellMode::Script => {
            if args.program.is_some() || !args.args.is_empty() {
                return Err(ToolError::InvalidArgs(
                    "script mode accepts command only; program and args belong to program mode"
                        .into(),
                ));
            }
            if args.command.is_none() {
                return Err(ToolError::InvalidArgs(
                    "script mode requires command".into(),
                ));
            }
        }
        ShellMode::Program => {
            if args.command.is_some() {
                return Err(ToolError::InvalidArgs(
                    "program mode accepts program and args only; command belongs to script mode"
                        .into(),
                ));
            }
            if args
                .program
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            {
                return Err(ToolError::InvalidArgs(
                    "program mode requires program".into(),
                ));
            }
        }
    }
    Ok(())
}

struct PreparedShell {
    identifier: String,
    invocation: PreparedInvocation,
    lease: ExecutionLease,
}

#[async_trait]
impl Tool for ShellTool {
    type Args = ShellArgs;
    type Output = ();

    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Run a process in the session cwd. `mode` `script` executes `command` \
         in the platform shell for pipelines, redirection, expansion, and for \
         editing workspace files. On a POSIX shell, run Python (`python3` or \
         `python`) with a quoted heredoc or a short script. On PowerShell, \
         pipe a here-string to `python`. `mode` `program` spawns `program` \
         with an explicit `args` vector and does not start a shell; only a \
         kernel-loadable PE, ELF, or Mach-O image is accepted. `write` and \
         `edit` remain available for one UTF-8 file. `read`, `grep`, and \
         `find` stay in-process. Do not use this tool to talk to the user. \
         Edits made here are not undone. The platform shell is pwsh or Git \
         bash on Windows; POSIX hosts use a POSIX shell. Execution is \
         unsandboxed current-user execution with normal file and network \
         access; environment filtering is not a sandbox. Same-account \
         processes outside this host are outside the security boundary. \
         Captured stdout/stderr is truncated beyond 50 KiB; a non-zero exit \
         is an error result, not a tool failure. Default timeout: 120 s. \
         There is no Core permission prompt."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "shell: mode script runs a platform shell script, including a \
             Python heredoc or short script that edits files (command); mode \
             program runs one kernel-loadable binary with explicit args and no \
             shell. Optional timeout_secs.",
        )
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let timeout = Duration::from_secs(
            args.timeout_secs
                .unwrap_or(self.default_timeout.as_secs())
                .max(1),
        );
        validate_shell_args(&args)?;
        if args.mode == ShellMode::Program {
            let program = args.program.unwrap_or_default();
            return crate::builtin::exec::launch_program(program, args.args, timeout, ctx).await;
        }
        let command = args.command.unwrap_or_default();
        let started = Instant::now();
        if ctx.cancel.is_cancelled() {
            return Err(command_cancelled_error(None));
        }
        let identifier = preferred_identifier();
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let lease = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(command_cancelled_error(None)),
            _ = &mut deadline => {
                return Ok(timed_out_before_spawn_result(
                    &command,
                    identifier,
                    started.elapsed().as_millis() as u64,
                    timeout,
                ));
            }
            lease = acquire_execution_lease() => lease,
        };

        let prepared =
            match prepare_shell(&command, &ctx.cwd, lease, &ctx.cancel, &mut deadline).await? {
                Some(prepared) => prepared,
                None => {
                    return Ok(timed_out_before_spawn_result(
                        &command,
                        identifier,
                        started.elapsed().as_millis() as u64,
                        timeout,
                    ));
                }
            };
        if ctx.cancel.is_cancelled() {
            return Err(command_cancelled_error(None));
        }

        let identity = prepared_identity(&prepared.invocation);
        let PreparedShell {
            identifier: shell_identifier,
            invocation,
            lease,
        } = prepared;
        let outcome = run_prepared(invocation, lease, &ctx.cancel, &mut deadline).await?;
        let duration_ms = started.elapsed().as_millis() as u64;
        match outcome {
            RunOutcome::Done {
                status,
                stdout,
                stderr,
                metadata,
            } => Ok(with_identity(
                format_result(
                    Some(status),
                    &command,
                    &shell_identifier,
                    stdout,
                    stderr,
                    duration_ms,
                    false,
                    None,
                ),
                &identity,
                metadata,
            )),
            RunOutcome::CollectFailed { error, teardown } => {
                Err(collection_error(&error, teardown.err()))
            }
            RunOutcome::Timeout {
                stdout,
                stderr,
                teardown,
                started: launched,
                metadata,
            } => Ok(with_identity(
                timed_out_result(
                    &command,
                    &shell_identifier,
                    stdout,
                    stderr,
                    duration_ms,
                    timeout,
                    teardown,
                    launched,
                ),
                &identity,
                metadata,
            )),
            RunOutcome::Cancelled { teardown } => Err(command_cancelled_error(teardown.err())),
        }
    }
}

async fn prepare_shell(
    command: &str,
    cwd: &Path,
    lease: ExecutionLease,
    cancel: &tokio_util::sync::CancellationToken,
    deadline: &mut std::pin::Pin<&mut tokio::time::Sleep>,
) -> Result<Option<PreparedShell>, ToolError> {
    #[cfg(windows)]
    {
        prepare_windows_shell(command, cwd, lease, cancel, deadline).await
    }
    #[cfg(not(windows))]
    {
        prepare_posix_shell(command, cwd, lease, cancel, deadline).await
    }
}

#[cfg(windows)]
fn no_usable_shell_error() -> ToolError {
    ToolError::Execution("No usable shell was found. Set tools.shell in Settings → General.".into())
}

#[cfg(windows)]
fn shell_file_name(program: &Path) -> String {
    program
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| ShellKind::from_program(program).as_str().to_owned())
}

#[cfg(windows)]
async fn prepare_windows_shell(
    command: &str,
    cwd: &Path,
    lease: ExecutionLease,
    cancel: &tokio_util::sync::CancellationToken,
    deadline: &mut std::pin::Pin<&mut tokio::time::Sleep>,
) -> Result<Option<PreparedShell>, ToolError> {
    let command = command.to_owned();
    let pin_cwd = cwd.to_path_buf();
    let pin_work = run_blocking_supervised("shell resolution", cancel, move |worker_cancel| {
        let detected = detect::select_windows_shell(
            runtime_shell().or_else(detect_default_shell),
            windows_system_cmd(),
        )
        .ok_or_else(no_usable_shell_error)?;
        let identifier = shell_file_name(&detected.program);
        let args = windows_shell_args(&detected, &command)?;
        let program = detected.program.to_str().ok_or_else(|| {
            ToolError::InvalidArgs(
                "shell program path is not valid Unicode and cannot be recorded".into(),
            )
        })?;
        let env = snapshot_child_environment()?;
        let invocation = prepare_from_snapshot(&pin_cwd, program, &args, &env, &worker_cancel)
            .map_err(ResolveError::into_tool_error)?;
        Ok(PreparedShell {
            identifier,
            invocation,
            lease,
        })
    });
    tokio::pin!(pin_work);
    let prepared = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(command_cancelled_error(None)),
        _ = deadline.as_mut() => return Ok(None),
        prepared = &mut pin_work => prepared?,
    };
    Ok(Some(prepared))
}

#[cfg(windows)]
fn windows_shell_args(detected: &DetectedShell, command: &str) -> Result<Vec<String>, ToolError> {
    match detected.kind {
        ShellKind::Pwsh => {
            let script = powershell_script_for(command);
            let encoded = encode_powershell_command(&script, &detected.program)?;
            Ok(powershell_args(encoded))
        }
        ShellKind::Bash => Ok(vec!["-c".to_owned(), command.to_owned()]),
        ShellKind::Cmd => Ok(detect::cmd_fallback_args(command)),
    }
}

#[cfg(windows)]
fn windows_system_cmd() -> Option<std::path::PathBuf> {
    let root = std::env::var_os("SystemRoot")?;
    let path = std::path::PathBuf::from(root)
        .join("System32")
        .join("cmd.exe");
    path.is_file().then_some(path)
}

#[cfg(not(windows))]
async fn prepare_posix_shell(
    command: &str,
    cwd: &Path,
    lease: ExecutionLease,
    cancel: &tokio_util::sync::CancellationToken,
    deadline: &mut std::pin::Pin<&mut tokio::time::Sleep>,
) -> Result<Option<PreparedShell>, ToolError> {
    let args = vec!["-c".to_owned(), command.to_owned()];
    let pin_cwd = cwd.to_path_buf();
    let pin_work = run_blocking_supervised("shell resolution", cancel, move |worker_cancel| {
        let env = snapshot_child_environment()?;
        if let Some(detected) = runtime_shell() {
            let program = detected.program.to_str().ok_or_else(|| {
                ToolError::InvalidArgs(
                    "shell program path is not valid Unicode and cannot be recorded".into(),
                )
            })?;
            let invocation = prepare_from_snapshot(&pin_cwd, program, &args, &env, &worker_cancel)
                .map_err(ResolveError::into_tool_error)?;
            return Ok(PreparedShell {
                identifier: program.to_owned(),
                invocation,
                lease,
            });
        }
        let mut last_not_found = None;
        for candidate in SHELL_CANDIDATES {
            match prepare_from_snapshot(&pin_cwd, candidate.executable, &args, &env, &worker_cancel)
            {
                Ok(invocation) => {
                    return Ok(PreparedShell {
                        identifier: candidate.executable.to_owned(),
                        invocation,
                        lease,
                    });
                }
                Err(error) => match shell_candidate_action(&error) {
                    ShellCandidateAction::TryNext => last_not_found = Some(error),
                    ShellCandidateAction::FailClosed => return Err(error.into_tool_error()),
                },
            }
        }
        Err(last_not_found
            .unwrap_or_else(|| ResolveError::NotFound {
                program: "shell".into(),
                searched: Some(0),
            })
            .into_tool_error())
    });
    tokio::pin!(pin_work);
    let prepared = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(command_cancelled_error(None)),
        _ = deadline.as_mut() => return Ok(None),
        prepared = &mut pin_work => prepared?,
    };
    Ok(Some(prepared))
}

#[cfg(windows)]
fn powershell_script_for(command: &str) -> String {
    let mut script = String::with_capacity(command.len() + POWERSHELL_UTF8_PRELUDE.len() + 4);
    if command.is_empty() {
        // PowerShell 7 rejects an empty -EncodedCommand payload as not Base64.
        script.push_str("#\n");
        script.push_str(POWERSHELL_UTF8_PRELUDE);
        return script;
    }
    let prologue = powershell_prologue_units(command);
    script.push_str(&command[..prologue]);
    if !script.is_empty() && !script.ends_with('\n') {
        script.push('\n');
    }
    script.push_str(POWERSHELL_UTF8_PRELUDE);
    script.push('\n');
    script.push_str(&command[prologue..]);
    script
}

/// Byte offset of the end of the leading statement-ordering prologue — blank
/// lines, comments, `using` statements, and one `param (...)` block — which
/// PowerShell requires (`using`) or expects (`param`) before all other
/// statements. The UTF-8 prelude is inserted right after it, so those forms
/// keep their required position.
#[cfg(windows)]
fn powershell_prologue_units(command: &str) -> usize {
    let mut offset = 0_usize;
    let mut paren_depth: i64 = 0;
    let mut in_param_block = false;
    let mut quote: Option<char> = None;
    for line in command.split_inclusive('\n') {
        if in_param_block {
            scan_param_line(line, &mut paren_depth, &mut quote);
            offset += line.len();
            if paren_depth <= 0 {
                in_param_block = false;
            }
            continue;
        }
        let trimmed = line.trim_start();
        let is_prologue_line = trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("using ")
            || trimmed.starts_with("using\t");
        if is_prologue_line {
            offset += line.len();
            continue;
        }
        if trimmed.starts_with("param(") || trimmed.starts_with("param (") {
            in_param_block = true;
            paren_depth = 0;
            quote = None;
            scan_param_line(line, &mut paren_depth, &mut quote);
            offset += line.len();
            if paren_depth <= 0 {
                in_param_block = false;
            }
            continue;
        }
        break;
    }
    offset
}

/// Tracks paren depth across one line of a `param (...)` block, ignoring
/// parentheses inside single- or double-quoted strings.
#[cfg(windows)]
fn scan_param_line(line: &str, paren_depth: &mut i64, quote: &mut Option<char>) {
    for character in line.chars() {
        match *quote {
            Some(open) if character == open => *quote = None,
            Some(_) => {}
            None => match character {
                '\'' | '"' => *quote = Some(character),
                '(' => *paren_depth += 1,
                ')' => *paren_depth -= 1,
                _ => {}
            },
        }
    }
}

#[cfg(windows)]
fn powershell_args(encoded_command: String) -> Vec<String> {
    let mut args = Vec::with_capacity(POWERSHELL_ARGUMENTS.len() + 1);
    args.extend(
        POWERSHELL_ARGUMENTS
            .iter()
            .map(|argument| (*argument).to_owned()),
    );
    args.push(encoded_command);
    args
}

fn command_cancelled_error(teardown: Option<std::io::Error>) -> ToolError {
    match teardown {
        Some(err) => ToolError::Execution(format!(
            "command cancelled before completion; termination failed: {err}"
        )),
        None => ToolError::Execution("command cancelled before completion".into()),
    }
}

fn collection_error(collection: &std::io::Error, teardown: Option<std::io::Error>) -> ToolError {
    match teardown {
        Some(err) => ToolError::Execution(format!(
            "failed to collect command output: {collection}; termination failed: {err}"
        )),
        None => ToolError::Execution(format!("failed to collect command output: {collection}")),
    }
}

fn timed_out_before_spawn_result(
    command: &str,
    shell_identifier: &str,
    duration_ms: u64,
    timeout: Duration,
) -> ToolResult {
    let notice = format!(
        "[command timed out after {}s before the shell started]",
        timeout.as_secs()
    );
    mark_timed_out(format_result(
        None,
        command,
        shell_identifier,
        CapturedStream::default(),
        CapturedStream::default(),
        duration_ms,
        true,
        Some(&notice),
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "timeout result assembly mirrors the tool's stable output fields"
)]
fn timed_out_result(
    command: &str,
    shell_identifier: &str,
    stdout: CapturedStream,
    stderr: CapturedStream,
    duration_ms: u64,
    timeout: Duration,
    teardown: Result<(), std::io::Error>,
    started: bool,
) -> ToolResult {
    let notice = match (started, teardown.as_ref().err()) {
        (_, Some(err)) => format!(
            "[command timed out after {}s; termination failed: {err}]",
            timeout.as_secs()
        ),
        (true, None) => format!(
            "[command timed out after {}s and was killed]",
            timeout.as_secs()
        ),
        (false, None) => format!(
            "[command timed out after {}s before the shell started]",
            timeout.as_secs()
        ),
    };
    mark_timed_out(format_result(
        None,
        command,
        shell_identifier,
        stdout,
        stderr,
        duration_ms,
        true,
        Some(&notice),
    ))
}

fn mark_timed_out(mut result: ToolResult) -> ToolResult {
    result.details.as_mut().expect("details were populated")["timed_out"] = json!(true);
    result
}

fn with_identity(
    mut result: ToolResult,
    identity: &PreparedIdentity,
    metadata: ExecutionMetadata,
) -> ToolResult {
    let details = result.details.as_mut().expect("details were populated");
    apply_execution_details(details, identity, metadata);
    result
}

const CLIXML_MARKER: &str = "#< CLIXML";

/// Turns redirected PowerShell CLIXML blobs into readable text.
#[must_use]
pub(crate) fn sanitize_captured_shell_text(text: &str) -> String {
    if !text.contains(CLIXML_MARKER) {
        return text.to_owned();
    }
    let errors = extract_clixml_s_nodes(text, "Error");
    if errors.is_empty() {
        return strip_clixml_header(text);
    }
    let mut lines = errors;
    lines.extend(extract_clixml_s_nodes(text, "Warning"));
    lines.join("\n")
}

fn extract_clixml_s_nodes(text: &str, kind: &str) -> Vec<String> {
    let open = format!(r#"<S S="{kind}">"#);
    let mut nodes = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find("</S>") else {
            break;
        };
        nodes.push(decode_clixml_text(&after[..end]));
        rest = &after[end + 4..];
    }
    nodes
}

fn decode_clixml_text(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("_x000D_", "\r")
        .replace("_x000A_", "\n")
        .replace("_x0009_", "\t")
        .trim_end()
        .to_owned()
}

fn strip_clixml_header(text: &str) -> String {
    let Some(start) = text.find(CLIXML_MARKER) else {
        return text.to_owned();
    };
    let prefix = text[..start].trim_end_matches(['\r', '\n']);
    let rest = &text[start + CLIXML_MARKER.len()..];
    let suffix = rest
        .rfind("</Objs>")
        .map(|end| rest[end + "</Objs>".len()..].trim_start_matches(['\r', '\n']))
        .unwrap_or("");
    match (prefix.is_empty(), suffix.is_empty()) {
        (true, true) => String::new(),
        (false, true) => prefix.to_owned(),
        (true, false) => suffix.to_owned(),
        (false, false) => format!("{prefix}\n{suffix}"),
    }
}

/// Assemble the tool result from collected output.
///
/// `status == None` marks a command that did not finish (timeout path);
/// such results are always `is_error`.
#[expect(
    clippy::too_many_arguments,
    reason = "result assembly mirrors the tool's stable output fields"
)]
fn format_result(
    status: Option<std::process::ExitStatus>,
    command: &str,
    shell: &str,
    stdout: CapturedStream,
    stderr: CapturedStream,
    duration_ms: u64,
    forced_error: bool,
    notice: Option<&str>,
) -> ToolResult {
    let stdout_text = sanitize_captured_shell_text(&decode_captured_text(&stdout.retained));
    let stderr_text = sanitize_captured_shell_text(&decode_captured_text(&stderr.retained));

    let mut text = String::new();
    if !stdout_text.trim().is_empty() {
        text.push_str(&stdout_text);
    }
    if !stderr_text.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str("[stderr]\n");
        text.push_str(&stderr_text);
    }

    let (text, truncated) = crate::builtin::truncate_bytes(&text, MAX_OUTPUT_BYTES);
    let mut text = text;
    if truncated {
        let total = stdout.total_bytes.saturating_add(stderr.total_bytes);
        text.push_str(&format!(
            "\n[output truncated: showed first {MAX_OUTPUT_BYTES} of {total} bytes]"
        ));
    }

    let is_error = forced_error || status.is_some_and(|s| !s.success());
    if is_error {
        match &status {
            Some(s) => text.push_str(&format!("\n[exit code: {}]", display_exit(s))),
            None => match notice {
                Some(notice) => {
                    text.push('\n');
                    text.push_str(notice);
                }
                None => text.push_str("\n[no exit status: command did not finish]"),
            },
        }
    } else if let Some(notice) = notice {
        text.push('\n');
        text.push_str(notice);
    }

    let mut details = json!({
        "command": command,
        "shell": shell,
        "stdout_bytes": stdout.total_bytes,
        "stderr_bytes": stderr.total_bytes,
        "duration_ms": duration_ms,
        "truncated": truncated,
    });
    if let Some(s) = status {
        details["exit_code"] = json!(display_exit(&s));
    }

    ToolResult {
        content: vec![mycode_core::message::ContentBlock::Text(text.into())],
        is_error,
        details: Some(details),
    }
}

fn display_exit(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

/// Encode the user script itself as PowerShell's UTF-16LE Base64 transport.
///
/// Besides the single-line UTF-8 console-encoding prelude that
/// [`powershell_script`] inserts after the statement-ordering prologue, no
/// launcher script or .NET decoding API is inserted, so a leading `using`
/// statement remains the first statement and ConstrainedLanguage can execute
/// its permitted cmdlets. The exact `CreateProcessW` budget includes the
/// quoted executable, fixed arguments, encoded payload, spaces, and final
/// UTF-16 NUL.
#[cfg(windows)]
pub(crate) fn encode_powershell_command(
    command: &str,
    executable: &Path,
) -> Result<String, ToolError> {
    encode_powershell_command_with(command, executable, POWERSHELL_ARGUMENTS)
}

#[cfg(windows)]
fn encode_powershell_command_with(
    command: &str,
    executable: &Path,
    arguments: &[&str],
) -> Result<String, ToolError> {
    let command_byte_len = command
        .encode_utf16()
        .count()
        .checked_mul(2)
        .ok_or_else(|| command_too_long_with(executable, None, arguments))?;
    let encoded_len = base64_encoded_len(command_byte_len)
        .ok_or_else(|| command_too_long_with(executable, None, arguments))?;
    let command_line_units = powershell_command_line_units_with(executable, encoded_len, arguments)
        .ok_or_else(|| command_too_long_with(executable, Some(encoded_len), arguments))?;
    if command_line_units > WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS {
        return Err(command_too_long_with(
            executable,
            Some(encoded_len),
            arguments,
        ));
    }

    Ok(BASE64_STANDARD.encode(utf16le_bytes(command, command_byte_len)))
}

#[cfg(windows)]
fn powershell_command_line_units_with(
    executable: &Path,
    encoded_len: usize,
    arguments: &[&str],
) -> Option<usize> {
    // `std::process::Command` quotes argv[0] on Windows even when it contains no
    // spaces. Structured exec quotes argv0 the same way. Every fixed argument
    // and Base64 character needs no extra quoting; an empty Base64 argument is
    // represented as `""`.
    let mut units = executable_utf16_units(executable).checked_add(2)?;
    for argument in arguments {
        units = units
            .checked_add(1)?
            .checked_add(argument.encode_utf16().count())?;
    }
    units = units
        .checked_add(1)?
        .checked_add(if encoded_len == 0 { 2 } else { encoded_len })?;
    units.checked_add(1)
}

#[cfg(windows)]
fn executable_utf16_units(executable: &Path) -> usize {
    use std::os::windows::ffi::OsStrExt as _;

    executable.as_os_str().encode_wide().count()
}

#[cfg(windows)]
fn maximum_encoded_command_chars_with(executable: &Path, arguments: &[&str]) -> Option<usize> {
    let one_character_line = powershell_command_line_units_with(executable, 1, arguments)?;
    WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS.checked_sub(one_character_line.checked_sub(1)?)
}

#[cfg(windows)]
fn base64_encoded_len(byte_len: usize) -> Option<usize> {
    byte_len.checked_add(2)?.checked_div(3)?.checked_mul(4)
}

#[cfg(windows)]
fn utf16le_bytes(value: &str, byte_len: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(byte_len);
    for unit in value.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

#[cfg(windows)]
fn command_too_long_with(
    executable: &Path,
    encoded_len: Option<usize>,
    arguments: &[&str],
) -> ToolError {
    let maximum = maximum_encoded_command_chars_with(executable, arguments)
        .map_or_else(|| "unrepresentable".to_owned(), |value| value.to_string());
    let encoded =
        encoded_len.map_or_else(|| "overflowed usize".to_owned(), |value| value.to_string());
    let executable_name = executable.file_name().map_or_else(
        || std::borrow::Cow::Borrowed("pwsh.exe"),
        |name| name.to_string_lossy(),
    );
    ToolError::InvalidArgs(format!(
        "command is too long for PowerShell 7's 32,767 UTF-16-code-unit CreateProcessW \
         command-line limit (including the terminator): encoded length is {encoded}, maximum \
         for executable {executable_name} is {maximum}"
    ))
}

#[cfg(test)]
mod mode_tests {
    use super::{ShellArgs, ShellMode, ShellTool, validate_shell_args};
    use crate::registry::ToolRegistry;
    use crate::tool::{Tool, ToolDyn, ToolError};
    use crate::{ToolCtx, ToolStream, register_builtins};

    #[test]
    fn schema_advertises_one_tool_and_two_modes() {
        let tool = ShellTool::new();
        let spec = ToolDyn::spec(&tool);
        assert_eq!(spec.name, "shell");
        let schema = spec.params_schema.to_string();
        assert!(schema.contains("\"script\""), "{schema}");
        assert!(schema.contains("\"program\""), "{schema}");
        assert!(!schema.contains("\"exec\""), "{schema}");
        let snippet = tool.prompt_snippet().expect("snippet");
        assert!(snippet.contains("script"));
        assert!(snippet.contains("program"));
        assert!(!snippet.contains("exec"));
        assert!(!spec.description.contains("`exec`"));
    }

    #[test]
    fn builtins_register_shell_not_exec() {
        let registry = ToolRegistry::new();
        register_builtins(&registry);
        let names = registry.names();
        assert!(names.iter().any(|name| name == "shell"));
        assert!(!names.iter().any(|name| name == "exec" || name == "task"));
    }

    #[test]
    fn modes_reject_the_other_paths_fields() {
        let script = ShellArgs {
            mode: ShellMode::Script,
            command: Some("echo hi".into()),
            program: Some("echo".into()),
            args: Vec::new(),
            timeout_secs: None,
        };
        assert!(matches!(
            validate_shell_args(&script),
            Err(ToolError::InvalidArgs(_))
        ));
        let program = ShellArgs {
            mode: ShellMode::Program,
            command: Some("echo hi".into()),
            program: Some("echo".into()),
            args: vec!["ok".into()],
            timeout_secs: None,
        };
        assert!(matches!(
            validate_shell_args(&program),
            Err(ToolError::InvalidArgs(_))
        ));
        let missing = ShellArgs {
            mode: ShellMode::Program,
            command: None,
            program: None,
            args: Vec::new(),
            timeout_secs: None,
        };
        assert!(matches!(
            validate_shell_args(&missing),
            Err(ToolError::InvalidArgs(_))
        ));
    }

    #[tokio::test]
    async fn invalid_mode_mix_does_not_spawn() {
        let tool = ShellTool::new();
        let ctx = ToolCtx::new(std::env::temp_dir());
        let mut out = ToolStream::closed();
        let error = tool
            .execute(
                ShellArgs {
                    mode: ShellMode::Script,
                    command: Some("echo hi".into()),
                    program: None,
                    args: vec!["nope".into()],
                    timeout_secs: Some(1),
                },
                &ctx,
                &mut out,
            )
            .await
            .expect_err("mixed script arguments");
        assert!(matches!(error, ToolError::InvalidArgs(_)));
    }
}
