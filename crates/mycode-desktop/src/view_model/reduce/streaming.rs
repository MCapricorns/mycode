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
        if streaming.status != status {
            status.clone_into(&mut streaming.status);
        }
    }
}

/// Buffers one streaming fragment into the active conversation.
pub(super) fn append_streaming(state: &mut WorkspaceState, thinking: bool, delta: String) {
    if delta.is_empty() {
        return;
    }
    // Only a thinking delta needs the catalog lookup behind the level.
    let show_reasoning = thinking && super::super::selected_reasoning_level(state) != "off";
    let status = if show_reasoning {
        t("Thinking", "思考中")
    } else {
        t("Replying", "正在回复")
    };
    set_streaming_status(state, status);
    if thinking && !show_reasoning {
        return;
    }
    let Some(streaming) = state
        .active
        .as_mut()
        .and_then(|conversation| conversation.streaming.as_mut())
    else {
        return;
    };
    let buffer = if thinking {
        &mut streaming.thinking
    } else {
        &mut streaming.text
    };
    // A char is at least one byte, so while the byte length stays under the
    // cap the whole delta fits without counting. Only a buffer near the cap
    // pays for the count; counting it on every delta made a long reply
    // quadratic.
    if buffer.len() + delta.len() <= MAX_STREAMING_CHARS {
        buffer.push_str(&delta);
    } else {
        let room = MAX_STREAMING_CHARS.saturating_sub(buffer.chars().count());
        buffer.extend(delta.chars().take(room));
    }
}
