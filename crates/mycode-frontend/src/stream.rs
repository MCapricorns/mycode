//! How many live chat events one paint may apply.
//!
//! The core sends `ChatText` and `ChatThinking` as they arrive, then
//! `ChatDone`. A UI task that drains that whole queue before the next frame
//! paints only the finished message. Small bursts keep a frame between live
//! text and its commit. Large backlogs catch up without a typewriter delay,
//! and failure/cancellation terminals bypass cosmetic pacing entirely.

use std::collections::VecDeque;

use mycode_app::BridgeEvent;

/// Gap between streaming paints. Inside the 50–80ms band: often enough to
/// read the reply growing, rare enough that a burst of tokens does not
/// relayout every character.
pub const STREAM_FRAME: std::time::Duration = std::time::Duration::from_millis(64);

/// Already-queued assistant text one frame may reveal. Live tokens are
/// slower than this, so a real stream shows whatever arrived since the last
/// paint. A backlog cannot dump the rest of the reply into the same frame.
const STREAM_CHARS_PER_FRAME: usize = 80;

/// Cap on delta events per frame, so a burst of empty fragments cannot skip
/// the character budget and run to `ChatDone`.
const STREAM_DELTAS_PER_FRAME: usize = 16;

/// Splits a queued burst into the events to paint now and the ones that wait
/// for the next frame.
pub fn split_stream_frame(
    queued: impl IntoIterator<Item = BridgeEvent>,
) -> (Vec<BridgeEvent>, VecDeque<BridgeEvent>) {
    let mut queued: VecDeque<BridgeEvent> = queued.into_iter().collect();
    // A stop must never wait behind seconds of already-received text. Keep
    // every event in order, including partial commits and tool results, but
    // close the failed turn in this paint. Leave a successor turn queued.
    if let Some(terminal) = queued
        .iter()
        .position(|event| matches!(event, BridgeEvent::ChatFailed { .. }))
    {
        return (queued.drain(..=terminal).collect(), queued);
    }
    let catch_up = queued.len() > STREAM_DELTAS_PER_FRAME * 8
        || queued.iter().filter_map(stream_delta_chars).sum::<usize>() > STREAM_CHARS_PER_FRAME * 8;
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
        if !catch_up
            && delta_chars.is_some()
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

/// Hidden sessions do not render text. Drop only their cosmetic deltas;
/// terminal and control events still reach the driver to settle held sends.
pub fn retain_active_deltas(queued: &mut VecDeque<BridgeEvent>, active_session: Option<&str>) {
    queued.retain(|event| match event {
        BridgeEvent::ChatText { session_id, .. } | BridgeEvent::ChatThinking { session_id, .. } => {
            active_session == Some(session_id.as_ref())
        }
        _ => true,
    });
}

pub fn is_stream_delta(event: &BridgeEvent) -> bool {
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
            },
        }
    }

    fn tool_start() -> BridgeEvent {
        BridgeEvent::ToolStarted {
            session_id: "s".into(),
            call_id: "c".into(),
            provider_call_id: "p".into(),
            name: "read".into(),
            target: "a.rs".into(),
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
        assert!(!rest.iter().any(is_stream_delta));
    }

    #[test]
    fn cancellation_flushes_backlog_without_losing_partial_work() {
        let mut queued: VecDeque<_> = (0..500).map(|_| text("partial 世界")).collect();
        queued.push_back(tool_start());
        queued.push_back(tool_done());
        queued.push_back(BridgeEvent::ChatFailed {
            session_id: "s".into(),
            message: mycode_app::CHAT_CANCELLED.into(),
            head: "partial-head".into(),
        });
        queued.push_back(text("next turn"));
        let (slice, rest) = split_stream_frame(queued);
        assert_eq!(delta_text(&slice), "partial 世界".repeat(500));
        assert!(slice.iter().any(is_tool_finished));
        assert!(matches!(slice.last(), Some(BridgeEvent::ChatFailed { .. })));
        assert_eq!(rest.len(), 1);
        assert_eq!(
            delta_text(&rest.into_iter().collect::<Vec<_>>()),
            "next turn"
        );
    }

    #[test]
    fn large_backlog_catches_up_before_committing() {
        let mut queued: VecDeque<_> = (0..2_000).map(|_| text("hello 世界")).collect();
        queued.push_back(done());
        let (slice, rest) = split_stream_frame(queued);
        assert_eq!(delta_text(&slice), "hello 世界".repeat(2_000));
        assert_eq!(rest.len(), 1);
        assert!(matches!(rest.front(), Some(BridgeEvent::ChatDone { .. })));
    }

    #[test]
    fn hidden_session_text_does_not_delay_visible_work_or_drop_control_events() {
        let mut queued = VecDeque::from([text("hidden"), done(), tool_done()]);
        queued.push_back(BridgeEvent::ChatText {
            session_id: "visible".into(),
            delta: "current".into(),
        });
        retain_active_deltas(&mut queued, Some("visible"));
        assert_eq!(queued.len(), 3);
        assert!(matches!(queued.front(), Some(BridgeEvent::ChatDone { .. })));
        assert_eq!(
            delta_text(&queued.iter().cloned().collect::<Vec<_>>()),
            "current"
        );
        retain_active_deltas(&mut queued, None);
        assert_eq!(queued.len(), 2);
        assert!(queued.iter().all(|event| !is_stream_delta(event)));
    }

    #[test]
    fn tool_start_paints_before_its_result() {
        let (slice, rest) = split_stream_frame([tool_start(), tool_done()]);
        assert!(slice.iter().any(is_tool_started));
        assert!(slice.iter().all(|event| !is_tool_finished(event)));
        assert!(rest.iter().any(is_tool_finished));
    }
}
