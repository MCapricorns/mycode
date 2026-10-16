//! `mycode-app` — the application core, with no frontend dependency.
//!
//! Everything a MYCode frontend needs to drive is here: sessions, model
//! turns, tools and their hosts, MCP servers, web search, provider
//! credentials, self-update, and the durable settings. A frontend owns none
//! of it. It starts a [`CoreBridge`], sends [`BridgeCommand`]s, awaits
//! [`BridgeReply`]s, and renders the [`BridgeEvent`] stream; the values in
//! those messages are defined in [`protocol`].
//!
//! [`CoreBridge`] owns a dedicated thread with a current-thread tokio runtime
//! hosting the session service, so a frontend never touches tokio types and
//! its own executor stays free. Replies ride tokio oneshot channels, whose
//! receivers are executor-agnostic futures. Model turns run as concurrent
//! runtime tasks streaming events back through a channel the frontend awaits;
//! configuration reads stay synchronous on the same thread.
//!
//! The command channel is unbounded on purpose: the worker loop must `await`
//! commands instead of blocking the runtime thread, or spawned tasks (chat
//! turns, catalog refreshes) would starve until the next command arrives.
//! `UnboundedSender::send` is synchronous, so the caller never touches async
//! machinery.
//!
//! Module map: `dispatch` runs the command loop, `state` holds the shared
//! core state, `ledger` reads and writes session ledgers, `turn` drives one
//! model turn, `tool_hosts` bridges the host-backed tools, `oauth` runs the
//! provider device flows, `settings_io` persists settings and secrets,
//! `projection` renders display entries, `search` backs the composer's
//! `@` mention, `mcp_tools` bridges MCP servers, `subagent` hosts the `agent`
//! tool, and `compaction`, `export`, `updates`, `web_client`, and
//! `mcp_client` round out the housekeeping and I/O seams.

#[cfg(test)]
mod bridge_tests;
mod compaction;
mod dispatch;
mod export;
mod ledger;
mod mcp_client;
mod mcp_tools;
mod oauth;
mod projection;
pub mod protocol;

pub use compaction::{
    display_summary_text, is_display_only_summary as is_compaction_summary, summary_body,
};
pub use export::IMPORT_NEEDS_MCP_CONFIRM;
mod search;
mod settings_io;
mod state;
mod subagent;
mod tool_hosts;
mod turn;
mod updates;
mod web_client;

pub use protocol::{
    ActiveConversation, AskPrompt, BranchId, CHAT_CANCELLED, ConversationEntry, EntryKind,
    HeadStamp, MAX_STREAMING_CHARS, OlderTranscript, SessionEventId, SessionId, SessionSummary,
    StreamingReply, tool_label,
};
pub use updates::apply::{parse_apply_update_args, run_apply_update_helper};
pub use updates::{PreparedUpdate, UpdateOffer};
pub use updates::{apply_and_restart, brief_error, cleanup_stale_stages, current_version};

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use mycode_config::{HomeLayout, UiState};
use mycode_providers::catalog::CatalogDocument;
use tokio::sync::oneshot;

// The event channel is unbounded on purpose: `send` never blocks, so a
// slow or stalled UI frame can never freeze the single-threaded core runtime
// mid-turn. `send` wakes the frontend task parked on `recv`. That task
// paints streaming text in short frames; it does not apply a whole queued
// turn before the next frame.

/// A request from the UI to the core thread.
#[derive(Debug)]
pub enum BridgeCommand {
    /// Refresh the sidebar session list.
    ListSessions,
    /// Create and open a fresh session.
    CreateSession,
    /// Recover and open one session's root conversation.
    OpenSession(SessionId),
    /// Commit one user message on the open branch.
    SendMessage {
        /// Target session.
        session: SessionId,
        /// Target branch.
        branch: BranchId,
        /// Head the UI observed.
        expected_head: HeadStamp,
        /// Message text; nonempty and bounded by the view-model.
        text: String,
    },
    /// Load the settings document with its revision and stored key ids.
    LoadSettings,
    /// Persist new settings under revision compare-and-swap.
    SaveSettings {
        /// The revision the editor loaded.
        expected_revision: mycode_config::AuthorityRevision,
        /// The complete replacement settings.
        settings: mycode_config::AppSettings,
    },
    /// Store or clear one provider API key in the secret store.
    SaveProviderKey {
        /// Provider identity from settings.
        provider_id: String,
        /// The key; empty clears the stored entry.
        api_key: String,
    },
    /// Start an OAuth device-flow sign-in for Copilot or Codex.
    StartOAuthSignIn {
        /// Catalog / settings provider id.
        provider_id: String,
        /// Model ids to bind to the provider once the sign-in succeeds.
        models: Vec<String>,
    },
    /// Run one model turn over stored history and stream the reply. The
    /// turn reads the branch's durable head itself, so a snapshot that is
    /// behind cannot fail it.
    ChatTurn {
        /// Target session.
        session: SessionId,
        /// Target branch.
        branch: BranchId,
        /// Provider identity from settings.
        provider_id: String,
        /// Model id offered by that provider.
        model: String,
        /// Reasoning effort for this session. `None` leaves the provider
        /// default. Global settings are not consulted, so one session's
        /// pick cannot fail the others.
        reasoning: Option<String>,
    },
    /// Summarize the open session now. The next turn sends the checkpoint.
    CompactSession {
        /// Target session.
        session: SessionId,
        /// Target branch.
        branch: BranchId,
        /// Head the UI observed.
        expected_head: HeadStamp,
        /// Provider identity from settings.
        provider_id: String,
        /// Model id that writes the summary.
        model: String,
    },
    /// Inject text into the running turn without cancelling it or its subagents.
    ///
    /// The core pushes into that session's steer inbox and replies at once.
    /// A missing inbox means the turn already ended; the desktop then queues
    /// the text so it is not lost.
    Steer {
        /// Session identity spelling.
        session_id: String,
        /// Composer text. Blank text is ignored by the inbox.
        text: String,
    },
    /// Abort the in-flight turn of one session (Escape in the chat).
    CancelChat {
        /// Session identity spelling.
        session_id: String,
    },
    /// Stop one running subagent without ending the parent turn.
    CancelSubagent {
        /// Session identity spelling.
        session_id: String,
        /// Provider call id of the `agent` tool.
        call_id: String,
    },
    /// List project files matching the composer's `@` fragment.
    SearchProjectFiles {
        /// Session whose bound project is searched.
        session_id: String,
        /// Case-insensitive substring filter; empty lists the first files.
        query: String,
    },
    /// List tools exposed by one enabled MCP server.
    McpListTools {
        /// The server row to probe. The whole row travels, not just its id,
        /// so the settings form can test a binding before it is saved.
        server: Box<mycode_config::McpServerSettings>,
    },
    /// Load one page of committed events older than `before`.
    ///
    /// `before` is the oldest event the UI already shows. The head must
    /// still match; a moved branch fails closed instead of splicing a page
    /// from a different snapshot.
    LoadOlder {
        /// Session identity spelling.
        session: SessionId,
        /// Branch identity spelling.
        branch: BranchId,
        /// Head the UI observed.
        expected_head: HeadStamp,
        /// Oldest event already on screen.
        before: String,
    },
    /// Rewind the branch to just before one user message (recall), with an
    /// optional edited text to prefill. Workspace files are not restored
    /// or deleted.
    RecallMessage {
        /// Session identity spelling.
        session: SessionId,
        /// Branch identity spelling.
        branch: BranchId,
        /// Head the UI observed; a stale head fails the rewind with the same
        /// moved-on error a stale `SendMessage` gets.
        expected_head: HeadStamp,
        /// Event to rewind to (the entry before the recalled message).
        to_event: String,
        /// Edited text to prefill for re-sending.
        edit: Option<String>,
    },
    /// Deletes one session's durable data (ledger, and any leftover file
    /// snapshots from older versions).
    DeleteSession {
        /// Session identity spelling.
        session_id: String,
    },
    /// Removes one directory from the remembered projects list.
    RemoveRecent {
        /// The project directory to forget.
        project: String,
    },
    /// Write one product-data export bundle to a user-chosen file.
    ExportData {
        /// Destination file chosen in a save dialog.
        path: PathBuf,
    },
    /// Apply one product-data export bundle from a user-chosen file.
    ImportData {
        /// Bundle file chosen in an open dialog.
        path: PathBuf,
        /// The user confirmed a bundle that changes MCP server commands.
        /// Without it such an import fails with [`IMPORT_NEEDS_MCP_CONFIRM`].
        confirm_mcp_commands: bool,
    },
    /// List discovered prompt resources for the session workspace.
    ListResources {
        /// Session identity spelling (selects the workspace directory).
        session_id: String,
    },
    /// Deliver the user's answers to the pending ask of one session.
    AskAnswer {
        /// Session identity spelling.
        session_id: String,
        /// One answer per asked question, in order; empty string skips.
        answers: Vec<String>,
    },
    /// Dismiss a pending ask without answering. The script sees a catchable
    /// tool error. This does not cancel the turn.
    AskDismiss {
        /// Session identity spelling.
        session_id: String,
    },
    /// Resolve the current provider catalog (cache, else bundled snapshot).
    GetCatalog,
    /// Re-download the cloud provider catalog.
    RefreshCatalog,
    /// List the models one OpenDesign key can use and install them in the
    /// catalog. An empty key uses the key stored for the configured
    /// OpenDesign provider.
    RefreshOpenDesignModels {
        /// Pasted key; empty uses the stored one.
        api_key: String,
    },
    /// Load the durable UI state document.
    LoadUiState,
    /// Persist the durable UI state document.
    SaveUiState {
        /// The replacement state.
        state: UiState,
    },
    /// Bind one project directory to a session for tool runs.
    SetProjectDir {
        /// Session identity spelling.
        session_id: String,
        /// Existing directory; `None` reverts to the shared scratch directory.
        path: Option<String>,
    },
    /// Query GitHub for a newer desktop release.
    CheckUpdate,
    /// Download and verify one update offer into a staging directory.
    DownloadUpdate {
        /// The offer to download.
        offer: UpdateOffer,
    },
}

/// Why the next prompt may miss the provider cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheDisruption {
    /// A compaction summary replaced the head of the prompt.
    Compaction,
    /// A steer was appended inside the running turn.
    Steer,
}

/// A streaming event from an active model turn.
#[derive(Debug, Clone)]
pub enum BridgeEvent {
    /// Incremental assistant text.
    ChatText {
        /// Session identity spelling.
        session_id: String,
        /// Text fragment.
        delta: String,
    },
    /// Incremental assistant reasoning.
    ChatThinking {
        /// Session identity spelling.
        session_id: String,
        /// Reasoning fragment.
        delta: String,
    },
    /// One intermediate assistant step was committed mid-turn.
    ///
    /// A turn that calls tools produces several assistant messages: each one
    /// requests tools, reads their results, and continues. Every step is
    /// committed as it arrives so the transcript and the ledger keep the
    /// model's actual order; only the closing step arrives as
    /// [`BridgeEvent::ChatDone`].
    AssistantStep {
        /// Session identity spelling.
        session_id: String,
        /// Committed assistant entry projection.
        entry: ConversationEntry,
    },
    /// The turn finished and its closing assistant message was committed.
    ChatDone {
        /// Session identity spelling.
        session_id: String,
        /// New branch head spelling.
        head: String,
        /// Committed assistant entry projection.
        entry: ConversationEntry,
    },
    /// In-progress token counts for the open turn. Not a ledger write.
    UsageSnapshot {
        /// Session identity spelling.
        session_id: String,
        /// Model id the turn is running on.
        model: String,
        /// Input tokens summed so far this turn (billing).
        input: u64,
        /// Latest prompt size, not the sum of tool rounds.
        context: u64,
        /// Cache-read tokens on that latest prompt.
        context_cache: u64,
        /// Output tokens summed so far this turn.
        output: u64,
        /// Prompt tokens served from the provider cache, when reported.
        cache: Option<u64>,
        /// Model requests (response cycles) the sums cover. A turn that
        /// calls tools sends one request per round, so `input` grows far
        /// past the window while `context` stays one prompt.
        requests: u32,
        /// Elapsed milliseconds since the turn started.
        elapsed_ms: u64,
    },
    /// The agent asked the user structured questions.
    AskRequested {
        /// Session identity spelling.
        session_id: String,
        /// Questions the user should answer, including multi-select.
        questions: Vec<crate::protocol::AskPrompt>,
    },
    /// A tool call started executing.
    ToolStarted {
        /// Session identity spelling.
        session_id: String,
        /// Provider-assigned call id.
        call_id: String,
        /// Tool name.
        name: String,
        /// Path, query, or command. Empty when the call has no target.
        target: String,
    },
    /// Incremental tool progress (including subagent steps).
    ToolProgress {
        /// Session identity spelling.
        session_id: String,
        /// Provider-assigned call id when known.
        call_id: String,
        /// Tool name when known; empty for a nested progress line.
        name: String,
        /// One-line progress.
        message: String,
    },
    /// A tool call finished; its result is committed to the ledger.
    ToolCompleted {
        /// Session identity spelling.
        session_id: String,
        /// Committed tool-result entry projection.
        entry: ConversationEntry,
    },
    /// The turn failed. Earlier steps in the same turn may already be committed.
    ChatFailed {
        /// Session identity spelling.
        session_id: String,
        /// Rendered failure for the banner. [`protocol::CHAT_CANCELLED`] is a
        /// quiet cancel: the desktop clears the turn and may send the queue.
        message: String,
        /// Ledger head after this turn's writes. Empty means the UI must
        /// leave `conversation.head` unchanged.
        head: String,
    },
    /// Steers that reached the turn after its last model request. They were
    /// never sent; the UI queues them as follow-ups.
    SteersReturned {
        /// Session identity spelling.
        session_id: String,
        /// Steer texts in arrival order.
        texts: Vec<String>,
    },
    /// A steer was written into the ledger while the turn kept running.
    SteerCommitted {
        /// Session identity spelling.
        session_id: String,
        /// Branch head after the steer message.
        head: String,
        /// Transcript row for the steered user message.
        entry: ConversationEntry,
    },
    /// The provider catalog changed after a cloud refresh.
    CatalogUpdated {
        /// Provider count in the refreshed catalog.
        providers: usize,
        /// Unix seconds of the successful fetch.
        fetched_at: u64,
    },
    /// A newer desktop release is available.
    UpdateAvailable {
        /// The resolved release offer.
        offer: UpdateOffer,
    },
    /// The automatic update check failed. The UI toasts `message`.
    UpdateCheckFailed {
        /// Short failure text. Not drawn as a full-width error in About.
        message: String,
    },
    /// A device-flow sign-in (Copilot or Codex) completed.
    CopilotSignedIn,
    /// A device-flow sign-in failed or expired.
    CopilotSignInFailed {
        /// Rendered failure for the sign-in panel.
        message: String,
    },
    /// The next provider request follows a rewrite of the cached prefix.
    ///
    /// Sent before that request's [`BridgeEvent::UsageSnapshot`], including
    /// when a mid-turn compaction defers its transcript card until the turn
    /// commits. The desktop compares the next prompt's cache hit with the
    /// one already on the meter.
    PromptCacheWatch {
        /// Session identity spelling.
        session_id: String,
        /// What rewrote the prefix.
        cause: CacheDisruption,
    },
    /// A compaction summary was written and should appear in the transcript.
    /// Manual `/compact` and automatic compaction both emit this. The text
    /// is the display copy; the model request still uses the checkpoint.
    SummaryShown {
        /// Session identity spelling.
        session_id: String,
        /// Transcript row for the summary.
        entry: ConversationEntry,
    },
    /// `/compact` finished. `ok` is false when the session could not be opened,
    /// a claimed summary was not written, or the provider could not run.
    /// `empty` and `covered` stay `ok`.
    CompactFinished {
        /// Session identity spelling.
        session_id: String,
        /// `compacted`, `covered`, `empty`, or a rendered failure.
        message: String,
        /// Whether the command completed without a provider error.
        ok: bool,
    },
}

/// Settings the UI can edit, plus documents reset while they were loaded.
#[derive(Debug)]
pub struct SettingsSnapshot {
    /// The settings document.
    pub settings: mycode_config::AppSettings,
    /// Revision to send back with the next save.
    pub revision: mycode_config::AuthorityRevision,
    /// Provider ids that already have a stored key.
    pub provider_keys: Vec<String>,
    /// MCP server ids that already have a stored key.
    pub mcp_keys: Vec<String>,
    /// Product documents backed up and replaced with defaults on this load.
    pub repairs: Vec<mycode_config::DocumentRepair>,
}

/// A reply from the core thread, already projected for the view-model.
#[derive(Debug)]
pub enum BridgeReply {
    /// Session list result.
    Sessions(Result<Vec<SessionSummary>, String>),
    /// Create result (session ids).
    Created(Result<SessionSummary, String>),
    /// Open result (conversation projection).
    Conversation(Result<ActiveConversation, String>),
    /// One older page of the open branch.
    Older(Result<OlderTranscript, String>),
    /// Send result (new head plus committed entry). The session id rides
    /// the reply even on failure, so the workspace can drop a send that
    /// raced a session switch instead of applying it to the conversation
    /// then open.
    Sent {
        /// Session the message targeted.
        session: String,
        /// New head plus committed entry, or why the send failed.
        outcome: Result<(String, ConversationEntry), String>,
    },
    /// Settings load result.
    Settings(Result<SettingsSnapshot, String>),
    /// Settings save result: the new revision.
    SettingsSaved(Result<mycode_config::AuthorityRevision, String>),
    /// Provider key save result: refreshed key-id lists (providers, MCP).
    ProviderKeySaved(Result<(Vec<String>, Vec<String>), String>),
    /// Chat turn acceptance; streaming continues over the event channel.
    ChatStarted(Result<(), String>),
    /// Chat cancel acceptance. The turn ends with [`BridgeEvent::ChatFailed`]
    /// whose message is [`protocol::CHAT_CANCELLED`] and whose head is the
    /// ledger head after the partial was written.
    ChatCancelled(Result<(), String>),
    /// Steer acceptance. `Err` means no live inbox (the turn already ended).
    Steered(Result<(), String>),
    /// One subagent cancel was delivered. The parent turn keeps running.
    SubagentCancelled(Result<(), String>),
    /// File matches for the composer's `@` mention.
    ProjectFiles(Result<Vec<String>, String>),
    /// MCP tools listing for one server. The server id rides the reply even
    /// on failure, so the settings page can attribute the error to its row.
    McpTools {
        /// Server identity the probe ran against.
        server_id: String,
        /// Listed tool names, or why the probe failed.
        outcome: Result<Vec<String>, String>,
    },
    /// Recall result: truncated conversation plus edited text. Workspace
    /// files are unchanged.
    Recalled(Result<(Box<ActiveConversation>, Option<String>), String>),
    /// Delete outcome.
    SessionDeleted(Result<(), String>),
    /// Export outcome.
    Exported(Result<crate::export::ExportSummary, String>),
    /// Import outcome.
    Imported(Result<crate::export::ImportSummary, String>),
    /// Resource list: (name, absolute path) pairs.
    Resources(Result<Vec<(String, String)>, String>),
    /// The user's answers were delivered to the waiting tool.
    AskAnswered(Result<(), String>),
    /// Provider catalog snapshot with its freshness metadata.
    Catalog(Result<CatalogInfo, String>),
    /// The catalog with the live OpenDesign listing installed, and how many
    /// chat models the key can use.
    OpenDesignModels(Result<(CatalogInfo, usize), String>),
    /// Durable UI state load result, plus documents reset earlier in startup.
    UiState(Result<(UiState, Vec<mycode_config::DocumentRepair>), String>),
    /// UI state persist result.
    UiStateSaved(Result<(), String>),
    /// Project directory bind result.
    ProjectSet(Result<(), String>),
    /// Update check result; `Ok(None)` means the app is current.
    UpdateChecked(Result<Option<UpdateOffer>, String>),
    /// Download-and-verify result.
    UpdateDownloaded(Result<PreparedUpdate, String>),
    /// The device flow started; the user code is on screen.
    CopilotSignInStarted(Result<CopilotSignInInfo, String>),
}

/// What the user needs to complete a device-flow sign-in.
#[derive(Clone, Debug)]
pub struct CopilotSignInInfo {
    /// Code the user types at the verification page.
    pub user_code: String,
    /// Verification page opened in the browser.
    pub verification_uri: String,
}

/// The resolved provider catalog shared with the UI.
#[derive(Clone, Debug)]
pub struct CatalogInfo {
    /// The catalog document.
    pub document: Arc<CatalogDocument>,
    /// Unix seconds of the successful cloud fetch; 0 for the baseline.
    pub fetched_at: u64,
}

/// Unbounded sender for [`BridgeEvent`]s.
///
/// `send` returns immediately and wakes the task parked on the paired
/// [`BridgeEventRx`]. A closed receiver is ignored; the core keeps running
/// after the UI is gone.
pub(crate) type BridgeEventTx = tokio::sync::mpsc::UnboundedSender<BridgeEvent>;

/// Receiving end of the bridge event stream.
///
/// Waiting on the receiver parks until the core sends or every sender is
/// dropped. Events already queued can be drained in the same wake, so one
/// wake applies a whole burst.
pub type BridgeEventRx = tokio::sync::mpsc::UnboundedReceiver<BridgeEvent>;

/// Handle to the core thread.
pub struct CoreBridge {
    command_tx: Option<tokio::sync::mpsc::UnboundedSender<WithReply>>,
    worker: Option<JoinHandle<()>>,
}

impl CoreBridge {
    /// Starts the core thread over one owned home.
    ///
    /// Returns the bridge handle together with the receiving end of the
    /// streaming event channel. Sending wakes the receiver and never waits
    /// on it.
    #[must_use]
    pub fn start(home: HomeLayout) -> (Self, BridgeEventRx) {
        let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let worker = std::thread::Builder::new()
            .name("mycode-core".into())
            .spawn(move || dispatch::run_core(home, command_rx, event_tx))
            .expect("core bridge thread");
        (
            Self {
                command_tx: Some(command_tx),
                worker: Some(worker),
            },
            event_rx,
        )
    }

    /// Enqueues one command immediately and returns its reply future.
    ///
    /// Dropping the returned future cancels only the wait, not the accepted
    /// durable core effect. The future holds no command sender, so keeping an
    /// unpolled reply cannot keep [`CoreBridge::shutdown`] waiting.
    /// A stopped bridge returns the error variant for the original command.
    pub fn request(
        &self,
        command: BridgeCommand,
    ) -> impl Future<Output = BridgeReply> + Send + 'static {
        let (reply_tx, reply_rx) = oneshot::channel();
        let failure = dispatch::error_reply(
            &command,
            if self.command_tx.is_some() {
                "core thread stopped"
            } else {
                "core bridge stopped"
            },
        );
        let queued = self
            .command_tx
            .as_ref()
            .is_some_and(|sender| sender.send(command.with_reply(reply_tx)).is_ok());
        async move {
            if !queued {
                return failure;
            }
            reply_rx.await.unwrap_or(failure)
        }
    }

    /// Stops the core thread and waits for it to finish.
    pub fn shutdown(&mut self) {
        // Dropping the sender closes the command loop; SessionService
        // shutdown runs inside the thread before it exits.
        self.command_tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl BridgeCommand {
    fn with_reply(self, reply: oneshot::Sender<BridgeReply>) -> WithReply {
        WithReply {
            command: self,
            reply,
        }
    }
}

struct WithReply {
    command: BridgeCommand,
    reply: oneshot::Sender<BridgeReply>,
}

/// Returns the wall-clock duration since the Unix epoch, saturating at zero
/// when the system clock predates the epoch.
pub use mycode_core::unix_now;
