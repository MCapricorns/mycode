//! Progress envelopes for tools called inside `run_code`.
//!
//! The outer tool event keeps its own call id. Its message is
//! `code_mode|<JSON>`; the envelope's `call_id` is the exact inner
//! `ToolCtx.call_id` used by the host's cancellation registration. Raw tool
//! messages (including `agent|role|phase|detail`) remain unchanged inside
//! `Progress.message`. Consumers must use the inner id for child identity
//! and cancellation, and `Completed` for the child's terminal state.

use serde::{Deserialize, Serialize};

/// Prefix of a JSON code-mode lifecycle/progress message.
pub const CODE_MODE_PROGRESS_PREFIX: &str = "code_mode|";

/// State of one inner call. Only snapshots use `Running`; a completion
/// envelope always carries a terminal phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InnerCallPhase {
    /// The inner tool is preparing or executing.
    Running,
    /// The tool returned successfully.
    Completed,
    /// Dispatch, tool execution, or the script task failed.
    Failed,
    /// The tool or its owning cell was cancelled.
    Cancelled,
}

/// Lifecycle of an inner call, carried in the outer tool's progress stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CodeModeProgress {
    /// Published before preflight or tool execution starts.
    Started {
        /// Exact inner ToolCtx call id (`outer_call_id.seq`).
        call_id: String,
        /// Registered tool name.
        name: String,
        /// Script journal sequence.
        seq: u64,
        /// One-line target of the tool's arguments.
        target: String,
    },
    /// One original tool progress message, without a name prefix or edits.
    Progress {
        /// Exact inner ToolCtx call id.
        call_id: String,
        /// Registered tool name.
        name: String,
        /// Original progress message; may contain pipes or newlines.
        message: String,
    },
    /// One terminal state, including cancellation and unwinding.
    Completed {
        /// Exact inner ToolCtx call id.
        call_id: String,
        /// Registered tool name.
        name: String,
        /// Terminal inner-call phase.
        phase: InnerCallPhase,
        /// True for a failed or cancelled call.
        is_error: bool,
    },
}

impl CodeModeProgress {
    /// Parses an envelope, leaving ordinary progress and malformed messages
    /// to the caller's existing fallback renderer.
    #[must_use]
    pub fn parse(message: &str) -> Option<Self> {
        serde_json::from_str(message.strip_prefix(CODE_MODE_PROGRESS_PREFIX)?).ok()
    }

    /// Encodes one envelope as an outer tool progress message.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{CODE_MODE_PROGRESS_PREFIX}{}",
            serde_json::to_string(self)
                .expect("code-mode progress contains only JSON-compatible values")
        )
    }
}
