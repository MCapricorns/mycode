//! The process-launch tool. The model sees exactly one shell tool, chosen
//! once when the process starts and kept for the whole session.
//!
//! Two modes share that name, one schema, and one prompt entry:
//!
//! * `script` runs `command` in the resolved interpreter. Windows prefers
//!   PowerShell 7 (`pwsh`), then Windows PowerShell 5.1 (`powershell.exe`).
//!   `cmd.exe` is the runtime fallback when neither exists. Git Bash, MSYS2,
//!   Cygwin, and WSL are not selected. macOS uses `$SHELL` when it is bash,
//!   zsh, or sh, then falls back to `zsh`, `bash`, then `sh`. Commands are
//!   not rewritten between shells.
//! * `program` spawns `program` with an explicit `args` vector and does not
//!   parse a shell command string. A PE, ELF, or Mach-O image runs directly.
//!   A direct shebang wrapper runs its interpreter with the script path and
//!   `args`. Batch files and shebang arguments such as `-c` are rejected.
//!
//! Both modes pin the launched image, snapshot cwd/env/PATH once per call,
//! allowlist the child environment, and use contained spawn. Execution runs
//! with current-user file and network authority. Environment filtering is
//! not a sandbox. Valid calls run directly with no Core permission prompt.
//! `write` and `edit` stay available. `read`, `grep`, and `find` stay
//! in-process. File edits from either path are not undone.
use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

mod detect;
mod prompt;
mod sanitize;

pub use detect::{DetectedShell, ShellKind, active_shell, detect_default_shell, resolved_shell};
pub use prompt::render_environment_block;
pub(crate) use sanitize::sanitize_captured_shell_text;

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
    CapturedStream, ExecutionLease, MAX_OUTPUT_BYTES, acquire_execution_lease, captured_text,
    collection_error, command_cancelled_error, display_exit, mark_timed_out,
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
/// the child's output encodings to UTF-8 so its own text (error records,
/// cmdlet output, `Write-Output`) reaches the pipe as UTF-8 instead of the
/// system OEM page (GBK on zh-CN hosts), which decodes as mojibake under a
/// UTF-8 capture.
///
/// Only pure property and variable assignments may appear here. Invoking any
/// cmdlet — even `New-Object` or `Out-Null` — runs the first pipeline,
/// emits the first-run progress records, and locks the redirected stderr
/// writer to the OEM page before the encoding is fixed (observed on Windows
/// PowerShell 5.1: stdout turned UTF-8 while stderr stayed GBK). Each
/// assignment carries its own `try`/`catch` so a host that refuses one still
/// applies the rest: a console-less process cannot always set
/// `[Console]::InputEncoding`, and ConstrainedLanguage may reject a property
/// set. `pwsh` 7.4+ preserves native .exe output bytes regardless of these
/// settings; only PowerShell's own text is covered. When the prelude cannot
/// run at all, `decode_captured_text` still falls back to BOM, BOM-less
/// UTF-16LE, and OEM/ANSI code-page detection.
const POWERSHELL_UTF8_PRELUDE: &str = "try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }; try { $OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }; try { [Console]::InputEncoding = [System.Text.Encoding]::UTF8 } catch { }";

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
    /// `script` runs `command` in the platform shell. `program` runs one kernel
    /// image, or a direct shebang wrapper via its interpreter, with `program`
    /// and `args`. It does not parse a shell command string. Omitted `mode`
    /// means `script`.
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
            lint_powershell_command(command)?;
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
    // Strip a leading UTF-8 BOM so a leading `using` statement remains the
    // first token PowerShell sees after `-EncodedCommand` decoding.
    let command = command.strip_prefix('\u{feff}').unwrap_or(command);
    if command.is_empty() {
        // PowerShell 7 rejects an empty -EncodedCommand payload as not Base64.
        script.push_str("#\n");
        script.push_str(POWERSHELL_UTF8_PRELUDE);
        return script;
    }
    let prologue = powershell_prologue_units(command);
    script.push_str(&command[..prologue]);
    if !script.is_empty() && !script.ends_with('\n') && !script.ends_with(';') {
        script.push('\n');
    }
    script.push_str(POWERSHELL_UTF8_PRELUDE);
    let rest = &command[prologue..];
    if rest.trim_start().starts_with(';') {
        script.push(' ');
    } else {
        script.push('\n');
    }
    script.push_str(rest);
    script
}

/// Byte offset of the end of the leading statement-ordering prologue —
/// whitespace, line/block comments, `using` statements, script attributes,
/// and one `param (...)` block — which PowerShell requires (`using`) or
/// expects (`param` and its attributes) before all other statements. The
/// UTF-8 prelude is inserted right after it, so those forms keep their
/// required position.
fn powershell_prologue_units(command: &str) -> usize {
    let mut index = 0_usize;
    let mut saw_attribute = false;
    while index < command.len() {
        let next = skip_ps_trivia(command, index);
        if next > index {
            index = next;
            continue;
        }
        if !saw_attribute && let Some(after_using) = scan_ps_using(command, index) {
            index = after_using;
            continue;
        }
        if let Some(after_param) = scan_ps_param_block(command, index) {
            index = after_param;
            break;
        }
        if let Some(after_attr) = scan_ps_attribute(command, index) {
            let after_trivia = skip_ps_trivia(command, after_attr);
            if scan_ps_attribute(command, after_trivia).is_some()
                || scan_ps_param_block(command, after_trivia).is_some()
            {
                saw_attribute = true;
                index = after_attr;
                continue;
            }
        }
        break;
    }
    if index > 0 && index < command.len() {
        index = consume_trailing_line_break(command, index);
    }
    index
}

fn consume_trailing_line_break(text: &str, mut index: usize) -> usize {
    let bytes = text.as_bytes();
    while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
        index += 1;
    }
    if index < bytes.len() && bytes[index] == b'#' && !text[index..].starts_with("<#") {
        while index < bytes.len() && !matches!(bytes[index], b'\r' | b'\n') {
            index += 1;
        }
    }
    if index < bytes.len() && bytes[index] == b'\r' {
        index += 1;
    }
    if index < bytes.len() && bytes[index] == b'\n' {
        index += 1;
    }
    index
}

fn skip_ps_trivia(text: &str, mut index: usize) -> usize {
    while index < text.len() {
        let rest = &text[index..];
        let Some(ch) = rest.chars().next() else {
            break;
        };
        if ch.is_whitespace() || ch == '\u{feff}' {
            index += ch.len_utf8();
            continue;
        }
        if let Some(comment) = rest.strip_prefix("<#") {
            index = match comment.find("#>") {
                Some(end) => index + 2 + end + 2,
                None => text.len(),
            };
            continue;
        }
        if ch == '#' {
            index = match rest.find('\n') {
                Some(nl) => index + nl + 1,
                None => text.len(),
            };
            continue;
        }
        break;
    }
    index
}

fn scan_ps_using(text: &str, start: usize) -> Option<usize> {
    let rest = &text[start..];
    if !rest
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("using"))
    {
        return None;
    }
    let after_kw = rest[5..].chars().next()?;
    if !after_kw.is_whitespace() || matches!(after_kw, '\r' | '\n') {
        return None;
    }
    let mut index = start + 5;
    while index < text.len() {
        let slice = &text[index..];
        if let Some(comment) = slice.strip_prefix("<#") {
            index = match comment.find("#>") {
                Some(end) => index + 2 + end + 2,
                None => text.len(),
            };
            continue;
        }
        let ch = slice.chars().next()?;
        match ch {
            '\r' | '\n' => return Some(consume_trailing_line_break(text, index)),
            ';' => return Some(index + 1),
            '#' => return Some(consume_trailing_line_break(text, index)),
            '\'' | '"' => index = skip_ps_quoted_string(text, index),
            '{' => index = scan_ps_balanced(text, index, '{', '}')?,
            '(' => index = scan_ps_balanced(text, index, '(', ')')?,
            _ => index += ch.len_utf8(),
        }
    }
    Some(text.len())
}

fn scan_ps_attribute(text: &str, start: usize) -> Option<usize> {
    if text.as_bytes().get(start) != Some(&b'[') {
        return None;
    }
    scan_ps_balanced(text, start, '[', ']')
}

fn scan_ps_param_block(text: &str, start: usize) -> Option<usize> {
    let rest = &text[start..];
    if !rest
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("param"))
    {
        return None;
    }
    let after_kw = start + 5;
    if let Some(ch) = text[after_kw..].chars().next()
        && (ch.is_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return None;
    }
    let open = skip_ps_trivia(text, after_kw);
    if text.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    scan_ps_balanced(text, open, '(', ')')
}

fn scan_ps_balanced(text: &str, start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0_usize;
    let mut index = start;
    while index < text.len() {
        let slice = &text[index..];
        if let Some(comment) = slice.strip_prefix("<#") {
            index += comment.find("#>")? + 4;
            continue;
        }
        let ch = slice.chars().next()?;
        if ch == '#' {
            index += slice.find('\n')? + 1;
            continue;
        }
        if matches!(ch, '\'' | '"') {
            index = skip_ps_quoted_string(text, index);
            continue;
        }
        if ch == open {
            depth += 1;
            index += ch.len_utf8();
            continue;
        }
        if ch == close {
            depth = depth.saturating_sub(1);
            index += ch.len_utf8();
            if depth == 0 {
                return Some(index);
            }
            continue;
        }
        index += ch.len_utf8();
    }
    None
}

fn skip_ps_quoted_string(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let quote = bytes[start];
    let mut index = start + 1;
    while index < text.len() {
        let ch = text[index..].chars().next().expect("char boundary");
        if quote == b'"' && ch == '`' {
            index += 1;
            if let Some(escaped) = text[index..].chars().next() {
                index += escaped.len_utf8();
            }
            continue;
        }
        if ch == char::from(quote) {
            index += 1;
            if bytes.get(index) == Some(&quote) {
                index += 1;
                continue;
            }
            return index;
        }
        index += ch.len_utf8();
    }
    text.len()
}

/// Skips a PowerShell here-string (`@'\n...\n'@` or `@"\n...\n"@`) when
/// `start` points at the leading `@`.
fn skip_ps_here_string(text: &str, start: usize) -> Option<usize> {
    let rest = &text[start..];
    let (quote, after_open) = if let Some(tail) = rest.strip_prefix("@'") {
        ('\'', tail)
    } else {
        ('"', rest.strip_prefix("@\"")?)
    };
    let line_end = if after_open.starts_with("\r\n") {
        2
    } else if after_open.starts_with('\n') {
        1
    } else {
        return None;
    };
    let mut index = start + 2 + line_end;
    let close = if quote == '\'' { "'@" } else { "\"@" };
    while index < text.len() {
        if text[index..].starts_with(close) {
            return Some(index + 2);
        }
        index = match text[index..].find('\n') {
            Some(nl) => index + nl + 1,
            None => text.len(),
        };
    }
    Some(text.len())
}

/// Rejects bash-only commands (`touch`, `export`, `source`, `which`, `chmod`,
/// `chown`, `sudo`, Unix package managers, `man`) at command position and
/// bash-only redirection/heredoc tokens (`>/dev/null`, `<<`) outside strings
/// and comments before PowerShell runs a script halfway through.
fn lint_powershell_command(command: &str) -> Result<(), ToolError> {
    let mut index = 0_usize;
    let mut at_command_start = true;
    while index < command.len() {
        let rest = &command[index..];
        if let Some(comment) = rest.strip_prefix("<#") {
            index = match comment.find("#>") {
                Some(end) => index + 2 + end + 2,
                None => command.len(),
            };
            continue;
        }
        let ch = rest.chars().next().expect("non-empty slice");
        match ch {
            ' ' | '\t' | '\u{feff}' => {
                index += ch.len_utf8();
                continue;
            }
            '\r' | '\n' | ';' => {
                index += 1;
                at_command_start = true;
                continue;
            }
            '#' => {
                index = match rest.find('\n') {
                    Some(nl) => index + nl,
                    None => command.len(),
                };
                continue;
            }
            '@' => {
                if let Some(after_here) = skip_ps_here_string(command, index) {
                    index = after_here;
                    at_command_start = false;
                    continue;
                }
                index += 1;
                if matches!(command.as_bytes().get(index), Some(b'{' | b'(')) {
                    index += 1;
                }
                at_command_start = false;
                continue;
            }
            '\'' | '"' => {
                index = skip_ps_quoted_string(command, index);
                at_command_start = false;
                continue;
            }
            '<' if rest.starts_with("<<") => {
                return Err(ToolError::InvalidArgs(
                    "`<<` heredocs are bash syntax and are rejected in PowerShell; use a here-string (`@'\\n...\\n'@`)"
                        .into(),
                ));
            }
            '>' => {
                let after_gt = rest.trim_start_matches('>').trim_start_matches([' ', '\t']);
                if let Some(after_null) = after_gt.strip_prefix("/dev/null")
                    && after_null.chars().next().is_none_or(|next| {
                        next.is_whitespace() || matches!(next, ';' | '|' | '&' | ')' | '}')
                    })
                {
                    return Err(ToolError::InvalidArgs(
                        "`/dev/null` does not exist in PowerShell; redirect to `$null` (for example `2>$null`)"
                            .into(),
                    ));
                }
                index += 1;
                at_command_start = false;
                continue;
            }
            '|' => {
                index += if rest.starts_with("||") { 2 } else { 1 };
                at_command_start = true;
                continue;
            }
            '&' if rest.starts_with("&&") => {
                index += 2;
                at_command_start = true;
                continue;
            }
            '{' | '(' => {
                index += 1;
                at_command_start = true;
                continue;
            }
            _ => {}
        }
        if at_command_start && ch.is_ascii_alphabetic() {
            let word_len = rest
                .bytes()
                .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                .count();
            let word = &rest[..word_len];
            let after_word = &rest[word_len..];
            let next_char = after_word.chars().next();
            if matches!(next_char, Some(' ' | '\t')) {
                let after_space = after_word.trim_start_matches([' ', '\t']);
                if let Some(arg_start) = after_space.chars().next()
                    && !matches!(
                        arg_start,
                        '\r' | '\n' | ';' | '|' | '&' | '}' | ')' | '#' | '='
                    )
                    && let Some(advice) = powershell_unix_command_advice(word)
                {
                    return Err(ToolError::InvalidArgs(advice.into()));
                }
            }
            index += word_len;
            at_command_start = false;
            continue;
        }
        at_command_start = false;
        index += ch.len_utf8();
    }
    Ok(())
}

fn powershell_unix_command_advice(word: &str) -> Option<&'static str> {
    // PowerShell commands are case-insensitive; `EXPORT` bypassed the match.
    match word.to_ascii_lowercase().as_str() {
        "touch" => Some(
            "`touch` is a Unix command and does not exist in PowerShell; use `(Get-Item <path>).LastWriteTime = Get-Date` to update a timestamp or `New-Item -ItemType File <path>` to create a file",
        ),
        "export" => Some(
            "`export` is a bash builtin and does not exist in PowerShell; use `$env:NAME = \"value\"`",
        ),
        "source" => Some(
            "`source` is a bash builtin and does not exist in PowerShell; dot-source scripts with `. .\\script.ps1`",
        ),
        "which" => Some(
            "`which` is a Unix command and does not exist in PowerShell; use `Get-Command <name>`",
        ),
        "chmod" | "chown" => Some(
            "`chmod`/`chown` are Unix commands and do not exist in PowerShell; use `icacls` or `Set-Acl` on Windows",
        ),
        "sudo" => Some("`sudo` is a Unix command and does not exist in PowerShell"),
        "apt" | "apt-get" | "yum" | "dnf" | "brew" => Some(
            "Unix package managers (`apt`, `yum`, `dnf`, `brew`) do not exist on Windows; use `winget` or `scoop`",
        ),
        // Windows PowerShell aliases `man` to `help`, which pipes through a
        // pager; the captured, input-less process has nobody to page it.
        "man" => Some("`man` runs the paging `help` function; use `Get-Help <cmdlet>`"),
        _ => None,
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
    let (stdout_text, stdout_cut) = captured_text(&stdout);
    let (stderr_text, stderr_cut) = captured_text(&stderr);
    let stdout_text = sanitize_captured_shell_text(&stdout_text);
    let stderr_text = sanitize_captured_shell_text(&stderr_text);

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

    let (mut text, cut) = crate::builtin::truncate_middle(&text, MAX_OUTPUT_BYTES);
    let truncated = cut || stdout_cut || stderr_cut;
    if truncated {
        let total = stdout.total_bytes.saturating_add(stderr.total_bytes);
        let _ = write!(
            text,
            "\n[output truncated: showed the start and end of {total} bytes]"
        );
    }

    let is_error = forced_error || status.is_some_and(|s| !s.success());
    if is_error {
        match &status {
            Some(s) => {
                let _ = write!(text, "\n[exit code: {}]", display_exit(s));
            }
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

#[cfg(test)]
mod mode_tests;
