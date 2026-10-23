//! Host-side dispatch loop for one script cell.
//!
//! The script thread sends a [`HostRequest`] and parks on a synchronous
//! reply channel; this loop runs on the caller's async runtime, prepares the
//! tool context, executes the tool, journals the result, and answers. No
//! `block_on` ever runs here: the core runtime is `new_current_thread`, so
//! blocking it would stall every other task on the core thread.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::registry::ToolRegistry;
use crate::stream::{ToolStream, ToolStreamItem};
use crate::tool::{ToolDyn, ToolError, ToolResult};
use crate::{ToolCtx, prepare_tool_ctx};

use super::cell::{ScriptCell, ScriptOutcome};
use super::journal::ScriptEventKind;

/// One request from the script thread.
pub enum HostRequest {
    /// `call(name, args)`; the script waits for [`HostReply`].
    Call {
        /// Journal sequence reserved by the script.
        seq: u64,
        /// Registry tool name.
        name: String,
        /// JSON arguments.
        args: Value,
        /// Answer channel. Sending or dropping it unblocks the script.
        reply: std::sync::mpsc::Sender<HostReply>,
    },
    /// `log(text)`; progress only, nothing to answer.
    Log {
        /// Journal sequence.
        seq: u64,
        /// Rendered text.
        text: String,
    },
    /// `complete(value)`; recorded, then the script stops.
    Complete {
        /// Journal sequence.
        seq: u64,
        /// The completion value.
        value: Value,
    },
}

/// The host's answer to a [`HostRequest::Call`].
pub enum HostReply {
    /// The result projection the script receives.
    Value(Value),
    /// The request will not be answered; the script should stop.
    Abort(String),
}

/// Ambient values every inner `call` inherits from the `run_code` context.
#[derive(Clone)]
pub(crate) struct DispatchContext {
    pub(crate) cwd: PathBuf,
    pub(crate) extra_roots: Vec<PathBuf>,
    pub(crate) read_roots: Vec<PathBuf>,
    pub(crate) cancel: CancellationToken,
    pub(crate) call_id: String,
}

/// One progress line from a script is bounded before it reaches the tool
/// stream: the model can build 4 MiB strings, and an unbounded progress
/// channel would let `log` bypass `max_output_bytes`.
const MAX_PROGRESS_BYTES: usize = 2048;

/// Drives one cell until the script finishes or the cell is terminated.
pub(crate) async fn drive_cell(
    cell: Arc<ScriptCell>,
    mut host_rx: mpsc::UnboundedReceiver<HostRequest>,
    mut done: oneshot::Receiver<ScriptOutcome>,
    registry: Arc<ToolRegistry>,
    base: DispatchContext,
    progress: mpsc::UnboundedSender<String>,
) {
    let terminate = cell.terminate_token();
    loop {
        tokio::select! {
            biased;
            // Termination drops `host_rx` on return, which unblocks a parked
            // script thread (its reply sender or request sender is gone).
            () = terminate.cancelled() => return,
            outcome = &mut done => {
                // The script thread sends `done` and closes its host sender
                // in that order; a `log`/`complete` it queued may still be
                // buffered. Drain it before settling so the journal keeps
                // the script's tail.
                drain_queued(&cell, &registry, &base, &progress, &mut host_rx).await;
                cell.finish(outcome.unwrap_or_else(|_| {
                    ScriptOutcome::Failed("the script thread ended without a result".to_owned())
                }));
                return;
            }
            request = host_rx.recv() => match request {
                Some(request) => handle(&cell, &registry, &base, &progress, request).await,
                // The script thread dropped its sender: its outcome was
                // already sent on `done` (or the thread panicked).
                None => {
                    let outcome = (&mut done).await.unwrap_or_else(|_| {
                        ScriptOutcome::Failed(
                            "the script thread ended without a result".to_owned(),
                        )
                    });
                    cell.finish(outcome);
                    return;
                }
            },
        }
    }
}

/// Handles requests the script queued before it ended.
///
/// A `Call` cannot be pending here — the script blocks on its reply — but if
/// one is, it is answered with an abort instead of being dropped.
async fn drain_queued(
    cell: &Arc<ScriptCell>,
    registry: &Arc<ToolRegistry>,
    base: &DispatchContext,
    progress: &mpsc::UnboundedSender<String>,
    host_rx: &mut mpsc::UnboundedReceiver<HostRequest>,
) {
    while let Ok(request) = host_rx.try_recv() {
        handle(cell, registry, base, progress, request).await;
    }
}

async fn handle(
    cell: &Arc<ScriptCell>,
    registry: &Arc<ToolRegistry>,
    base: &DispatchContext,
    progress: &mpsc::UnboundedSender<String>,
    request: HostRequest,
) {
    match request {
        HostRequest::Log { seq, text } => {
            let text = bound_progress(&text);
            cell.record(
                seq,
                ScriptEventKind::Log,
                &json!({ "text": text }),
                None,
            );
            let _ = progress.send(text);
        }
        HostRequest::Complete { seq, value } => {
            cell.record(seq, ScriptEventKind::Complete, &value, None);
        }
        HostRequest::Call {
            seq,
            name,
            args,
            reply,
        } => {
            let call_args = json!({ "name": name, "arguments": args });
            // The engine rejects this before it leaves the script; the check
            // here keeps the property true for any other host path too.
            let outcome = if name == "run_code" || name == "wait_code" {
                Err(format!(
                    "call(\"{name}\") is not allowed: code mode cannot invoke itself"
                ))
            } else {
                run_tool(cell, registry, base, progress, &name, &args, seq).await
            };
            let response = match outcome {
                Ok(result) => json!({
                    "content": serde_json::to_value(&result.content).unwrap_or(Value::Null),
                    "is_error": result.is_error,
                }),
                Err(message) => json!({ "__error__": message }),
            };
            cell.record(seq, ScriptEventKind::Call, &call_args, Some(&response));
            let _ = reply.send(HostReply::Value(response));
        }
    }
}

/// Truncates one progress line at a character boundary.
fn bound_progress(text: &str) -> String {
    if text.len() <= MAX_PROGRESS_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_PROGRESS_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = text[..end].to_owned();
    bounded.push_str("\n(progress truncated)");
    bounded
}

/// Runs one tool through the shared preflight and returns its result.
async fn run_tool(
    cell: &Arc<ScriptCell>,
    registry: &Arc<ToolRegistry>,
    base: &DispatchContext,
    progress: &mpsc::UnboundedSender<String>,
    name: &str,
    args: &Value,
    seq: u64,
) -> Result<ToolResult, String> {
    let Some(tool) = registry.get(name) else {
        return Err(format!(
            "unknown tool: {name}. `call(name, args)` takes a registry tool name; \
             MCP tools go through call(\"use_tool\", …) after search_tool."
        ));
    };
    let ctx = prepare_tool_ctx(
        &base.cwd,
        &base.extra_roots,
        &base.read_roots,
        base.cancel.clone(),
        format!("{}.{seq}", base.call_id),
        tool.as_ref(),
        args,
    )
    .await?;
    let _ = progress.send(format!("call {name}"));
    execute_with_progress(cell, progress, name, tool.as_ref(), args.clone(), ctx).await
}

async fn execute_with_progress(
    cell: &Arc<ScriptCell>,
    progress: &mpsc::UnboundedSender<String>,
    name: &str,
    tool: &dyn ToolDyn,
    args: Value,
    ctx: ToolCtx,
) -> Result<ToolResult, String> {
    let (mut inner_tx, mut inner_rx) = ToolStream::channel();
    let mut execute = std::pin::pin!(tool.execute_dyn(args, &ctx, &mut inner_tx));
    let mut finished: Option<Result<ToolResult, ToolError>> = None;
    let mut inner_open = true;
    let terminate = cell.terminate_token();
    while finished.is_none() {
        tokio::select! {
            biased;
            () = terminate.cancelled() => {
                return Err("terminated before the tool answered".to_owned());
            }
            result = &mut execute => {
                finished = Some(result);
            }
            item = inner_rx.recv(), if inner_open => match item {
                Some(ToolStreamItem::Progress(update)) => {
                    let _ = progress.send(format!("{name}: {}", update.message));
                }
                Some(ToolStreamItem::Terminal(_)) => {}
                None => inner_open = false,
            },
        }
    }
    finished
        .unwrap_or_else(|| {
            Err(ToolError::Execution(
                "tool ended without a result".to_owned(),
            ))
        })
        .map_err(|error| error.to_string())
}
