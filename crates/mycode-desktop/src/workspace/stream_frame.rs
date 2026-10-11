//! How many live chat events one paint may apply.
//!
//! The core sends `ChatText` and `ChatThinking` as they arrive, then
//! `ChatDone`. The UI task used to drain that whole queue before the next
//! frame, so a fast turn painted only the finished message. A frame takes a
//! short slice of deltas, leaves the closing event for the next frame, and
//! the pump waits about one frame so the slice can paint.

use std::collections::VecDeque;

use mycode_app::BridgeEvent;

/// Gap between streaming paints. Inside the 50–80ms band: often enough to
/// read the reply growing, rare enough that a burst of tokens does not
/// relayout every character.
pub(super) const STREAM_FRAME: std::time::Duration = std::time::Duration::from_millis(64);

/// Already-queued assistant text one frame may reveal. Live tokens are
/// slower than this, so a real stream shows whatever arrived since the last
/// paint. A backlog cannot dump the rest of the reply into the same frame.
const STREAM_CHARS_PER_FRAME: usize = 80;

/// Cap on delta events per frame, so a burst of empty fragments cannot skip
/// the character budget and run to `ChatDone`.
const STREAM_DELTAS_PER_FRAME: usize = 16;

/// Splits a queued burst into the events to paint now and the ones that wait
/// for the next frame.
pub(super) fn split_stream_frame(
    queued: impl IntoIterator<Item = BridgeEvent>,
) -> (Vec<BridgeEvent>, VecDeque<BridgeEvent>) {
    let mut queued: VecDeque<BridgeEvent> = queued.into_iter().collect();
    let mut slice = Vec::new();
    let mut chars = 0usize;
    let mut deltas = 0usize;
    let mut saw_delta = false;
    let mut saw_tool_start = false;
    while let Some(event) = queued.pop_front() {
        let delta_chars = stream_delta_chars(&event);
        let closes = closes_live_text(&event);
        let tool_finished = is_tool_finished(&event);
        if closes && saw_delta {
            queued.push_front(event);
            break;
        }
        if tool_finished && saw_tool_start {
            queued.push_front(event);
            break;
        }
        if delta_chars.is_some()
            && saw_delta
            && (chars >= STREAM_CHARS_PER_FRAME || deltas >= STREAM_DELTAS_PER_FRAME)
        {
            queued.push_front(event);
            break;
        }
        if let Some(count) = delta_chars {
            chars += count;
            deltas += 1;
            saw_delta = true;
        }
        if is_tool_started(&event) {
            saw_tool_start = true;
        }
        slice.push(event);
        if closes {
            break;
        }
    }
    (slice, queued)
}

pub(super) fn is_stream_delta(event: &BridgeEvent) -> bool {
    stream_delta_chars(event).is_some()
}

fn stream_delta_chars(event: &BridgeEvent) -> Option<usize> {
    match event {
        BridgeEvent::ChatText { delta, .. } | BridgeEvent::ChatThinking { delta, .. } => {
            Some(delta.chars().count())
        }
        _ => None,
    }
}

fn closes_live_text(event: &BridgeEvent) -> bool {
    matches!(
        event,
        BridgeEvent::ChatDone { .. }
            | BridgeEvent::ChatFailed { .. }
            | BridgeEvent::AssistantStep { .. }
    )
}

fn is_tool_started(event: &BridgeEvent) -> bool {
    matches!(event, BridgeEvent::ToolStarted { .. })
}

fn is_tool_finished(event: &BridgeEvent) -> bool {
    matches!(event, BridgeEvent::ToolCompleted { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use mycode_app::{ConversationEntry, EntryKind};

    fn text(delta: &str) -> BridgeEvent {
        BridgeEvent::ChatText {
            session_id: "s".into(),
            delta: delta.into(),
        }
    }

    fn done() -> BridgeEvent {
        BridgeEvent::ChatDone {
            session_id: "s".into(),
            head: "h".into(),
            entry: ConversationEntry {
                event_id: "e".into(),
                kind: EntryKind::AssistantMessage,
                text: Arc::from("full"),
                call_id: None,
                thinking: String::new(),
                parent_call_id: None,
            },
        }
    }

    fn tool_start() -> BridgeEvent {
        BridgeEvent::ToolStarted {
            session_id: "s".into(),
            call_id: "c".into(),
            name: "read".into(),
            target: "a.rs".into(),
            parent: None,
        }
    }

    fn tool_done() -> BridgeEvent {
        BridgeEvent::ToolCompleted {
            session_id: "s".into(),
            entry: ConversationEntry {
                event_id: "r".into(),
                kind: EntryKind::ToolResult,
                text: Arc::from("ok"),
                call_id: Some("c".into()),
                thinking: String::new(),
                parent_call_id: None,
            },
        }
    }

    fn delta_text(events: &[BridgeEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                BridgeEvent::ChatText { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_dumped_turn_paints_deltas_before_chat_done() {
        let mut queued: VecDeque<BridgeEvent> = (0..20).map(|_| text("abcdefghij")).collect();
        queued.push_back(done());
        let mut painted = String::new();
        let mut frames = 0usize;
        while !queued.is_empty() {
            let (slice, rest) = split_stream_frame(queued);
            assert!(!slice.is_empty());
            let slice_has_delta = slice.iter().any(is_stream_delta);
            let slice_closes = slice.iter().any(closes_live_text);
            assert!(
                !(slice_has_delta && slice_closes),
                "a frame must not commit the reply in the same paint as new text"
            );
            painted.push_str(&delta_text(&slice));
            if slice_closes {
                assert_eq!(painted, "abcdefghij".repeat(20));
            }
            queued = rest;
            frames += 1;
            assert!(frames < 40, "the burst did not drain");
        }
        assert!(frames > 2);
    }

    #[test]
    fn one_delta_does_not_share_a_frame_with_chat_done() {
        let (slice, rest) = split_stream_frame([text("Hello"), done()]);
        assert_eq!(delta_text(&slice), "Hello");
        assert!(rest.iter().any(closes_live_text));
    }

    #[test]
    fn tool_start_paints_before_its_result() {
        let (slice, rest) = split_stream_frame([tool_start(), tool_done()]);
        assert!(slice.iter().any(is_tool_started));
        assert!(slice.iter().all(|event| !is_tool_finished(event)));
        assert!(rest.iter().any(is_tool_finished));
    }
}
