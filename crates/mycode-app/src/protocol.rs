//! The vocabulary the core and a frontend agree on.
//!
//! These are the values [`crate::BridgeEvent`] and [`crate::BridgeReply`] are
//! typed in terms of: a session listing row, one transcript entry, the open
//! conversation, and the in-flight reply. They used to live in the desktop's
//! view model, which made the core's protocol depend on one particular UI.
//! They are plain data with no rendering concern, so they belong here and the
//! frontend reads them.

use std::sync::Arc;

// A frontend names sessions, branches, and heads when it issues a command, so
// the identity vocabulary is part of the protocol. Re-exported here so a
// frontend depends on this crate alone and never on the session substrate.
pub use mycode_agent::session::{BranchId, HeadStamp, SessionEventId, SessionId};

/// One row of the session listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    /// Session identity spelling (`ses1-…`).
    pub session_id: String,
    /// Root branch identity spelling.
    pub root_branch_id: String,
    /// Display title: the session's first user message, trimmed.
    pub title: String,
    /// Total committed events across branches.
    pub event_count: u64,
    /// Whether this session is currently open. The core reports `false` for
    /// every row of a listing and `true` only for a freshly `Created`
    /// session; a frontend that tracks open conversations recomputes the
    /// flag itself.
    pub active: bool,
    /// The index row is marked corrupt or failed validation. Such a row
    /// cannot be opened; a frontend offers deletion.
    pub corrupt: bool,
}

/// What one conversation entry represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    UserMessage,
    AssistantMessage,
    ToolCall,
    ToolResult,
    Usage,
}

/// One entry of a conversation transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationEntry {
    /// Event identity spelling.
    pub event_id: String,
    /// What this entry represents.
    pub kind: EntryKind,
    /// Display text, already lossy-decoded. Shared so a frontend can hand
    /// entries to its renderer without copying the transcript every frame.
    pub text: Arc<str>,
    /// Call identity for tool entries.
    pub call_id: Option<String>,
    /// Reasoning text shown above an assistant answer. Empty when none.
    pub thinking: String,
    /// Provider or ledger id of the enclosing `run_code` call. `None` for a
    /// top-level card. Live events use the provider id; replay uses the
    /// ledger id, matching the parent card's `call_id` in that view.
    pub parent_call_id: Option<String>,
}

/// The currently open conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveConversation {
    /// Session identity spelling.
    pub session_id: String,
    /// Root branch identity spelling.
    pub branch_id: String,
    /// Current committed head: `empty` or an event identity.
    pub head: String,
    /// Loaded entries in ledger order. This is a tail window, not the
    /// whole branch: earlier events stay on disk until the UI pages back.
    pub entries: Vec<ConversationEntry>,
    /// Oldest loaded event when earlier events remain. `None` once the
    /// branch start is in `entries`.
    pub older_before: Option<String>,
    /// Live assistant reply while a model turn streams.
    pub streaming: Option<StreamingReply>,
}

/// One page of transcript older than the cursor the UI already shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OlderTranscript {
    /// Session identity spelling.
    pub session_id: String,
    /// Branch identity spelling.
    pub branch_id: String,
    /// Entries in ledger order, strictly before `requested_before`.
    pub entries: Vec<ConversationEntry>,
    /// Next backward cursor, or `None` at the start of the branch.
    pub older: Option<String>,
    /// Cursor this page was requested with. A reply whose cursor no longer
    /// matches the open window is stale.
    pub requested_before: String,
}

/// Buffered fragments of a streaming reply.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamingReply {
    /// Visible assistant text so far.
    pub text: String,
    /// Reasoning text so far.
    pub thinking: String,
    /// One-line status shown even before the first token (waiting, thinking,
    /// running a tool). Empty only after the turn ends.
    pub status: String,
}

/// Upper bound kept for one streamed reply before further deltas are dropped.
pub const MAX_STREAMING_CHARS: usize = 256 * 1024;

/// One `ask_user` question presented to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskPrompt {
    /// Question headline.
    pub question: String,
    /// Candidate answers. Empty means free text only.
    pub choices: Vec<String>,
    /// Whether the user may skip this question.
    pub optional: bool,
    /// Whether more than one choice may be selected.
    pub multiple: bool,
}

/// Failure-message sentinel marking a user-initiated turn cancel.
pub const CHAT_CANCELLED: &str = "cancelled";
