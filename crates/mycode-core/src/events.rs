//! Agent loop fan-out events.
//!
//! `AgentEvent`, `MessageDelta`, and `TurnOutcome` form the live Agent loop
//! fan-out protocol. `mycode-agent` distributes `AgentEvent` through
//! `tokio::broadcast`, so the event type must stay `Clone`.

use serde::{Deserialize, Serialize};

use crate::error::MycodeError;
use crate::ids::CallId;
use crate::message::{SharedMessage, ToolResultMessage};

/// Events emitted by the Agent loop.
///
/// Subscribers receive them via `tokio::broadcast`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum AgentEvent {
    /// A new turn started processing.
    TurnStarted,
    /// Incremental delta while an assistant message streams in.
    MessageDelta(MessageDelta),
    /// A complete message was appended to the Agent history.
    ///
    /// Shared with the in-memory history so a broadcast clone does not copy
    /// the message body.
    MessageAdded(SharedMessage),
    /// A tool call started executing.
    ToolStarted {
        call_id: CallId,
        name: String,
        /// Path, query, or command the call is acting on. Empty when unknown.
        target: String,
    },
    /// Progress update from a running tool.
    ToolProgress { call_id: CallId, message: String },
    /// A tool call finished.
    ToolCompleted {
        call_id: CallId,
        result: ToolResultMessage,
    },
    /// A tool call inside `run_code`. Persisted for the UI only.
    ///
    /// These events are not appended to model history. `parent` is the
    /// provider id of the enclosing `run_code` call.
    PtcNested {
        parent: CallId,
        call_id: CallId,
        kind: PtcNestedKind,
    },
    /// The current turn ended.
    TurnEnded(TurnOutcome),
    /// An error occurred within the Agent loop.
    Error(MycodeError),
}

/// One phase of a tool call made from inside `run_code`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PtcNestedKind {
    /// The inner call started.
    Started { name: String, target: String },
    /// Incremental output or a subagent step.
    Progress { message: String },
    /// The inner call finished. `result` is the UI card, not model history.
    Completed { result: ToolResultMessage },
}

/// Incremental assistant content while streaming. See `docs/core.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum MessageDelta {
    /// Partial text content.
    TextDelta(String),
    /// Partial thinking content.
    ThinkingDelta(String),
    /// Partial tool-call arguments (raw JSON fragment).
    ToolCallDelta { id: String, partial_json: String },
}

/// Why a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnOutcome {
    /// The model stopped without further tool calls.
    Completed,
    /// The turn was aborted via cancellation.
    Aborted,
}
