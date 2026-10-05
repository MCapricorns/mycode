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
//! `@` mention, `mcp_tools` bridges MCP servers, `subagent` hosts the `task`
//! tool, and `compaction`, `export`, `updates`, `web_client`, and
//! `mcp_client` round out the housekeeping and I/O seams.

mod compaction;
mod dispatch;
mod export;
mod ledger;
mod mcp_client;
mod mcp_tools;
mod oauth;
mod projection;
pub mod protocol;
mod rollback;
mod search;
mod settings_io;
mod state;
mod subagent;
mod tool_hosts;
mod turn;
mod updates;
mod web_client;

pub use protocol::{
    ActiveConversation, BranchId, CHAT_CANCELLED, ConversationEntry, EntryKind, HeadStamp,
    MAX_STREAMING_CHARS, SessionEventId, SessionId, SessionSummary, StreamingReply,
};
pub use updates::{PreparedUpdate, UpdateOffer};
pub use updates::{apply_and_restart, brief_error, cleanup_stale_stages, current_version};

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use mycode_config::{HomeLayout, UiState};
use mycode_providers::catalog::CatalogDocument;
use tokio::sync::oneshot;

// The event channel is unbounded on purpose: `try_send` never blocks, so a
// slow or stalled UI frame can never freeze the single-threaded core runtime
// mid-turn. `try_send` wakes the frontend task parked on `recv`. That task
// applies the event and drains whatever else is already queued, so one wake
// folds a whole burst.

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
    /// Start an OAuth device-flow sign-in for Copilot, xAI, or Codex.
    StartOAuthSignIn {
        /// Catalog / settings provider id.
        provider_id: String,
        /// Model ids to bind to the provider once the sign-in succeeds.
        models: Vec<String>,
    },
    /// Run one model turn over stored history and stream the reply.
    ChatTurn {
        /// Target session.
        session: SessionId,
        /// Target branch.
        branch: BranchId,
        /// Head observed after the user message commit.
        expected_head: HeadStamp,
        /// Provider identity from settings.
        provider_id: String,
        /// Model id offered by that provider.
        model: String,
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
        /// Provider call id of the `task` tool.
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
    /// Roll every snapshotted file of one session back to its earliest state.
    RollbackWorkspace {
        /// Session identity spelling.
        session_id: String,
    },
    /// Rewind the branch to just before one user message (recall), with an
    /// optional edited text to re-send.
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
    /// Deletes one session's durable data (ledger and checkpoints).
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
    /// Resolve the current provider catalog (cache, else bundled snapshot).
    GetCatalog,
    /// Re-download the cloud provider catalog.
    RefreshCatalog,
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
    /// Something the user should read that does not end the turn.
    ///
    /// History compaction and other background housekeeping report through
    /// here; the turn keeps running.
    Notice {
        /// Session identity spelling.
        session_id: String,
        /// One-line message.
        message: String,
    },
    /// In-progress token counts for the open turn. Not a ledger write.
    UsageSnapshot {
        /// Session identity spelling.
        session_id: String,
        /// Model id the turn is running on.
        model: String,
        /// Input tokens summed so far this turn.
        input: u64,
        /// Output tokens summed so far this turn.
        output: u64,
        /// Prompt tokens served from the provider cache, when reported.
        cache: Option<u64>,
        /// Elapsed milliseconds since the turn started.
        elapsed_ms: u64,
    },
    /// A durable usage record was committed.
    UsageRecorded {
        /// Session identity spelling.
        session_id: String,
        /// Provider identity.
        provider: String,
        /// Model id.
        model: String,
        /// Input tokens.
        input: u64,
        /// Output tokens.
        output: u64,
        /// Prompt tokens served from the provider cache, when reported.
        cache: Option<u64>,
        /// Wall-clock turn duration in milliseconds.
        elapsed_ms: u64,
        /// Committed usage entry projection.
        entry: ConversationEntry,
    },
    /// The agent asked the user structured questions.
    AskRequested {
        /// Session identity spelling.
        session_id: String,
        /// (question, choices, optional) rows.
        questions: Vec<(String, Vec<String>, bool)>,
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
    /// Incremental tool progress (including nested subagent steps).
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
    /// The turn failed; nothing was committed.
    ChatFailed {
        /// Session identity spelling.
        session_id: String,
        /// Rendered failure for the banner.
        message: String,
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
    /// The Copilot device-flow sign-in completed; the provider is ready.
    CopilotSignedIn,
    /// The Copilot device-flow sign-in failed or expired.
    CopilotSignInFailed {
        /// Rendered failure for the sign-in panel.
        message: String,
    },
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
    /// Send result (new head plus committed entry).
    Sent(Result<(String, ConversationEntry), String>),
    /// Settings load result: document, revision, provider ids with keys.
    Settings(
        Result<
            (
                mycode_config::AppSettings,
                mycode_config::AuthorityRevision,
                Vec<String>,
                Vec<String>,
            ),
            String,
        >,
    ),
    /// Settings save result: the new revision.
    SettingsSaved(Result<mycode_config::AuthorityRevision, String>),
    /// Provider key save result: refreshed key-id lists (providers, MCP).
    ProviderKeySaved(Result<(Vec<String>, Vec<String>), String>),
    /// Chat turn acceptance; streaming continues over the event channel.
    ChatStarted(Result<(), String>),
    /// Chat cancel acceptance; the turn unwinds with a `cancelled` event.
    ChatCancelled(Result<(), String>),
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
    /// Rollback outcome: restored absolute paths.
    RolledBack(Result<Vec<String>, String>),
    /// Recall result: truncated conversation plus edited text.
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
    /// Durable UI state load result.
    UiState(Result<UiState, String>),
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
/// `try_send` returns immediately and wakes the task parked on the paired
/// [`BridgeEventRx`]. A closed receiver is ignored; the core keeps running
/// after the UI is gone.
pub(crate) type BridgeEventTx = async_channel::Sender<BridgeEvent>;

/// Receiving end of the bridge event stream.
///
/// Waiting on the receiver parks until the core sends or every sender is
/// dropped. Events already queued can be drained in the same wake, so one
/// wake applies a whole burst.
pub type BridgeEventRx = async_channel::Receiver<BridgeEvent>;

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
        let (event_tx, event_rx) = async_channel::unbounded();
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

    /// Sends one command and returns the reply future.
    ///
    /// Dropping the returned future cancels the wait but never cancels the
    /// durable core effect.
    ///
    /// # Panics
    ///
    /// Panics after [`CoreBridge::shutdown`].
    pub fn request(
        &self,
        command: BridgeCommand,
    ) -> impl Future<Output = BridgeReply> + Send + 'static {
        let (reply_tx, reply_rx) = oneshot::channel();
        let sender = self.command_tx.clone();
        async move {
            // An unbounded send only fails when the worker stopped; surface
            // that as the generic loss reply instead of panicking here.
            let sender = match sender {
                Some(sender) => sender,
                None => return BridgeReply::Sessions(Err("core bridge stopped".to_owned())),
            };
            if sender.send(command.with_reply(reply_tx)).is_err() {
                return BridgeReply::Sessions(Err("core thread stopped".to_owned()));
            }
            match reply_rx.await {
                Ok(reply) => reply,
                Err(_) => BridgeReply::Sessions(Err("core thread stopped".to_owned())),
            }
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
