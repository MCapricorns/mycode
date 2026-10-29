//! Host-side dispatch loop for one script cell.
//!
//! The script thread sends a [`HostRequest`] and parks on a synchronous
//! reply channel; this loop runs on the caller's async runtime, prepares the
//! tool context, executes the tool, journals the result, and answers. No
//! `block_on` ever runs here: the core runtime is `new_current_thread`, so
//! blocking it would stall every other task on the core thread.

use std::path::PathBuf;
use std::sync::Arc;

use mycode_providers::message::ContentBlock;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::registry::ToolRegistry;
use crate::stream::{ToolStream, ToolStreamItem};
use crate::tool::{ToolDyn, ToolResult};
use crate::{ToolCtx, prepare_tool_ctx};

use super::cell::{InnerCallSummary, ScriptCell, ScriptOutcome};
use super::journal::ScriptEventKind;
use super::progress::{CodeModeProgress, InnerCallPhase};

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
    /// `yield_control()`; ordered with the preceding log/call requests.
    Yield {
        /// Journal sequence.
        seq: u64,
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
    progress: broadcast::Sender<String>,
) {
    let terminate = cell.terminate_token();
    // Also cancel detached work when this task is dropped or panics. This
    // is a cell-owned child token, never the parent turn's token.
    let _cancel_on_drop = base.cancel.clone().drop_guard();
    let drive = async {
        loop {
            tokio::select! {
                biased;
                outcome = &mut done => {
                    // Drain the script's queued tail before settling.
                    drain_queued(&cell, &registry, &base, &progress, &mut host_rx).await;
                    cell.finish(outcome.unwrap_or_else(|_| {
                        ScriptOutcome::Failed("the script thread ended without a result".to_owned())
                    }));
                    return;
                }
                request = host_rx.recv() => match request {
                    Some(request) => handle(&cell, &registry, &base, &progress, request).await,
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
    };
    // Race the entire dispatcher, including preflight and tool execution.
    // A tool that ignores its token cannot keep a cancelled cell alive.
    // Dropping execution preserves the builtins' process/worker Drop cleanup.
    tokio::select! {
        biased;
        () = terminate.cancelled() => {}
        () = base.cancel.cancelled() => cell.terminate(),
        () = drive => {}
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
    progress: &broadcast::Sender<String>,
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
    progress: &broadcast::Sender<String>,
    request: HostRequest,
) {
    match request {
        HostRequest::Log { seq, text } => {
            cell.record(seq, ScriptEventKind::Log, &json!({ "text": text }), None);
            let _ = progress.send(bound_progress(&text));
        }
        HostRequest::Yield { seq } => {
            cell.record(seq, ScriptEventKind::Yield, &Value::Null, None);
            cell.mark_yielded();
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
            let mut lifecycle = InnerCallLifecycle::new(cell, base, seq, &name, &args);
            // The engine rejects this before it leaves the script; the check
            // here keeps the property true for any other host path too.
            let outcome = if name == "run_code" || name == "wait_code" {
                Err(format!(
                    "call(\"{name}\") is not allowed: code mode cannot invoke itself"
                ))
            } else {
                run_tool(registry, base, progress, &name, &args, &lifecycle.call_id).await
            };
            // The UI summary rides beside the journal entry: the journal
            // hashes the call, this names it, so a frontend can render the
            // real tool instead of only the outer `run_code`.
            let (is_error, diff) = match &outcome {
                Ok(result) => (
                    result.is_error,
                    result
                        .details
                        .as_ref()
                        .and_then(|details| details.get("diff"))
                        .and_then(Value::as_str)
                        .filter(|diff| !diff.trim().is_empty())
                        .map(str::to_owned),
                ),
                Err(_) => (true, None),
            };
            let response = outcome.and_then(|result| project_result(cell, result));
            lifecycle.finish(&response, is_error || response.is_err(), diff);
            let response = match response {
                Ok(response) => response,
                Err(message) => json!({
                    "content": ToolResult::error(&message).content,
                    "is_error": true,
                    "__error__": message,
                }),
            };
            cell.record(seq, ScriptEventKind::Call, &call_args, Some(&response));
            let _ = reply.send(HostReply::Value(response));
        }
    }
}

/// Returns image metadata only after the whole batch is admitted.
/// Failed admission uses the same error result path as other dispatch refusals.
fn project_result(cell: &ScriptCell, result: ToolResult) -> Result<Value, String> {
    let mut images = Vec::new();
    let content: Vec<Value> = result
        .content
        .into_iter()
        .map(|block| {
            if let ContentBlock::Image(image) = block {
                let metadata = json!({ "Image": {
                    "mime_type": image.mime_type,
                    "data_hash": blake3::hash(image.data.as_bytes()).to_hex().to_string(),
                    "delivery": "native image in run_code/wait_code result",
                }});
                images.push(image);
                metadata
            } else {
                serde_json::to_value(block).unwrap_or(Value::Null)
            }
        })
        .collect();
    cell.queue_images(images)?;
    Ok(json!({ "content": content, "is_error": result.is_error }))
}

/// Owns an inner call's identity and terminal event through cancellation
/// and unwinding. Snapshots retain the same state even after run_code yields
/// and no live progress receiver is attached.
struct InnerCallLifecycle {
    cell: Arc<ScriptCell>,
    cancel: CancellationToken,
    call_id: String,
    summary: InnerCallSummary,
    finished: bool,
}

impl InnerCallLifecycle {
    fn new(
        cell: &Arc<ScriptCell>,
        base: &DispatchContext,
        seq: u64,
        name: &str,
        args: &Value,
    ) -> Self {
        let call_id = format!("{}.{seq}", base.call_id);
        let summary = InnerCallSummary {
            seq,
            call_id: Some(call_id.clone()),
            phase: Some(InnerCallPhase::Running),
            name: name.to_owned(),
            target: crate::tool_target(name, args),
            is_error: false,
            diff: None,
        };
        cell.publish_inner_call(summary.clone());
        Self {
            cell: Arc::clone(cell),
            cancel: base.cancel.clone(),
            call_id,
            summary,
            finished: false,
        }
    }

    fn cancelled(&self) -> bool {
        self.cell.terminated() || self.cancel.is_cancelled()
    }

    fn finish(&mut self, outcome: &Result<Value, String>, is_error: bool, diff: Option<String>) {
        // The legacy AgentHost API reports an independently stopped child
        // as `Execution("subagent <role> cancelled")`. Preserve that state
        // even though its parent cell's token was deliberately not cancelled.
        let cancelled = self.cancelled()
            || outcome.as_ref().err().is_some_and(|message| {
                message.contains(crate::tool::INTERRUPTED_NOTE)
                    || (self.summary.name == "agent"
                        && message.starts_with("subagent ")
                        && message.ends_with(" cancelled"))
            });
        let phase = if cancelled {
            InnerCallPhase::Cancelled
        } else if is_error {
            InnerCallPhase::Failed
        } else {
            InnerCallPhase::Completed
        };
        self.publish(phase, is_error || cancelled, diff);
    }

    fn publish(&mut self, phase: InnerCallPhase, is_error: bool, diff: Option<String>) {
        self.finished = true;
        self.summary.phase = Some(phase);
        self.summary.is_error = is_error;
        self.summary.diff = diff;
        self.cell.publish_inner_call(self.summary.clone());
    }
}

impl Drop for InnerCallLifecycle {
    fn drop(&mut self) {
        if !self.finished {
            let phase = if self.cancelled() {
                InnerCallPhase::Cancelled
            } else {
                InnerCallPhase::Failed
            };
            self.publish(phase, true, None);
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
    registry: &Arc<ToolRegistry>,
    base: &DispatchContext,
    progress: &broadcast::Sender<String>,
    name: &str,
    args: &Value,
    call_id: &str,
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
        call_id,
        tool.as_ref(),
        args,
    )
    .await?;
    execute_with_progress(progress, name, tool.as_ref(), args.clone(), ctx).await
}

async fn execute_with_progress(
    progress: &broadcast::Sender<String>,
    name: &str,
    tool: &dyn ToolDyn,
    args: Value,
    ctx: ToolCtx,
) -> Result<ToolResult, String> {
    let (mut inner_tx, mut inner_rx) = ToolStream::channel();
    let terminal_pusher = inner_tx.clone();
    let mut execute = std::pin::pin!(tool.execute_dyn(args, &ctx, &mut inner_tx));
    let mut streamed_terminal = None;
    let result = loop {
        tokio::select! {
            biased;
            result = &mut execute => break result,
            item = inner_rx.recv() => {
                if let Some(item) = item {
                    forward_item(item, progress, name, &ctx.call_id, &mut streamed_terminal);
                }
            }
        }
    };
    // A synchronously finishing tool may have queued both progress and its
    // terminal in the same poll. Claim the fallback, then drain the FIFO;
    // the stream enforces that the first terminal wins across all clones.
    let error = result.as_ref().err().map(ToString::to_string);
    let claimed_fallback = terminal_pusher.terminal(match result {
        Ok(result) => result,
        Err(error) => ToolResult::error(error.to_string()),
    });
    while let Some(item) = inner_rx.try_recv() {
        forward_item(item, progress, name, &ctx.call_id, &mut streamed_terminal);
    }
    // Keep legacy dispatcher failures (`__error__`) unless the tool already
    // claimed an explicit terminal result before returning its error.
    if claimed_fallback && let Some(error) = error {
        return Err(error);
    }
    streamed_terminal.ok_or_else(|| "tool ended without a result".to_owned())
}

fn forward_item(
    item: ToolStreamItem,
    progress: &broadcast::Sender<String>,
    name: &str,
    call_id: &str,
    terminal: &mut Option<ToolResult>,
) {
    match item {
        ToolStreamItem::Progress(update) => {
            let _ = progress.send(
                CodeModeProgress::Progress {
                    call_id: call_id.to_owned(),
                    name: name.to_owned(),
                    message: update.message,
                }
                .encode(),
            );
        }
        ToolStreamItem::Terminal(result) => *terminal = Some(result),
    }
}
