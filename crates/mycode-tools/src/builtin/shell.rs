//! The process-launch tool. The model sees exactly one shell tool, chosen
//! once when the process starts and kept for the whole session.
//!
//! Two modes share that name, one schema, and one prompt entry:
//!
//! * `script` runs `command` in the resolved interpreter. Windows prefers
//!   PowerShell 7 (`pwsh`), then Windows PowerShell 5.1 (`powershell.exe`).
//!   `cmd.exe` is the runtime fallback when neither exists. Git Bash, MSYS2,
//!   Cygwin, and WSL are not selected. Linux and macOS use `$SHELL` when it
//!   is bash, zsh, or sh, then the platform default (`zsh` on macOS, `bash`
//!   elsewhere). Commands are not rewritten between shells.
//! * `program` spawns `program` with an explicit `args` vector and does not
//!   start a shell. Only a kernel-loadable PE, ELF, or Mach-O image is
//!   accepted. Shebang scripts and batch files are rejected.
//!
//! Both modes pin the launched image, snapshot cwd/env/PATH once per call,
//! allowlist the child environment, and use contained spawn. Execution is
//! unsandboxed current-user file and network authority; environment filtering
//! is not a sandbox. Valid calls run directly with no Core permission prompt.
//! `write` and `edit` stay available. `read`, `grep`, and `find` stay
//! in-process. File edits from either path are not undone.
use std::path::Path;
use std::time::{Duration, Instant};

#[path = "shell_detect.rs"]
mod detect;
#[path = "shell_prompt.rs"]
mod prompt;

pub use detect::{DetectedShell, ShellKind, active_shell, detect_default_shell, resolved_shell};
pub use prompt::render_environment_block;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use crate::builtin::blocking::run_blocking_supervised;
use crate::builtin::exec::{
    ExecutionMetadata, PreparedIdentity, PreparedInvocation, ResolveError, RunOutcome,
    apply_execution_details, prepare_from_snapshot, prepared_identity, run_prepared,
    snapshot_child_environment,
};
use crate::builtin::process::{
    CapturedStream, ExecutionLease, MAX_OUTPUT_BYTES, acquire_execution_lease, collection_error,
    command_cancelled_error, decode_captured_text, display_exit, mark_timed_out,
};
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Maximum `CreateProcessW` command-line length, including its terminator.
/// Applied on every host so a PowerShell script cannot exceed the Windows
/// limit when the same command is later run there.
const WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS: usize = 32_767;

/// PowerShell arguments placed before the directly encoded user script.
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
const POWERSHELL_UTF8_PRELUDE: &str = "try { $utf8 = New-Object System.Text.UTF8Encoding $false; [Console]::InputEncoding = $utf8; [Console]::OutputEncoding = $utf8; $OutputEncoding = $utf8 } catch { }";

/// One-line name of the shell `script` mode would launch right now.
///
/// Includes the Windows `cmd.exe` runtime fallback. `None` is not returned
/// after [`resolved_shell`]; the line is kept for callers that only need the
/// interpreter label.
#[must_use]
pub fn script_shell_line() -> Option<String> {
    let shell = resolved_shell();
    Some(format!(
        "{} ({})",
        prompt::shell_label(shell.kind),
        shell.program.display()
    ))
}

/// The process-launch builtin. Its model-facing name follows the resolved shell.
#[derive(Debug)]
pub struct ShellTool {
    default_timeout: Duration,
    /// When set, presentation and launch use this shell instead of process
    /// detection. Production leaves it unset.
    forced: Option<DetectedShell>,
}

impl ShellTool {
    /// A shell tool with the default 120 s timeout.
    #[must_use]
    pub fn new() -> Self {
        Self {
            default_timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            forced: None,
        }
    }

    /// A shell tool with a custom default timeout; per-call `timeout_secs`
    /// arguments still take precedence.
    #[must_use]
    pub fn with_default_timeout(secs: u64) -> Self {
        Self {
            default_timeout: Duration::from_secs(secs),
            forced: None,
        }
    }

    /// A shell tool pinned to one interpreter. Used by tests and by callers
    /// that already resolved the shell.
    #[must_use]
    pub fn forcing(shell: DetectedShell) -> Self {
        Self {
            default_timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            forced: Some(shell),
        }
    }

    fn resolved(&self) -> DetectedShell {
        self.forced.clone().unwrap_or_else(resolved_shell)
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Which launch path the shell tool uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum ShellMode {
    /// Run `command` in the platform shell. This is the default when `mode` is omitted.
    #[default]
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
    /// Omitted `mode` means `script`.
    #[serde(default)]
    #[schemars(default)]
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
        self.resolved().kind.tool_name()
    }

    fn description(&self) -> &str {
        prompt::tool_description(self.resolved().kind)
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(prompt::prompt_snippet(self.resolved().kind))
    }

    fn params_schema(&self) -> serde_json::Value {
        let mut schema = crate::tool::args_schema::<ShellArgs>();
        prompt::apply_parameter_docs(&mut schema, self.resolved().kind);
        schema
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
        let forced = self.forced.clone();
        let identifier = forced
            .as_ref()
            .map(|shell| shell_file_name(&shell.program))
            .unwrap_or_else(|| "shell".to_owned());
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let lease = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(command_cancelled_error(None)),
            _ = &mut deadline => {
                return Ok(timed_out_before_spawn_result(
                    &command,
                    &identifier,
                    started.elapsed().as_millis() as u64,
                    timeout,
                ));
            }
            lease = acquire_execution_lease() => lease,
        };

        let prepared = match prepare_shell(
            forced,
            &command,
            &ctx.cwd,
            lease,
            &ctx.cancel,
            &mut deadline,
        )
        .await?
        {
            Some(prepared) => prepared,
            None => {
                return Ok(timed_out_before_spawn_result(
                    &command,
                    &identifier,
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
    forced: Option<DetectedShell>,
    command: &str,
    cwd: &Path,
    lease: ExecutionLease,
    cancel: &tokio_util::sync::CancellationToken,
    deadline: &mut std::pin::Pin<&mut tokio::time::Sleep>,
) -> Result<Option<PreparedShell>, ToolError> {
    let command = command.to_owned();
    let pin_cwd = cwd.to_path_buf();
    let pin_work = run_blocking_supervised("shell resolution", cancel, move |worker_cancel| {
        let shell = forced.unwrap_or_else(resolved_shell);
        let identifier = shell_file_name(&shell.program);
        let args = script_launch_args(&shell, &command)?;
        let program = shell.program.to_str().ok_or_else(|| {
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

fn shell_file_name(program: &Path) -> String {
    program
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| ShellKind::from_program(program).as_str().to_owned())
}

/// Launch arguments for one script. PowerShell receives the user's text
/// unchanged, inside the UTF-8 prelude, as `-EncodedCommand`.
fn script_launch_args(shell: &DetectedShell, command: &str) -> Result<Vec<String>, ToolError> {
    match shell.kind {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => {
            let script = powershell_script_for(command);
            let encoded = encode_powershell_command(&script, &shell.program)?;
            Ok(powershell_args(encoded))
        }
        ShellKind::Bash | ShellKind::Zsh | ShellKind::Sh => {
            Ok(vec!["-c".to_owned(), command.to_owned()])
        }
        ShellKind::Cmd => Ok(detect::cmd_fallback_args(command)),
    }
}

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
        // PowerShell keywords are case-insensitive: `Param($x)` and
        // `Using namespace …` are legal spellings the prelude must sit
        // behind, or the inserted statement makes them fail to parse.
        let lower = trimmed.to_ascii_lowercase();
        let is_prologue_line = trimmed.is_empty()
            || lower.starts_with('#')
            || lower.starts_with("using ")
            || lower.starts_with("using\t");
        if is_prologue_line {
            offset += line.len();
            continue;
        }
        if lower.starts_with("param(") || lower.starts_with("param (") {
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

fn timed_out_before_spawn_result(
    command: &str,
    shell_identifier: &str,
    duration_ms: u64,
    timeout: Duration,
) -> ToolResult {
    timed_out_result(
        command,
        shell_identifier,
        CapturedStream::default(),
        CapturedStream::default(),
        duration_ms,
        timeout,
        Ok(()),
        false,
    )
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

/// Turns captured shell text into readable plain text.
///
/// PowerShell's redirected error stream is CLIXML. Control characters there
/// are `_xHHHH_` escapes (`_x001B_` is ESC), and the message may still carry
/// ANSI SGR codes. Escapes are decoded for every captured string, including
/// fragments with no `#< CLIXML` marker. When the marker is present,
/// `<S S="Error">` and `<S S="Warning">` nodes are extracted as before, then
/// ANSI/VT sequences are stripped from that text.
#[must_use]
pub(crate) fn sanitize_captured_shell_text(text: &str) -> String {
    if !text.contains(CLIXML_MARKER) && !text.contains("_x") && !has_ansi_introducer(text) {
        return text.to_owned();
    }
    let readable = if text.contains(CLIXML_MARKER) {
        let errors = extract_clixml_s_nodes(text, "Error");
        if errors.is_empty() {
            decode_clixml_char_escapes(&strip_clixml_header(text))
        } else {
            let mut lines = errors;
            lines.extend(extract_clixml_s_nodes(text, "Warning"));
            lines.join("\n")
        }
    } else {
        decode_clixml_char_escapes(text)
    };
    strip_ansi_sequences(&readable)
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
    let unescaped = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    decode_clixml_char_escapes(&unescaped).trim_end().to_owned()
}

/// Decodes every CLIXML `_xHHHH_` escape (4 hex digits) to a Unicode scalar.
///
/// Surrogate code points are dropped. The scan is one pass, so `_x005F_`
/// (underscore) does not re-open the following text as another escape.
fn decode_clixml_char_escapes(text: &str) -> String {
    if !text.contains("_x") {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if is_clixml_char_escape(bytes, index) {
            let code =
                u32::from_str_radix(&text[index + 2..index + 6], 16).expect("four hex digits");
            if let Some(ch) = char::from_u32(code) {
                out.push(ch);
            }
            index += 7;
            continue;
        }
        let ch = text[index..]
            .chars()
            .next()
            .expect("index on a char boundary");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

fn is_clixml_char_escape(bytes: &[u8], index: usize) -> bool {
    index + 7 <= bytes.len()
        && bytes[index] == b'_'
        && bytes[index + 1] == b'x'
        && bytes[index + 6] == b'_'
        && bytes[index + 2..index + 6]
            .iter()
            .all(u8::is_ascii_hexdigit)
}

fn has_ansi_introducer(text: &str) -> bool {
    text.chars().any(is_ansi_introducer)
}

fn is_ansi_introducer(ch: char) -> bool {
    matches!(
        ch,
        '\u{1b}' | '\u{90}' | '\u{98}' | '\u{9b}' | '\u{9d}' | '\u{9e}' | '\u{9f}'
    )
}

/// Removes ANSI/VT sequences: CSI, OSC, string Fe sequences, and other
/// single-character ESC Fe/Fp/Fs sequences. `text` is already decoded, so
/// both a real ESC and a former `_x001B_` are the U+001B character here.
fn strip_ansi_sequences(text: &str) -> String {
    if !has_ansi_introducer(text) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(is_ansi_introducer) {
        out.push_str(&rest[..start]);
        let skip = ansi_sequence_len(&rest[start..]);
        if skip == 0 {
            let ch = rest[start..].chars().next().expect("introducer");
            out.push(ch);
            rest = &rest[start + ch.len_utf8()..];
            continue;
        }
        rest = &rest[start + skip..];
    }
    out.push_str(rest);
    out
}

fn ansi_sequence_len(text: &str) -> usize {
    let mut chars = text.char_indices();
    let Some((_, first)) = chars.next() else {
        return 0;
    };
    match first {
        '\u{1b}' => esc_sequence_len(text, chars),
        '\u{9b}' => csi_payload_len(text, chars),
        '\u{90}' | '\u{98}' | '\u{9d}' | '\u{9e}' | '\u{9f}' => string_payload_len(text, chars),
        _ => 0,
    }
}

fn esc_sequence_len(text: &str, mut chars: std::str::CharIndices<'_>) -> usize {
    let Some((index, next)) = chars.next() else {
        return 1;
    };
    match next {
        '[' => csi_payload_len(text, chars),
        ']' | 'P' | 'X' | '^' | '_' => string_payload_len(text, chars),
        ch if ('\u{20}'..='\u{2f}').contains(&ch) => escape_intermediate_len(index, chars),
        ch if ('\u{30}'..='\u{7e}').contains(&ch) => index + ch.len_utf8(),
        _ => 1,
    }
}

/// CSI payload, with the introducer (`ESC [` or U+009B) already consumed.
fn csi_payload_len(text: &str, chars: std::str::CharIndices<'_>) -> usize {
    let mut end = bytes_already_consumed(text, &chars);
    for (index, ch) in chars {
        if ('\u{40}'..='\u{7e}').contains(&ch) {
            return index + ch.len_utf8();
        }
        if !('\u{20}'..='\u{3f}').contains(&ch) {
            return index;
        }
        end = index + ch.len_utf8();
    }
    end
}

/// OSC/DCS/SOS/PM/APC payload. Ends at BEL, 8-bit ST, or `ESC \`.
fn string_payload_len(text: &str, mut chars: std::str::CharIndices<'_>) -> usize {
    let mut end = bytes_already_consumed(text, &chars);
    while let Some((index, ch)) = chars.next() {
        match ch {
            '\u{7}' | '\u{9c}' => return index + ch.len_utf8(),
            '\u{1b}' => {
                if chars.as_str().starts_with('\\') {
                    return index + 2;
                }
                return index;
            }
            _ => end = index + ch.len_utf8(),
        }
    }
    end
}

/// ESC intermediate bytes (0x20–0x2F) plus the final byte (0x30–0x7E).
fn escape_intermediate_len(first_index: usize, chars: std::str::CharIndices<'_>) -> usize {
    let mut end = first_index + 1;
    for (index, ch) in chars {
        if ('\u{20}'..='\u{2f}').contains(&ch) {
            end = index + ch.len_utf8();
            continue;
        }
        if ('\u{30}'..='\u{7e}').contains(&ch) {
            return index + ch.len_utf8();
        }
        return index;
    }
    end
}

fn bytes_already_consumed(text: &str, chars: &std::str::CharIndices<'_>) -> usize {
    text.len() - chars.as_str().len()
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

/// Encode the user script itself as PowerShell's UTF-16LE Base64 transport.
///
/// Besides the single-line UTF-8 console-encoding prelude that
/// [`powershell_script_for`] inserts after the statement-ordering prologue, no
/// launcher script or .NET decoding API is inserted, so a leading `using`
/// statement remains the first statement and ConstrainedLanguage can execute
/// its permitted cmdlets. The exact `CreateProcessW` budget includes the
/// quoted executable, fixed arguments, encoded payload, spaces, and final
/// UTF-16 NUL.
pub(crate) fn encode_powershell_command(
    command: &str,
    executable: &Path,
) -> Result<String, ToolError> {
    encode_powershell_command_with(command, executable, POWERSHELL_ARGUMENTS)
}

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

fn executable_utf16_units(executable: &Path) -> usize {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        executable.as_os_str().encode_wide().count()
    }
    #[cfg(not(windows))]
    {
        executable.to_string_lossy().encode_utf16().count()
    }
}

fn maximum_encoded_command_chars_with(executable: &Path, arguments: &[&str]) -> Option<usize> {
    let one_character_line = powershell_command_line_units_with(executable, 1, arguments)?;
    WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS.checked_sub(one_character_line.checked_sub(1)?)
}

fn base64_encoded_len(byte_len: usize) -> Option<usize> {
    byte_len.checked_add(2)?.checked_div(3)?.checked_mul(4)
}

fn utf16le_bytes(value: &str, byte_len: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(byte_len);
    for unit in value.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

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
        "command is too long for PowerShell's 32,767 UTF-16-code-unit command-line \
         limit (including the terminator): encoded length is {encoded}, maximum \
         for executable {executable_name} is {maximum}"
    ))
}

#[cfg(all(test, target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
#[path = "shell_lifecycle_test.rs"]
mod lifecycle_test;

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
        let expected = super::resolved_shell().kind.tool_name();
        assert_eq!(spec.name, expected);
        assert!(matches!(
            spec.name.as_str(),
            "powershell" | "bash" | "zsh" | "sh" | "cmd"
        ));
        let schema = spec.params_schema.to_string();
        assert!(schema.contains("\"script\""), "{schema}");
        assert!(schema.contains("\"program\""), "{schema}");
        assert!(!schema.contains("\"exec\""), "{schema}");
        let snippet = tool.prompt_snippet().expect("snippet");
        assert!(snippet.contains("script"));
        assert!(snippet.contains("program"));
        assert!(!snippet.contains("exec"));
        assert!(!spec.description.contains("`exec`"));
        assert!(!spec.description.contains("translated"));
        let required = spec
            .params_schema
            .get("required")
            .and_then(|value| value.as_array());
        let mode_required =
            required.is_some_and(|items| items.iter().any(|item| item.as_str() == Some("mode")));
        assert!(
            !mode_required,
            "mode defaults to script, schema was {schema}"
        );
    }

    #[test]
    fn omitted_mode_is_script() {
        let value = serde_json::json!({"command": "echo hi"});
        crate::tool::validate_args::<ShellArgs>(&value).expect("mode is optional");
        let args: ShellArgs = serde_json::from_value(value).expect("decode");
        assert_eq!(args.mode, ShellMode::Script);
        assert_eq!(args.command.as_deref(), Some("echo hi"));
    }

    #[test]
    fn powershell_and_bash_specs_are_mutually_exclusive() {
        let powershell = ShellTool::forcing(super::DetectedShell {
            kind: super::ShellKind::WindowsPowerShell,
            program: std::path::PathBuf::from("powershell.exe"),
        });
        let bash = ShellTool::forcing(super::DetectedShell {
            kind: super::ShellKind::Bash,
            program: std::path::PathBuf::from("/bin/bash"),
        });
        let pwsh = ShellTool::forcing(super::DetectedShell {
            kind: super::ShellKind::Pwsh,
            program: std::path::PathBuf::from("pwsh.exe"),
        });
        let ps_spec = ToolDyn::spec(&powershell);
        let bash_spec = ToolDyn::spec(&bash);
        let pwsh_spec = ToolDyn::spec(&pwsh);
        assert_eq!(ps_spec.name, "powershell");
        assert_eq!(pwsh_spec.name, "powershell");
        assert_eq!(bash_spec.name, "bash");
        assert!(ps_spec.description.contains("Windows PowerShell 5.1"));
        assert!(ps_spec.description.contains("&&"));
        assert!(ps_spec.params_schema.to_string().contains("syntax errors"));
        assert!(pwsh_spec.description.contains("PowerShell 7"));
        assert!(pwsh_spec.description.contains("Get-ChildItem"));
        assert!(!pwsh_spec.description.contains("translated"));
        assert!(bash_spec.description.contains("bash -c"));
        assert!(bash_spec.description.contains("quoted heredoc"));
        assert!(!bash_spec.description.contains("Get-ChildItem"));
        let registry = ToolRegistry::new();
        registry.register(std::sync::Arc::new(powershell));
        let names = registry.names();
        assert_eq!(names, vec!["powershell".to_owned()]);
        assert!(!names.iter().any(|name| name == "bash" || name == "shell"));
    }

    #[test]
    fn builtins_register_one_shell_tool_not_exec() {
        let registry = ToolRegistry::new();
        register_builtins(&registry);
        let names = registry.names();
        let shell_names: Vec<_> = names
            .iter()
            .filter(|name| {
                matches!(
                    name.as_str(),
                    "powershell" | "bash" | "zsh" | "sh" | "cmd" | "shell"
                )
            })
            .cloned()
            .collect();
        assert_eq!(
            shell_names,
            vec![super::resolved_shell().kind.tool_name().to_owned()]
        );
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

    fn result_text(result: &crate::tool::ToolResult) -> String {
        match result.content.first() {
            Some(mycode_core::message::ContentBlock::Text(block)) => block.text.clone(),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[test]
    fn powershell_launch_keeps_the_command_and_does_not_translate_bash() {
        let shell = super::DetectedShell {
            kind: super::ShellKind::Pwsh,
            program: std::path::PathBuf::from("pwsh.exe"),
        };
        let args = super::script_launch_args(&shell, "ls -la").expect("encode");
        assert!(args.iter().any(|arg| arg == "-EncodedCommand"));
        let encoded = args.last().expect("payload");
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
            .expect("base64");
        let script = String::from_utf16(
            &bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>(),
        )
        .expect("utf-16");
        assert!(script.contains("ls -la"), "{script}");
        assert!(!script.contains("Get-ChildItem"), "{script}");
        assert!(script.contains("[Console]::OutputEncoding"), "{script}");
        assert!(
            !script.contains("translated"),
            "launch script must not announce a rewrite: {script}"
        );

        let windows_ps = super::DetectedShell {
            kind: super::ShellKind::WindowsPowerShell,
            program: std::path::PathBuf::from("powershell.exe"),
        };
        let ps_args = super::script_launch_args(&windows_ps, "Get-ChildItem").expect("encode");
        assert_eq!(
            ps_args[..super::POWERSHELL_ARGUMENTS.len()],
            super::POWERSHELL_ARGUMENTS
                .iter()
                .map(|arg| (*arg).to_owned())
                .collect::<Vec<_>>()
        );

        let bash = super::DetectedShell {
            kind: super::ShellKind::Bash,
            program: std::path::PathBuf::from("bash"),
        };
        assert_eq!(
            super::script_launch_args(&bash, "ls -la").unwrap(),
            vec!["-c".to_owned(), "ls -la".to_owned()]
        );
    }

    #[test]
    fn result_text_does_not_announce_a_translation() {
        let plain = super::format_result(
            None,
            "Get-ChildItem -Force",
            "pwsh.exe",
            crate::builtin::process::CapturedStream::default(),
            crate::builtin::process::CapturedStream::default(),
            12,
            false,
            None,
        );
        assert_eq!(result_text(&plain), "");
        let details = plain.details.as_ref().expect("details");
        assert!(details.get("translated_command").is_none());
        assert!(!result_text(&plain).contains("translated"));
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize_captured_shell_text;

    #[test]
    fn clixml_x001b_sgr_fragment_is_plain_text() {
        let raw = "_x001B_[31;1mGet-ChildItem: _x001B_[0m";
        let cleaned = sanitize_captured_shell_text(raw);
        assert_eq!(cleaned, "Get-ChildItem: ");
        assert!(!cleaned.contains("_x001B_"), "{cleaned:?}");
        assert!(!cleaned.contains('\u{1b}'), "{cleaned:?}");
        assert!(!cleaned.contains("[31"), "{cleaned:?}");
        assert!(!cleaned.contains("[0m"), "{cleaned:?}");
    }

    #[test]
    fn raw_esc_csi_sequence_is_stripped() {
        let raw = "\u{1b}[31;1merror\u{1b}[0m";
        assert_eq!(sanitize_captured_shell_text(raw), "error");
    }

    #[test]
    fn clixml_error_node_strips_embedded_ansi() {
        let raw = "\
#< CLIXML
<Objs Version=\"1.1.0.1\" xmlns=\"http://schemas.microsoft.com/powershell/2004/04\"><S S=\"Error\">_x001B_[31;1mGet-ChildItem: _x001B_[0mmissing_x000D__x000A_</S></Objs>";
        let cleaned = sanitize_captured_shell_text(raw);
        assert_eq!(cleaned, "Get-ChildItem: missing");
        assert!(!cleaned.contains("_x001B_"), "{cleaned:?}");
        assert!(!cleaned.contains('\u{1b}'), "{cleaned:?}");
        assert!(!cleaned.contains("#< CLIXML"), "{cleaned:?}");
    }

    #[test]
    fn ordinary_text_and_cr_lf_tab_decoding_still_work() {
        assert_eq!(sanitize_captured_shell_text("plain text"), "plain text");
        assert_eq!(
            sanitize_captured_shell_text("keep trailing\n"),
            "keep trailing\n"
        );
        assert_eq!(
            sanitize_captured_shell_text("line1_x000D__x000A_line2_x0009_end"),
            "line1\r\nline2\tend"
        );
        let blob = "#< CLIXML\n<Objs><S S=\"Error\">left_x000D__x000A_right &amp; &lt;x&gt;</S><S S=\"Warning\">careful</S></Objs>";
        assert_eq!(
            sanitize_captured_shell_text(blob),
            "left\r\nright & <x>\ncareful"
        );
        let untouched = "before\n#< CLIXML\n<Objs><S S=\"Information\">nope</S></Objs>\nafter";
        assert_eq!(sanitize_captured_shell_text(untouched), "before\nafter");
    }

    #[test]
    fn osc_fe_and_invalid_scalars_are_handled() {
        assert_eq!(
            sanitize_captured_shell_text("\u{1b}]0;title\u{7}name"),
            "name"
        );
        assert_eq!(
            sanitize_captured_shell_text("\u{1b}]8;;https://example.test\u{1b}\\link"),
            "link"
        );
        assert_eq!(sanitize_captured_shell_text("\u{1b}(Bplain"), "plain");
        assert_eq!(sanitize_captured_shell_text("a_xD800_b_xDFFF_c"), "abc");
        assert_eq!(
            sanitize_captured_shell_text(
                "\u{1b}[32mbefore\u{1b}[0m\n#< CLIXML\n<Objs></Objs>\n\u{1b}[31mafter\u{1b}[0m"
            ),
            "before\nafter"
        );
    }
}
