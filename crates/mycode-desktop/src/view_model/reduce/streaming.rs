//! The live streaming bubble: status lines and buffered deltas for the turn
//! that is still running.

use mycode_app::{MAX_STREAMING_CHARS, StreamingReply};

use crate::i18n::t;

use crate::view_model::WorkspaceState;

pub(super) fn tool_call_label(name: &str, target: &str) -> String {
    let target = target.trim();
    if target.is_empty() {
        name.to_owned()
    } else {
        format!("{name}  {target}")
    }
}

/// Ensures a live streaming bubble exists and updates its status line.
pub(super) fn set_streaming_status(state: &mut WorkspaceState, status: &str) {
    if let Some(conversation) = state.active.as_mut() {
        let streaming = conversation
            .streaming
            .get_or_insert_with(StreamingReply::default);
        streaming.status = status.to_owned();
    }
}

/// Buffers one streaming fragment into the active conversation.
pub(super) fn append_streaming(state: &mut WorkspaceState, thinking: bool, delta: String) {
    if delta.is_empty() {
        return;
    }
    let show_reasoning = super::super::selected_reasoning_level(state) != "off";
    if let Some(conversation) = state.active.as_mut() {
        let streaming = conversation
            .streaming
            .get_or_insert_with(StreamingReply::default);
        streaming.status = if thinking && show_reasoning {
            t("Thinking", "思考中").to_owned()
        } else {
            t("Replying", "正在回复").to_owned()
        };
        if thinking && !show_reasoning {
            return;
        }
        let buffer = if thinking {
            &mut streaming.thinking
        } else {
            &mut streaming.text
        };
        // Take the remaining room once instead of re-counting the buffer per
        // pushed character. The buffer can grow to `MAX_STREAMING_CHARS`
        // (256 * 1024 characters), which made the per-char check quadratic
        // over a long turn.
        let room = MAX_STREAMING_CHARS.saturating_sub(buffer.chars().count());
        buffer.extend(delta.chars().take(room));
    }
}
