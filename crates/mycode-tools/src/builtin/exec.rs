//! `exec` — run a kernel-loadable program with an explicit argument vector.
//!
//! The model supplies `program` plus `args`; MYCode never inserts a shell or
//! parses shell syntax. Only PE, ELF, and Mach-O images are launched.
//! Scripts require an explicit interpreter or the shell tool. Execution is
//! unsandboxed current-user execution with normal file and network access;
//! environment allowlisting is not isolation. There is no Core permission
//! prompt: a registered, schema-valid call is dispatched directly.
//! Same-account hostile processes remain outside the security boundary.
//! stdout/stderr are captured with the shared 50 KiB truncation cap; a
//! non-zero exit is an error result, not a tool failure. Timeout and cancel
//! await terminate-and-reap; dropping the future transfers cleanup ownership.
//! Launch is Windows x86_64, Linux x86_64 GNU, and macOS Apple Silicon.
//! Windows ARM64 and other Unix (musl, Android, BSD) are unsupported. The
//! spawn helpers stay compiled there and return before launch, so their
//! unused items are expected on those targets.
#![cfg_attr(
    not(any(
        all(windows, target_arch = "x86_64"),
        all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )),
    allow(dead_code, unused_imports, unused_variables)
)]
#[cfg(all(windows, target_arch = "x86_64"))]
mod argv;
mod env;
mod image;
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
mod linux;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod macos;
mod prepare;
mod resolve;
mod spawn;
#[cfg(any(
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod unix;
#[cfg(all(windows, target_arch = "x86_64"))]
mod windows;

use std::ffi::OsString;
use std::path::Path;
use std::pin::Pin;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::time::Sleep;
use tokio_util::sync::CancellationToken;

use crate::builtin::blocking::run_blocking_supervised;
use crate::builtin::process::{
    CapturedStream, ExecutionLease, MAX_OUTPUT_BYTES, acquire_execution_lease, decode_captured_text,
};
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

use prepare::environment_summary;
use resolve::encode_hex;

/// Typed pin/resolve failure used by the shell candidate fallback.
pub(crate) use prepare::PreparedInvocation;
pub(crate) use spawn::{ExecutionMetadata, RunOutcome};

/// Pin/resolve failure with a typed executable-not-found case.
///
/// Only [`Self::NotFound`] may try another shell candidate. Image, identity,
/// digest, permission, and cancellation failures stay [`Self::Other`].
#[derive(Debug)]
pub(crate) enum ResolveError {
    /// PATH lookup or opening the resolved path found no executable file.
    NotFound {
        /// Requested program or basename, not a host path dump.
        program: String,
        /// Absolute PATH entries searched; `None` when a path open missed.
        searched: Option<usize>,
    },
    /// Any other rejection; callers must fail closed.
    Other(ToolError),
}

impl ResolveError {
    /// Whether another candidate may be attempted.
    #[must_use]
    pub(crate) const fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }

    pub(super) fn path_not_found(path: &Path) -> Self {
        let program = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "program".to_owned());
        Self::NotFound {
            program,
            searched: None,
        }
    }

    /// Converts this failure into the public tool error.
    #[must_use]
    pub(crate) fn into_tool_error(self) -> ToolError {
        match self {
            not_found @ Self::NotFound { .. } => ToolError::InvalidArgs(not_found.to_string()),
            Self::Other(error) => error,
        }
    }
}

impl From<ToolError> for ResolveError {
    fn from(error: ToolError) -> Self {
        Self::Other(error)
    }
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound {
                program,
                searched: Some(searched),
            } => write!(
                f,
                "program {program} not found on PATH ({searched} directories searched)"
            ),
            Self::NotFound {
                program,
                searched: None,
            } => write!(f, "program {program} not found"),
            Self::Other(error) => write!(f, "{error}"),
        }
    }
}

/// Default command timeout (seconds), matching the shell tool.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Maximum argument lengths retained in the redacted result summary.
///
/// The full vector is represented by a length-framed digest, so increasing
/// this limit only adds UI/session metadata and does not improve identity.
const MAX_ARGUMENT_LENGTH_SUMMARY: usize = 64;

/// Captured launch identity for UI details after preparation succeeds.
#[derive(Clone, Debug)]
pub(crate) struct PreparedIdentity {
    /// Canonical native path of the pinned executable.
    pub program: String,
    /// SHA-256 digest of the pinned image bytes.
    pub digest: String,
    /// SHA-256 invocation digest over path, identity, argv, cwd, and env.
    pub invocation_digest: String,
    /// Native file-identity token (volume/file id or device/inode).
    pub image_identity: String,
    /// Classified kernel image kind (`pe`, `elf`, or `mach-o`).
    pub image: &'static str,
    /// Redacted environment lengths; values are never copied here.
    pub env_summary: Value,
}

/// Snapshots the allowlisted child environment for one structured launch.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArgs`] when the reconstructed block exceeds budget.
pub(crate) fn snapshot_child_environment() -> Result<Vec<(OsString, OsString)>, ToolError> {
    env::snapshot_child_environment()
}

/// Pins `program` and preserves its requested spelling as `argv[0]`.
///
/// The environment is supplied by a previously captured snapshot.
///
/// # Errors
///
/// Returns [`ResolveError::NotFound`] when PATH or path lookup misses the
/// executable, and [`ResolveError::Other`] for every fail-closed rejection.
pub(crate) fn prepare_from_snapshot(
    session_cwd: &Path,
    program: &str,
    args: &[String],
    env: &[(OsString, OsString)],
    cancel: &CancellationToken,
) -> Result<PreparedInvocation, ResolveError> {
    PreparedInvocation::from_snapshot_with_argv0(session_cwd, program, program, args, env, cancel)
}

/// Extracts redacted launch identity before the snapshot is consumed by spawn.
#[must_use]
pub(crate) fn prepared_identity(prepared: &PreparedInvocation) -> PreparedIdentity {
    let program = prepared
        .canonical_path()
        .to_str()
        .expect("pin_program validated canonical path Unicode")
        .to_owned();
    PreparedIdentity {
        program,
        digest: encode_hex(prepared.image_digest()),
        invocation_digest: encode_hex(prepared.invocation_digest()),
        image_identity: prepared.image_identity().debug_token(),
        image: image_kind_label(prepared.image_kind()),
        env_summary: environment_summary(prepared.env(), MAX_ARGUMENT_LENGTH_SUMMARY),
    }
}

fn image_kind_label(kind: image::ImageKind) -> &'static str {
    match kind {
        image::ImageKind::Elf => "elf",
        image::ImageKind::Pe => "pe",
        image::ImageKind::MachO { fat: true } => "mach-o-fat",
        image::ImageKind::MachO { fat: false } => "mach-o",
    }
}

/// Spawns a prepared invocation through the structured-exec broker.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] or [`ToolError::InvalidArgs`] when spawn
/// itself fails. Collection failures are returned as [`RunOutcome`].
pub(crate) async fn run_prepared(
    prepared: PreparedInvocation,
    lease: ExecutionLease,
    cancel: &CancellationToken,
    deadline: &mut Pin<&mut Sleep>,
) -> Result<RunOutcome, ToolError> {
    spawn::run_pinned(prepared, lease, cancel, deadline).await
}

/// Writes image, digest, identity, and redacted env summary onto tool details.
pub(crate) fn apply_execution_details(
    details: &mut Value,
    identity: &PreparedIdentity,
    metadata: ExecutionMetadata,
) {
    details["program"] = json!(identity.program);
    details["image"] = json!(identity.image);
    details["image_identity"] = json!(identity.image_identity);
    details["digest_sha256"] = json!(identity.digest);
    details["invocation_digest_sha256"] = json!(identity.invocation_digest);
    details["identity"] = json!(execution_identity(&identity.invocation_digest, metadata));
    details["env_summary"] = identity.env_summary.clone();
    if let Some(architecture) = metadata.loaded_architecture() {
        details["loaded_architecture"] = json!(architecture);
    }
    if let Some(translated) = metadata.translated() {
        details["translated"] = json!(translated);
    }
}

/// The `exec` builtin.
#[derive(Debug)]
pub struct ExecTool {
    default_timeout: Duration,
}

impl ExecTool {
    /// An exec tool with the default 120 s timeout.
    pub fn new() -> Self {
        Self {
            default_timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// An exec tool with a custom default timeout; per-call `timeout_secs`
    /// arguments still take precedence.
    #[must_use]
    pub fn with_default_timeout(secs: u64) -> Self {
        Self {
            default_timeout: Duration::from_secs(secs),
        }
    }
}

impl Default for ExecTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Arguments for [`ExecTool`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecArgs {
    /// Executable to run: a bare basename searched only in absolute host PATH
    /// entries, or a path resolved against the session cwd. Must be a
    /// kernel-loadable PE, ELF, or Mach-O image.
    pub program: String,
    /// Argument vector passed to the program verbatim — no shell parsing,
    /// quoting, or expansion is applied to these strings.
    #[serde(default)]
    pub args: Vec<String>,
    /// Timeout in seconds for this run (default: 120).
    pub timeout_secs: Option<u64>,
}

#[async_trait]
impl Tool for ExecTool {
    type Args = ExecArgs;
    type Output = ();

    fn name(&self) -> &str {
        "exec"
    }

    fn description(&self) -> &str {
        "Run a program directly with an explicit argument vector. MYCode does \
         not insert a shell or parse shell syntax. Only kernel-loadable PE, \
         ELF, or Mach-O images are launched; shebang scripts, batch files, \
         and implicit interpreter fallback are rejected. `program` is a bare \
         name resolved against absolute host PATH entries (never the working \
         directory) or a path resolved against the session cwd. Execution is \
         unsandboxed \
         current-user execution with normal file and network access; it is \
         not a sandbox. Same-account processes outside this host are outside \
         the security boundary. Captured stdout/stderr is truncated beyond \
         50 KiB; a non-zero exit is an error result, not a tool failure. \
         Default timeout: 120 s. There is no Core permission prompt."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "exec: run one kernel-loadable program with explicit arguments and \
             no shell parsing (program, args[], optional timeout_secs).",
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
        let started = Instant::now();
        if ctx.cancel.is_cancelled() {
            return Err(command_cancelled_error(None));
        }
        resolve::validate_request(&args.program, &args.args)?;

        let program_arg = args.program.clone();
        let argv = args.args.clone();
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let lease = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(command_cancelled_error(None)),
            _ = &mut deadline => {
                return Ok(timed_out_before_spawn_result(
                    &program_arg,
                    &argv,
                    started.elapsed().as_millis() as u64,
                    timeout,
                ));
            }
            lease = acquire_execution_lease() => lease,
        };
        let cwd = ctx.cwd.clone();
        let program = args.program;
        let pin_args = args.args;
        let pin_work =
            run_blocking_supervised("exec resolution", &ctx.cancel, move |worker_cancel| {
                let prepared =
                    PreparedInvocation::prepare(&cwd, &program, &pin_args, &worker_cancel);
                Ok((prepared?, lease))
            });
        tokio::pin!(pin_work);

        let (prepared, lease) = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(command_cancelled_error(None)),
            _ = &mut deadline => {
                return Ok(timed_out_before_spawn_result(
                    &program_arg,
                    &argv,
                    started.elapsed().as_millis() as u64,
                    timeout,
                ));
            }
            prepared = &mut pin_work => prepared?,
        };
        if ctx.cancel.is_cancelled() {
            return Err(command_cancelled_error(None));
        }

        let program = prepared
            .canonical_path()
            .to_str()
            .expect("pin_program validated canonical path Unicode")
            .to_owned();
        let digest = encode_hex(prepared.image_digest());
        let invocation_digest = encode_hex(prepared.invocation_digest());
        let image_identity = prepared.image_identity().debug_token();
        let env_summary = environment_summary(prepared.env(), MAX_ARGUMENT_LENGTH_SUMMARY);
        let image = match prepared.image_kind() {
            image::ImageKind::Elf => "elf",
            image::ImageKind::Pe => "pe",
            image::ImageKind::MachO { fat: true } => "mach-o-fat",
            image::ImageKind::MachO { fat: false } => "mach-o",
        };
        let outcome = spawn::run_pinned(prepared, lease, &ctx.cancel, &mut deadline).await?;
        let duration_ms = started.elapsed().as_millis() as u64;
        match outcome {
            RunOutcome::Done {
                status,
                stdout,
                stderr,
                metadata,
            } => {
                let execution_identity = execution_identity(&invocation_digest, metadata);
                Ok(with_image_metadata(
                    format_result(
                        Some(status),
                        &program,
                        &argv,
                        &execution_identity,
                        &digest,
                        stdout,
                        stderr,
                        duration_ms,
                        false,
                        None,
                    ),
                    image,
                    metadata,
                    &image_identity,
                    &invocation_digest,
                    &env_summary,
                ))
            }
            RunOutcome::CollectFailed { error, teardown } => {
                Err(collection_error(&error, teardown.err()))
            }
            RunOutcome::Timeout {
                stdout,
                stderr,
                teardown,
                started,
                metadata,
            } => {
                let execution_identity = execution_identity(&invocation_digest, metadata);
                Ok(with_image_metadata(
                    timed_out_result(
                        &program,
                        &argv,
                        &execution_identity,
                        &digest,
                        stdout,
                        stderr,
                        duration_ms,
                        timeout,
                        teardown,
                        started,
                    ),
                    image,
                    metadata,
                    &image_identity,
                    &invocation_digest,
                    &env_summary,
                ))
            }
            RunOutcome::Cancelled { teardown } => Err(command_cancelled_error(teardown.err())),
        }
    }
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
    program: &str,
    argv: &[String],
    duration_ms: u64,
    timeout: Duration,
) -> ToolResult {
    let notice = format!(
        "[command timed out after {}s before the program started]",
        timeout.as_secs()
    );
    mark_timed_out(format_result(
        None,
        program,
        argv,
        "",
        "",
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
    program: &str,
    argv: &[String],
    identity: &str,
    digest: &str,
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
            "[command timed out after {}s before the program started]",
            timeout.as_secs()
        ),
    };
    mark_timed_out(format_result(
        None,
        program,
        argv,
        identity,
        digest,
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

fn execution_identity(invocation_digest: &str, metadata: ExecutionMetadata) -> String {
    match (metadata.loaded_architecture(), metadata.translated()) {
        (Some(architecture), Some(translated)) => {
            format!("{invocation_digest} arch:{architecture} translated:{translated}")
        }
        _ => invocation_digest.to_owned(),
    }
}

fn with_image_metadata(
    mut result: ToolResult,
    image: &str,
    metadata: ExecutionMetadata,
    image_identity: &str,
    invocation_digest: &str,
    env_summary: &Value,
) -> ToolResult {
    let details = result.details.as_mut().expect("details were populated");
    details["image"] = json!(image);
    details["image_identity"] = json!(image_identity);
    details["invocation_digest_sha256"] = json!(invocation_digest);
    details["env_summary"] = env_summary.clone();
    if let Some(architecture) = metadata.loaded_architecture() {
        details["loaded_architecture"] = json!(architecture);
    }
    if let Some(translated) = metadata.translated() {
        details["translated"] = json!(translated);
    }
    result
}

#[expect(
    clippy::too_many_arguments,
    reason = "result assembly mirrors the tool's stable output fields"
)]
fn format_result(
    status: Option<std::process::ExitStatus>,
    program: &str,
    argv: &[String],
    identity: &str,
    digest: &str,
    stdout: CapturedStream,
    stderr: CapturedStream,
    duration_ms: u64,
    forced_error: bool,
    notice: Option<&str>,
) -> ToolResult {
    let stdout_text = decode_captured_text(&stdout.retained);
    let stderr_text = decode_captured_text(&stderr.retained);

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
        "program": program,
        "args_count": argv.len(),
        "args_digest_sha256": argument_digest(argv),
        "args_summary": argument_summary(argv),
        "identity": identity,
        "digest_sha256": digest,
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

fn argument_digest(argv: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(u64::try_from(argv.len()).unwrap_or(u64::MAX).to_be_bytes());
    for argument in argv {
        hasher.update(
            u64::try_from(argument.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        hasher.update(argument.as_bytes());
    }
    let digest: [u8; 32] = hasher.finalize().into();
    encode_hex(&digest)
}

fn argument_summary(argv: &[String]) -> Value {
    let byte_lengths: Vec<usize> = argv
        .iter()
        .take(MAX_ARGUMENT_LENGTH_SUMMARY)
        .map(String::len)
        .collect();
    json!({
        "byte_lengths": byte_lengths,
        "omitted": argv.len().saturating_sub(MAX_ARGUMENT_LENGTH_SUMMARY),
    })
}

fn display_exit(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}
