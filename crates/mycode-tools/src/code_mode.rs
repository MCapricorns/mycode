//! Code mode: `run_code` submits a model-written JavaScript script that calls
//! registered tools as first-class functions; `wait_code` collects or stops
//! a long-running cell.
//!
//! The script runs on its own `std::thread`; this module never blocks the
//! async runtime. See `wiki/architecture/code-mode-engine.md`.

pub mod cell;
pub mod dispatch;
pub mod engine;
pub mod journal;
pub mod progress;

use std::sync::{Arc, Weak};
use std::time::Duration;

use futures_util::FutureExt;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::ctx::ToolCtx;
use crate::registry::ToolRegistry;
use crate::stream::ToolStream;
use crate::tool::{INTERRUPTED_NOTE, Tool, ToolError, ToolResult};

pub use cell::{
    CellPhase, CellSnapshot, CodeCellState, CodeCells, DEFAULT_MAX_OUTPUT_BYTES, DEFAULT_YIELD_MS,
    InnerCallSummary, MAX_HOST_CALLS, MAX_YIELD_MS, ScriptCell, ScriptEvent, ScriptOutcome,
};
pub use journal::{JournalEntry, ScriptEventKind, ScriptJournal};
pub use progress::{CODE_MODE_PROGRESS_PREFIX, CodeModeProgress, InnerCallPhase};

use self::dispatch::DispatchContext;

/// `run_code` arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunCodeArgs {
    /// JavaScript source only (no Node.js or browser APIs). Every tool is
    /// `call(name, args)`, where `name` is a registry tool name;
    /// `complete(value)` ends the script.
    pub script: String,
    /// How long to wait before returning a cell id (default 30000 ms, at
    /// most 300000 ms).
    pub yield_time_ms: Option<u64>,
    /// Cap on `log`/`complete` output (default 32768 bytes).
    pub max_output_bytes: Option<usize>,
}

/// Runs a model-written JavaScript script against the turn's tool registry.
///
/// The registry is held weakly on purpose: the registry owns this tool, so a
/// strong handle would make every turn's registry (and its tools, including
/// live MCP connections) leak for the life of the process. While a script is
/// running, its dispatcher keeps the upgraded `Arc` alive; when the cell
/// ends, the registry can go.
pub struct RunCodeTool {
    registry: Weak<ToolRegistry>,
    cells: Arc<CodeCells>,
    scope: String,
}

impl RunCodeTool {
    /// Binds the registry scripts call into, the cross-turn cell table, and
    /// the session whose cells this tool may start or collect.
    #[must_use]
    pub fn new(registry: Arc<ToolRegistry>, cells: Arc<CodeCells>, scope: String) -> Self {
        Self {
            registry: Arc::downgrade(&registry),
            cells,
            scope,
        }
    }
}

impl Tool for RunCodeTool {
    type Args = RunCodeArgs;
    type Output = String;

    fn name(&self) -> &str {
        "run_code"
    }

    fn description(&self) -> &str {
        "Run synchronous JavaScript in embedded QuickJS (JavaScript only; no Node.js, fs, \
         require, fetch, network globals, imports or async/await). `call(name, args)` invokes a registered \
         tool and returns `{ content, is_error }`; read a text block as `result.content[0].Text`. \
         Images are emitted natively in run_code/wait_code results; the script receives image \
         metadata without base64. Dispatch refusals also include the legacy `__error__` message. `log(text)` reports \
         progress; `complete(value)` ends the script and returns \
         value. `search_tool`, `use_tool`, `read`, `write`, `grep`, `find` and the rest are \
         ordinary tools reached through `call`. A long script returns a cell id to continue \
         with wait_code."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "run_code: run synchronous JavaScript only, with no Node.js, filesystem or network globals. Every tool is `call(name, args)` with a registry \
             tool name (MCP tools: search_tool then use_tool); `complete(value)` ends it, \
             `log(text)` reports progress. It is a long call that occupies its own cycle — use \
             yield_time_ms and wait_code for work that may outlast it.",
        )
    }

    /// Code mode never runs beside another call: a script can perform any
    /// sequence of writes, and the dispatcher gives a non-parallel tool its
    /// own batch in the model's order (`wiki/architecture/code-mode-engine.md`
    /// §3.1).
    fn parallel_safe(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Execution(format!(
                "run_code cancelled{INTERRUPTED_NOTE}"
            )));
        }
        let yield_ms = args
            .yield_time_ms
            .unwrap_or(DEFAULT_YIELD_MS)
            .min(MAX_YIELD_MS);
        let max_output = args
            .max_output_bytes
            .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES)
            .clamp(1024, 4 * 1024 * 1024);
        let Some(registry) = self.registry.upgrade() else {
            return Err(ToolError::Execution(
                "the tool registry was dropped while this tool was alive".to_owned(),
            ));
        };
        let cell = self.cells.create(&self.scope, max_output);
        let (host_tx, host_rx) = mpsc::unbounded_channel();
        let (done_tx, done_rx) = oneshot::channel();
        // Progress crosses to the caller through this channel, never through
        // a clone of `out`: a clone held by the dispatcher would keep the
        // dispatcher's receiver open after `execute` returns and the turn
        // would wait for the script instead of receiving the cell id.
        let progress_tx = cell.progress_sender();
        let mut progress_rx = cell.subscribe_progress();
        // Captured before the script starts: a `yield_control()` that lands
        // before the select below is polled must still settle this call.
        let yield_epoch = cell.yield_epoch();
        // A cell owns a child token: stopping it cancels detached inner work
        // without cancelling the turn or another cell.
        let dispatch_cancel = ctx.cancel.child_token();
        let script_cancel = dispatch_cancel.clone();
        let script = args.script;
        let thread_cell = Arc::clone(&cell);
        let spawned = std::thread::Builder::new()
            .name(format!("code-mode-{}", cell.cell_id()))
            .spawn(move || {
                let outcome = engine::evaluate(&script, host_tx, thread_cell, script_cancel);
                let _ = done_tx.send(outcome);
            });
        if let Err(error) = spawned {
            cell.finish(ScriptOutcome::Failed(format!(
                "failed to start the script thread: {error}"
            )));
            return Err(ToolError::Execution(format!(
                "failed to start the script thread: {error}"
            )));
        }
        // A panic anywhere in the dispatcher (a tool's `execute_dyn` included)
        // must still settle the cell, or `wait_code` would wait on Running
        // forever.
        let dispatch_cell = Arc::clone(&cell);
        let base = DispatchContext {
            cwd: ctx.cwd.clone(),
            extra_roots: ctx.extra_roots.clone(),
            read_roots: ctx.read_roots.clone(),
            cancel: dispatch_cancel,
            call_id: ctx.call_id.clone(),
        };
        tokio::spawn(crate::builtin::inherit_agent_activity(async move {
            let dispatch = std::panic::AssertUnwindSafe(dispatch::drive_cell(
                Arc::clone(&dispatch_cell),
                host_rx,
                done_rx,
                registry,
                base,
                progress_tx,
            ));
            if dispatch.catch_unwind().await.is_err() {
                dispatch_cell.finish(ScriptOutcome::Failed(
                    "the code-mode dispatcher panicked".to_owned(),
                ));
            }
        }));

        // Stopping the turn by dropping this future must still stop the
        // script; the cancel branch below only runs when the future is
        // polled.
        let mut guard = TerminateOnDrop::new(Arc::clone(&cell));
        let settle = cell.wait_settled(yield_epoch);
        tokio::pin!(settle);
        let deadline = tokio::time::sleep(Duration::from_millis(yield_ms));
        tokio::pin!(deadline);
        let mut progress_open = true;
        let waited = loop {
            tokio::select! {
                biased;
                () = ctx.cancel.cancelled() => {
                    cell.terminate();
                    break None;
                }
                () = &mut settle => break Some(true),
                () = &mut deadline => break Some(false),
                item = progress_rx.recv(), if progress_open => match item {
                    Ok(message) => {
                        out.progress(message);
                    }
                    // Snapshots preserve each inner call's current/terminal
                    // state if the bounded live stream ever falls behind.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => progress_open = false,
                },
            }
        };
        // A ready cell may win the select before its final progress items.
        // Forward the already queued tail before closing the outer stream.
        drain_progress(&mut progress_rx, out);
        guard.disarm();
        match waited {
            None => Err(ToolError::Execution(format!(
                "run_code cancelled{INTERRUPTED_NOTE}"
            ))),
            Some(true) => Ok(result_for(&cell)),
            Some(false) => {
                cell.mark_yielded();
                Ok(result_for(&cell))
            }
        }
    }
}

fn drain_progress(progress: &mut broadcast::Receiver<String>, out: &ToolStream) {
    loop {
        match progress.try_recv() {
            Ok(message) => {
                out.progress(message);
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => return,
        }
    }
}

/// Terminates the script when a `run_code` or `wait_code` future is dropped
/// mid-wait (a canceled turn), unless the call returned normally.
struct TerminateOnDrop {
    cell: Arc<ScriptCell>,
    armed: bool,
}

impl TerminateOnDrop {
    fn new(cell: Arc<ScriptCell>) -> Self {
        Self { cell, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TerminateOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cell.terminate();
        }
    }
}

/// `wait_code` arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitCodeArgs {
    /// The cell id `run_code` returned.
    pub cell_id: String,
    /// How long to wait before answering again (default 30000 ms, at most
    /// 300000 ms).
    pub yield_time_ms: Option<u64>,
    /// Stop the script instead of waiting for it.
    pub terminate: Option<bool>,
}

/// Collects the result of a `run_code` cell, or stops it.
pub struct WaitCodeTool {
    cells: Arc<CodeCells>,
    scope: String,
}

impl WaitCodeTool {
    /// Binds the cross-turn cell table and the session whose cells it may
    /// reach.
    #[must_use]
    pub fn new(cells: Arc<CodeCells>, scope: String) -> Self {
        Self { cells, scope }
    }
}

impl Tool for WaitCodeTool {
    type Args = WaitCodeArgs;
    type Output = String;

    fn name(&self) -> &str {
        "wait_code"
    }

    fn description(&self) -> &str {
        "Wait for a run_code cell, or stop it. Pass the cell_id run_code returned; pass \
         terminate: true to stop a script that is still running. Returns the script's output \
         when it finished. Pending inner-tool images are emitted natively once in the next \
         run_code/wait_code result, including a running result."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "wait_code: continue waiting for a run_code cell (cell_id), or stop it with \
             terminate: true.",
        )
    }

    /// Collecting a cell can let its script finish its remaining writes, so
    /// it is never parallel with other calls either.
    fn parallel_safe(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let Some(cell) = self.cells.get(&self.scope, &args.cell_id) else {
            return Err(ToolError::InvalidArgs(format!(
                "unknown cell id {}: run_code returns the id of a cell that is still running",
                args.cell_id
            )));
        };
        if args.terminate.unwrap_or(false) {
            cell.terminate();
            return Ok(result_for(&cell));
        }
        let yield_ms = args
            .yield_time_ms
            .unwrap_or(DEFAULT_YIELD_MS)
            .min(MAX_YIELD_MS);
        // A cell yielded by `run_code` (or by `yield_control`) is `Yielded`
        // right now; waiting must be for the *next* yield, not this one.
        let yield_epoch = cell.yield_epoch();
        if !cell.phase().is_terminal() {
            out.progress(format!(
                "waiting for code-mode cell {} (up to {yield_ms} ms)",
                cell.cell_id()
            ));
        }
        let mut guard = TerminateOnDrop::new(Arc::clone(&cell));
        let mut progress = cell.subscribe_progress();
        let settle = tokio::time::timeout(
            Duration::from_millis(yield_ms),
            cell.wait_settled(yield_epoch),
        );
        tokio::pin!(settle);
        let waited = loop {
            tokio::select! {
                biased;
                () = ctx.cancel.cancelled() => break None,
                finished = &mut settle => break Some(finished.is_ok()),
                item = progress.recv() => {
                    if let Ok(message) = item {
                        out.progress(message);
                    }
                }
            }
        };
        drain_progress(&mut progress, out);
        if waited.is_none() {
            return Err(ToolError::Execution(format!(
                "wait_code cancelled{INTERRUPTED_NOTE}"
            )));
        }
        guard.disarm();
        if cell.phase() == CellPhase::Running {
            cell.mark_yielded();
        }
        Ok(result_for(&cell))
    }
}

/// Renders one cell snapshot as a tool result, with the journal and the
/// inner-call summaries in the UI-only details.
///
/// The summaries are what let a frontend name the real tools a script
/// called: the journal beside them holds only hashes, by design.
fn result_for(cell: &ScriptCell) -> ToolResult {
    let snapshot = cell.snapshot();
    let details = json!({
        "cell_id": snapshot.cell_id,
        "phase": snapshot.phase,
        "calls": snapshot.calls,
        "journal": snapshot.journal.entries(),
        "inner_calls": snapshot.inner_calls,
        "agent_states": cell.agent_states(),
    });
    let text = snapshot.render();
    let mut result = match snapshot.phase {
        CellPhase::Completed => ToolResult::text(text).with_details(details),
        CellPhase::Failed | CellPhase::Terminated => ToolResult::error(text).with_details(details),
        CellPhase::Running | CellPhase::Yielded => ToolResult::text(text).with_details(details),
    };
    result.content.extend(
        cell.take_images()
            .into_iter()
            .map(mycode_providers::message::ContentBlock::Image),
    );
    result
}

#[cfg(test)]
mod image_tests;
#[cfg(test)]
mod progress_tests;
#[cfg(test)]
mod regressions;
/// Argument type used by tests that need a serialized form.
#[cfg(test)]
mod tests;
