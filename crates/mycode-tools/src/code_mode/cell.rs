//! Cell table and per-cell state for code-mode scripts.
//!
//! A cell outlives the `run_code` call that started it and is shared through
//! [`crate::CodeCells`] on the application's core state, so `wait_code` can
//! keep waiting for it across turns. See
//! `wiki/architecture/code-mode-engine.md` §6.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;

use super::journal::{ScriptEventKind, ScriptJournal};
use super::progress::{CodeModeProgress, InnerCallPhase};

/// The journal entry type, as seen from the cell module's vocabulary.
pub use super::journal::JournalEntry as ScriptEvent;

/// Default `yield_time_ms` when the model does not pass one.
pub const DEFAULT_YIELD_MS: u64 = 30_000;
/// Upper bound on `yield_time_ms`; a single `run_code` never ties up its
/// dispatch cycle longer than this.
pub const MAX_YIELD_MS: u64 = 300_000;
/// Default cap on logged/completed output, in bytes.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 32 * 1024;
/// Script-side `call()` budget, which stops `while true { call(…) }`.
pub const MAX_HOST_CALLS: u64 = 256;

/// How many finished cells stay addressable by `wait_code`.
const MAX_RETAINED_CELLS: usize = 32;

/// Lifecycle of one script cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellPhase {
    /// The script thread is evaluating.
    Running,
    /// The script asked for control back (`yield_control`) or the caller's
    /// `yield_time_ms` elapsed; it keeps evaluating.
    Yielded,
    /// The script finished normally.
    Completed,
    /// The script failed (a Rhai error, an exhausted budget, a dead host).
    Failed,
    /// The cell was terminated (user cancel, `terminate: true`).
    Terminated,
}

/// The cell phase under the design document's alternate name.
pub type CodeCellState = CellPhase;

impl CellPhase {
    /// Whether the cell will not change again.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Terminated)
    }
}

/// How a script thread finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptOutcome {
    /// The script ran to completion (including an explicit `complete(…)`).
    Completed,
    /// The script stopped with an error.
    Failed(String),
    /// Evaluation was stopped on purpose.
    Terminated,
}

/// One inner `call(name, args)` a script made, in UI terms.
///
/// The journal records only hashes (it is a determinism record), so it
/// cannot tell the UI what a script actually did. This summary is the
/// UI-facing counterpart: the real tool name, its one-line target, and
/// whether it failed — enough to render each inner call with the
/// dedicated renderer its tool name deserves instead of one generic
/// `run_code` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InnerCallSummary {
    /// Journal sequence of the call.
    pub seq: u64,
    /// Exact ToolCtx id used to dispatch this call. Absent in older records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// Current or terminal call state. Absent in older records, which only
    /// stored summaries after completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<InnerCallPhase>,
    /// Registry tool name the script called.
    pub name: String,
    /// One-line target (`tool_target` of the call's arguments).
    pub target: String,
    /// Whether the call failed or was refused.
    pub is_error: bool,
    /// The call's UI-only diff (edits and writes), when it made one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

impl InnerCallSummary {
    fn progress_event(&self) -> Option<CodeModeProgress> {
        let call_id = self.call_id.clone()?;
        Some(match self.phase? {
            InnerCallPhase::Running => CodeModeProgress::Started {
                call_id,
                name: self.name.clone(),
                seq: self.seq,
                target: self.target.clone(),
            },
            phase => CodeModeProgress::Completed {
                call_id,
                name: self.name.clone(),
                phase,
                is_error: self.is_error,
            },
        })
    }
}

struct CellInner {
    phase: CellPhase,
    output: String,
    truncated: bool,
    journal: ScriptJournal,
    inner_calls: Vec<InnerCallSummary>,
    next_seq: u64,
    calls: u64,
    operations: u64,
    /// How many times the script (or a caller's elapsed budget) handed
    /// control back. A waiter compares this against the value it started
    /// from, so a stale `Yielded` phase does not wake the next `wait_code`
    /// immediately.
    yield_epoch: u64,
}

impl CellInner {
    fn push_output(&mut self, text: &str, cap: usize) {
        if self.truncated {
            return;
        }
        if !self.output.is_empty() {
            if self.output.len() == cap {
                self.truncated = true;
                return;
            }
            self.output.push('\n');
        }
        let mut end = text.len().min(cap.saturating_sub(self.output.len()));
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.output.push_str(&text[..end]);
        self.truncated = end < text.len();
    }
}

/// One code-mode script execution.
///
/// Written by the script thread (output, `complete` value), by the host
/// dispatcher (journal, operation count), and by the tool that started it
/// (finish, terminate). Every field is behind one lock so a snapshot is
/// consistent.
pub struct ScriptCell {
    cell_id: String,
    /// The session the script came from. `wait_code` and `run_code` are
    /// resolved through one process-wide table, so a lookup only matches a
    /// cell created by the same session.
    scope: String,
    /// Creation ordinal; eviction keeps the oldest finished cells first.
    created: u64,
    max_output_bytes: usize,
    inner: RwLock<CellInner>,
    notify: Notify,
    /// Bounded live progress. Each run/wait caller subscribes without
    /// keeping a ToolStream alive after that call returns. Snapshots retain
    /// inner-call ids and terminal states even when no caller is listening.
    progress: broadcast::Sender<String>,
    terminate: CancellationToken,
}

impl ScriptCell {
    fn new(cell_id: String, scope: String, created: u64, max_output_bytes: usize) -> Self {
        Self {
            cell_id,
            scope,
            created,
            max_output_bytes: max_output_bytes.max(1),
            inner: RwLock::new(CellInner {
                phase: CellPhase::Running,
                output: String::new(),
                truncated: false,
                journal: ScriptJournal::new(),
                inner_calls: Vec::new(),
                next_seq: 0,
                calls: 0,
                operations: 0,
                yield_epoch: 0,
            }),
            notify: Notify::new(),
            progress: broadcast::channel(256).0,
            terminate: CancellationToken::new(),
        }
    }

    /// The session this cell belongs to.
    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// The id the model uses with `wait_code`.
    #[must_use]
    pub fn cell_id(&self) -> &str {
        &self.cell_id
    }

    /// Current phase.
    #[must_use]
    pub fn phase(&self) -> CellPhase {
        self.inner.read().phase
    }

    /// The output cap this cell was created with.
    #[must_use]
    pub fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    /// Reserves the next host-step sequence number.
    pub fn next_seq(&self) -> u64 {
        let mut inner = self.inner.write();
        let seq = inner.next_seq;
        inner.next_seq += 1;
        seq
    }

    /// Counts one script-side `call` and reports whether it is within budget.
    pub fn begin_call(&self) -> bool {
        let mut inner = self.inner.write();
        inner.calls += 1;
        inner.calls <= MAX_HOST_CALLS
    }

    /// Calls used so far.
    #[must_use]
    pub fn calls(&self) -> u64 {
        self.inner.read().calls
    }

    /// Records the operation counter the engine reported.
    pub fn set_operations(&self, operations: u64) {
        self.inner.write().operations = operations;
    }

    /// Operations executed so far.
    #[must_use]
    pub fn operations(&self) -> u64 {
        self.inner.read().operations
    }

    /// Appends text to the model-visible output, truncating at the cap.
    pub fn push_output(&self, text: &str) {
        self.inner.write().push_output(text, self.max_output_bytes);
    }

    /// Appends one journalled step under the sequence the script reserved.
    pub fn record(
        &self,
        seq: u64,
        kind: ScriptEventKind,
        args: &Value,
        result: Option<&Value>,
    ) -> super::journal::JournalEntry {
        self.inner.write().journal.record(seq, kind, args, result)
    }

    /// A copy of the journal so far.
    #[must_use]
    pub fn journal(&self) -> ScriptJournal {
        self.inner.read().journal.clone()
    }

    /// Records or updates one inner call without changing its script order.
    pub fn record_inner_call(&self, summary: InnerCallSummary) {
        let mut inner = self.inner.write();
        if let Some(existing) = inner
            .inner_calls
            .iter_mut()
            .find(|call| call.seq == summary.seq)
        {
            *existing = summary;
        } else {
            inner.inner_calls.push(summary);
        }
    }

    /// Publishes the lifecycle transition under the same lock as its
    /// snapshot. A terminal transition wins once, including cell cancellation.
    pub(crate) fn publish_inner_call(&self, summary: InnerCallSummary) {
        let mut inner = self.inner.write();
        if let Some(existing) = inner
            .inner_calls
            .iter_mut()
            .find(|call| call.seq == summary.seq)
        {
            if existing
                .phase
                .is_some_and(|phase| phase != InnerCallPhase::Running)
            {
                return;
            }
            *existing = summary.clone();
        } else {
            inner.inner_calls.push(summary.clone());
        }
        if let Some(event) = summary.progress_event() {
            let _ = self.progress.send(event.encode());
        }
    }

    pub(crate) fn progress_sender(&self) -> broadcast::Sender<String> {
        self.progress.clone()
    }

    pub(crate) fn subscribe_progress(&self) -> broadcast::Receiver<String> {
        self.progress.subscribe()
    }

    /// The inner calls so far, in the order the script made them.
    #[must_use]
    pub fn inner_calls(&self) -> Vec<InnerCallSummary> {
        self.inner.read().inner_calls.clone()
    }

    /// Marks the cell yielded: the script asked for control back, or the
    /// caller's budget elapsed.
    pub fn mark_yielded(&self) {
        {
            let mut inner = self.inner.write();
            if inner.phase.is_terminal() {
                return;
            }
            inner.yield_epoch += 1;
            if inner.phase == CellPhase::Running {
                inner.phase = CellPhase::Yielded;
            }
        }
        self.notify.notify_waiters();
    }

    /// The current yield generation; see [`ScriptCell::wait_settled`].
    #[must_use]
    pub fn yield_epoch(&self) -> u64 {
        self.inner.read().yield_epoch
    }

    /// Asks the script to stop.
    pub fn terminate(&self) {
        {
            let mut inner = self.inner.write();
            if inner.phase.is_terminal() {
                return;
            }
            inner.phase = CellPhase::Terminated;
            for call in &mut inner.inner_calls {
                if call.phase == Some(InnerCallPhase::Running) {
                    call.phase = Some(InnerCallPhase::Cancelled);
                    call.is_error = true;
                    if let Some(event) = call.progress_event() {
                        let _ = self.progress.send(event.encode());
                    }
                }
            }
        }
        self.terminate.cancel();
        self.notify.notify_waiters();
    }

    /// Whether termination was requested.
    #[must_use]
    pub fn terminated(&self) -> bool {
        self.terminate.is_cancelled()
    }

    /// The token the dispatcher races against while a script runs.
    #[must_use]
    pub fn terminate_token(&self) -> CancellationToken {
        self.terminate.clone()
    }

    /// Records the script thread's outcome. A terminal phase set earlier
    /// (termination, or another observer) wins.
    pub fn finish(&self, outcome: ScriptOutcome) {
        {
            let mut inner = self.inner.write();
            if inner.phase.is_terminal() {
                return;
            }
            inner.phase = match outcome {
                ScriptOutcome::Completed => CellPhase::Completed,
                ScriptOutcome::Terminated => CellPhase::Terminated,
                ScriptOutcome::Failed(_) => CellPhase::Failed,
            };
            if let ScriptOutcome::Failed(error) = &outcome {
                inner.push_output(&format!("error: {error}"), self.max_output_bytes);
            }
        }
        self.notify.notify_waiters();
    }

    /// Waits until the cell reaches a terminal phase or yields control back
    /// *again*, relative to the yield generation `since`.
    ///
    /// A caller that has already seen the current `Yielded` phase passes the
    /// [`ScriptCell::yield_epoch`] it saw, so an earlier yield does not make
    /// the wait return at once; `run_code` passes `0` for a fresh cell.
    pub async fn wait_settled(&self, since: u64) {
        loop {
            // Register the waiter before re-reading the state: a yield
            // between the read and the registration would otherwise be lost
            // (`notify_waiters` leaves no permit behind).
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let inner = self.inner.read();
                if inner.phase.is_terminal() || inner.yield_epoch != since {
                    return;
                }
            }
            notified.await;
        }
    }

    /// A consistent copy of everything a caller reports to the model.
    #[must_use]
    pub fn snapshot(&self) -> CellSnapshot {
        let inner = self.inner.read();
        let mut output = inner.output.clone();
        if inner.truncated {
            output.push_str(&format!(
                "\n(output truncated at {} bytes)",
                self.max_output_bytes
            ));
        }
        CellSnapshot {
            cell_id: self.cell_id.clone(),
            phase: inner.phase,
            output,
            calls: inner.calls,
            operations: inner.operations,
            journal: inner.journal.clone(),
            inner_calls: inner.inner_calls.clone(),
        }
    }
}

/// One consistent read of a cell.
#[derive(Debug, Clone)]
pub struct CellSnapshot {
    /// Cell identity.
    pub cell_id: String,
    /// Current phase.
    pub phase: CellPhase,
    /// Output so far, already capped and annotated.
    pub output: String,
    /// `call()` count.
    pub calls: u64,
    /// Rhai operations executed, as last reported by the engine.
    pub operations: u64,
    /// Journal so far.
    pub journal: ScriptJournal,
    /// UI summaries of the inner calls so far.
    pub inner_calls: Vec<InnerCallSummary>,
}

impl CellSnapshot {
    /// The text a tool result should carry.
    #[must_use]
    pub fn render(&self) -> String {
        let output = if self.output.is_empty() {
            "(no output)".to_owned()
        } else {
            self.output.clone()
        };
        match self.phase {
            CellPhase::Completed => output,
            CellPhase::Failed => output,
            CellPhase::Terminated => format!("{output}\n(terminated)"),
            CellPhase::Running | CellPhase::Yielded => format!(
                "Script running with cell ID {} ({} calls so far). Call wait_code with this \
                 id to keep waiting, or wait_code with terminate: true to stop it.",
                self.cell_id, self.calls
            ),
        }
    }
}

/// Cross-turn table of script cells.
///
/// Owned by the application's core state; the tools in this crate receive an
/// `Arc` at construction. Finished cells are retained (bounded) so a
/// `wait_code` that arrives a turn later still finds the result.
#[derive(Default)]
pub struct CodeCells {
    cells: RwLock<HashMap<String, Arc<ScriptCell>>>,
    next: AtomicU64,
}

impl CodeCells {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates and registers a cell for `scope` (the session id).
    ///
    /// The table is process-wide, so the scope is part of the lookup key:
    /// another session's model cannot enumerate or reach this cell even if
    /// it guesses `cell-0`.
    pub fn create(&self, scope: &str, max_output_bytes: usize) -> Arc<ScriptCell> {
        let index = self.next.fetch_add(1, Ordering::Relaxed);
        let cell = Arc::new(ScriptCell::new(
            format!("cell-{index}"),
            scope.to_owned(),
            index,
            max_output_bytes,
        ));
        let mut cells = self.cells.write();
        if cells.len() >= MAX_RETAINED_CELLS {
            // Oldest finished cell first (by creation ordinal, not by the
            // lexicographic `cell-N` spelling). When nothing has finished,
            // the oldest cell is terminated and dropped so a model that
            // never collects its cells cannot grow the table — or the
            // script threads behind it — without bound.
            let mut finished: Vec<(u64, String, bool)> = cells
                .iter()
                .map(|(id, cell)| (cell.created, id.clone(), cell.phase().is_terminal()))
                .collect();
            finished.sort_by_key(|(created, _, terminal)| (!terminal, *created));
            for (_, id, terminal) in finished {
                if cells.len() < MAX_RETAINED_CELLS {
                    break;
                }
                if let Some(evicted) = cells.remove(&id)
                    && !terminal
                {
                    evicted.terminate();
                }
            }
        }
        cells.insert(cell.cell_id.clone(), Arc::clone(&cell));
        cell
    }

    /// Looks one cell up for `scope`.
    #[must_use]
    pub fn get(&self, scope: &str, cell_id: &str) -> Option<Arc<ScriptCell>> {
        self.cells
            .read()
            .get(cell_id)
            .filter(|cell| cell.scope == scope)
            .cloned()
    }

    /// Terminates one cell of `scope`; `false` when the id is unknown.
    pub fn terminate(&self, scope: &str, cell_id: &str) -> bool {
        match self.get(scope, cell_id) {
            Some(cell) => {
                cell.terminate();
                true
            }
            None => false,
        }
    }

    /// Number of live and retained cells.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.read().len()
    }

    /// Whether no cell was ever created.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{CellPhase, CodeCells, ScriptOutcome};

    #[test]
    fn output_is_capped_and_annotated() {
        let cell = CodeCells::new().create("s", 32);
        cell.push_output(&"a".repeat(40));
        let snapshot = cell.snapshot();
        assert!(
            snapshot.output.starts_with("aaaaaaaaaa"),
            "{}",
            snapshot.output
        );
        assert!(snapshot.output.contains("output truncated at 32 bytes"));
        assert!(snapshot.output.len() < 40 + 64);
    }

    #[test]
    fn a_cell_reaches_a_terminal_phase_once() {
        let cells = CodeCells::new();
        let cell = cells.create("s", 1024);
        assert_eq!(cell.phase(), CellPhase::Running);
        cell.finish(ScriptOutcome::Failed("boom".to_owned()));
        assert_eq!(cell.phase(), CellPhase::Failed);
        assert!(cell.snapshot().output.contains("error: boom"));
        // A later termination does not rewrite a finished cell.
        cell.terminate();
        assert_eq!(cell.phase(), CellPhase::Failed);
        assert!(!cells.terminate("s", "missing"));
    }

    #[test]
    fn the_call_counter_budget_stops_at_the_limit() {
        let cell = CodeCells::new().create("s", 1024);
        for _ in 0..super::MAX_HOST_CALLS {
            assert!(cell.begin_call());
        }
        assert!(!cell.begin_call());
    }

    #[test]
    fn journal_sequences_are_monotonic() {
        let cell = CodeCells::new().create("s", 1024);
        assert_eq!(cell.next_seq(), 0);
        cell.record(
            0,
            super::ScriptEventKind::Call,
            &json!({"name": "read"}),
            Some(&json!({"content": []})),
        );
        assert_eq!(cell.next_seq(), 1);
        assert_eq!(cell.journal().entries().len(), 1);
    }

    /// A cancellation that ends the script is reported as terminated, not as
    /// a failure.
    #[test]
    fn a_terminated_outcome_stays_terminated() {
        let cell = CodeCells::new().create("s", 64);
        cell.finish(ScriptOutcome::Terminated);
        assert_eq!(cell.phase(), CellPhase::Terminated);
    }

    /// The table is process-wide, so a session can only reach its own cells.
    #[test]
    fn a_cell_is_only_visible_to_its_own_scope() {
        let cells = CodeCells::new();
        let cell = cells.create("alice", 1024);
        assert_eq!(cell.cell_id(), "cell-0");
        assert!(cells.get("alice", "cell-0").is_some());
        assert!(cells.get("bob", "cell-0").is_none());
        assert!(!cells.terminate("bob", "cell-0"));
        assert_eq!(cell.phase(), CellPhase::Running);
        assert!(cells.terminate("alice", "cell-0"));
        assert_eq!(cell.phase(), CellPhase::Terminated);
    }

    /// Eviction takes finished cells in creation order (`cell-2` before
    /// `cell-10`), never by the lexicographic id.
    #[test]
    fn eviction_takes_the_oldest_finished_cell_in_creation_order() {
        let cells = CodeCells::new();
        let first = cells.create("s", 16);
        first.finish(ScriptOutcome::Completed);
        for _ in 1..super::MAX_RETAINED_CELLS {
            cells.create("s", 16);
        }
        assert!(cells.get("s", "cell-0").is_some());
        let last = cells.create("s", 16);
        assert!(
            cells.get("s", "cell-0").is_none(),
            "the oldest finished cell goes first"
        );
        assert!(cells.get("s", last.cell_id()).is_some());
    }

    #[test]
    fn eviction_preserves_older_running_work_when_a_finished_cell_exists() {
        let cells = CodeCells::new();
        let oldest = cells.create("s", 16);
        let finished = cells.create("s", 16);
        finished.finish(ScriptOutcome::Completed);
        for _ in 2..super::MAX_RETAINED_CELLS {
            cells.create("s", 16);
        }
        cells.create("s", 16);
        assert!(cells.get("s", oldest.cell_id()).is_some());
        assert!(!oldest.terminated());
        assert!(cells.get("s", finished.cell_id()).is_none());
    }

    /// With nothing finished the oldest running cell is stopped, so a model
    /// that never collects its cells cannot grow the table forever.
    #[test]
    fn eviction_stops_the_oldest_running_cell_when_the_table_is_full() {
        let cells = CodeCells::new();
        let oldest = cells.create("s", 16);
        for _ in 1..super::MAX_RETAINED_CELLS {
            cells.create("s", 16);
        }
        assert_eq!(cells.len(), super::MAX_RETAINED_CELLS);
        cells.create("s", 16);
        assert_eq!(cells.len(), super::MAX_RETAINED_CELLS);
        assert!(cells.get("s", oldest.cell_id()).is_none());
        assert_eq!(oldest.phase(), CellPhase::Terminated);
    }
}
