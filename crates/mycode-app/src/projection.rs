//! Display projections: committed ledger events become the
//! [`ConversationEntry`] rows a frontend renders.

use mycode_agent::session::{EventKind, SessionEvent};
use mycode_core::ToolResultMessage;
use mycode_core::{AssistantMessage, ContentBlock};

use crate::ledger::decode_text;
use crate::protocol::{ConversationEntry, EntryKind};

/// Latest prompt size carried by one ledger payload.
///
/// Current turns store it on the assistant message (`prompt_tokens` and
/// `cache_read_tokens`). Older ledgers stored a `usage` event whose
/// `context` / `context_cache` fields are the same meter. A payload with no
/// positive prompt size is not a snapshot.
pub(crate) fn meter_snapshot(kind: EventKind, payload: &[u8]) -> Option<(u64, u64)> {
    let (context, cache) = match kind {
        EventKind::Message => assistant_meter(payload)?,
        EventKind::Usage => usage_event_meter(payload)?,
        EventKind::ToolCall | EventKind::ToolResult | EventKind::Task => return None,
    };
    (context > 0).then_some((context, cache))
}

fn assistant_meter(payload: &[u8]) -> Option<(u64, u64)> {
    let message: AssistantMessage = serde_json::from_slice(payload).ok()?;
    let usage = message.usage?;
    let context = if usage.prompt_tokens > 0 {
        usage.prompt_tokens
    } else {
        usage.input_tokens
    };
    Some((context, usage.cache_read_tokens.unwrap_or(0)))
}

fn usage_event_meter(payload: &[u8]) -> Option<(u64, u64)> {
    let value: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let context = value.get("context")?.as_u64()?;
    let cache = value
        .get("context_cache")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some((context, cache))
}

pub(crate) fn project_replayed_entry(
    event: &SessionEvent,
    payload: &[u8],
) -> Option<ConversationEntry> {
    match event.kind {
        EventKind::Message => {
            // Assistant messages are typed JSON; a parse miss means the
            // payload is the user's plain-text message. Parse once.
            match serde_json::from_slice::<AssistantMessage>(payload) {
                Ok(message) => Some(project_assistant_message(event.event_id.as_str(), &message)),
                Err(_) => Some(ConversationEntry {
                    event_id: event.event_id.as_str().to_owned(),
                    kind: EntryKind::UserMessage,
                    text: decode_text(payload).into(),
                    call_id: None,
                    thinking: String::new(),
                }),
            }
        }
        EventKind::ToolResult => {
            let mut entry = project_tool_result(event.event_id.as_str(), payload);
            // Pair with the ToolCall row, which is keyed by the ledger call
            // identity. The payload's tool_call_id is the provider id, a
            // different namespace, so using it left every reopened tool row
            // in progress.
            if let Some(call) = event.call_id.as_ref() {
                entry.call_id = Some(call.as_str().to_owned());
            }
            Some(entry)
        }
        EventKind::ToolCall => {
            let value: serde_json::Value = serde_json::from_slice(payload).unwrap_or_default();
            let name = value["name"]
                .as_str()
                .or_else(|| value["toolCall"]["name"].as_str())
                .unwrap_or("tool");
            let target = value["target"].as_str().unwrap_or("");
            Some(ConversationEntry {
                event_id: event.event_id.as_str().to_owned(),
                kind: EntryKind::ToolCall,
                text: mycode_core::tool_label(name, target).into(),
                call_id: event.call_id.as_ref().map(|call| call.as_str().to_owned()),
                thinking: String::new(),
            })
        }
        // Legacy durable usage records and todo snapshots. Neither is
        // conversation; ledgers written by older builds still decode.
        EventKind::Usage | EventKind::Task => None,
    }
}

/// Appends the UI-only diff from tool details. The model-facing content
/// stays unchanged; this text is what the transcript renders.
fn attach_ui_diff(text: &str, details: Option<&serde_json::Value>) -> String {
    let Some(diff) = details
        .and_then(|details| details.get("diff"))
        .and_then(|diff| diff.as_str())
        .map(str::trim)
        .filter(|diff| !diff.is_empty())
    else {
        return text.to_owned();
    };
    if text.contains(diff) {
        return text.to_owned();
    }
    format!("{text}\n{diff}")
}

/// Projects a committed tool-result payload into a display entry.
pub(crate) fn project_tool_result(event_id: &str, payload: &[u8]) -> ConversationEntry {
    let result: ToolResultMessage =
        serde_json::from_slice(payload).unwrap_or_else(|_| ToolResultMessage {
            tool_call_id: String::new(),
            content: Vec::new(),
            is_error: true,
            details: None,
        });
    project_tool_result_message(event_id, &result)
}

/// Projects an in-memory tool result without a second JSON parse.
pub(crate) fn project_tool_result_message(
    event_id: &str,
    result: &ToolResultMessage,
) -> ConversationEntry {
    let mut text = String::new();
    for block in &result.content {
        if let ContentBlock::Text(block) = block {
            text.push_str(&block.text);
        }
    }
    let text = attach_ui_diff(&text, result.details.as_ref());
    ConversationEntry {
        event_id: event_id.to_owned(),
        kind: EntryKind::ToolResult,
        text: if result.is_error {
            format!("failed: {text}").into()
        } else {
            text.into()
        },
        call_id: Some(result.tool_call_id.clone()),
        thinking: String::new(),
    }
}

/// Projects an in-memory assistant message without a second JSON parse.
pub(crate) fn project_assistant_message(
    event_id: &str,
    message: &AssistantMessage,
) -> ConversationEntry {
    let mut text = String::new();
    let mut thinking = String::new();
    for block in &message.blocks {
        match block {
            ContentBlock::Text(block) => text.push_str(&block.text),
            ContentBlock::Thinking(block) => {
                if !thinking.is_empty() {
                    thinking.push('\n');
                }
                thinking.push_str(&block.text);
            }
            // Tool calls already render as their own ledger rows.
            ContentBlock::ToolCall(_) => {}
            _ => {}
        }
    }
    ConversationEntry {
        event_id: event_id.to_owned(),
        kind: EntryKind::AssistantMessage,
        text: text.into(),
        call_id: None,
        thinking,
    }
}

#[cfg(test)]
mod tests {
    use mycode_agent::session::SessionEvent;

    use super::project_replayed_entry;

    fn event_of_kind(kind: mycode_agent::session::EventKind) -> SessionEvent {
        SessionEvent {
            event_id: mycode_agent::session::SessionEventId::parse(
                "evt1-00000000000000000000000000000000",
            )
            .expect("event id"),
            digest: "sha256:0".to_owned(),
            bytes: 1,
            kind,
            call_id: None,
        }
    }

    #[test]
    fn legacy_usage_events_replay_as_nothing() {
        let event = event_of_kind(mycode_agent::session::EventKind::Usage);
        assert!(project_replayed_entry(&event, b"{}").is_none());
    }

    #[test]
    fn the_last_usage_snapshot_is_the_context_meter() {
        let assistant = mycode_core::AssistantMessage {
            blocks: vec![mycode_core::ContentBlock::Text(
                mycode_core::TextBlock::new("done"),
            )],
            usage: Some(mycode_core::Usage {
                input_tokens: 900,
                output_tokens: 20,
                cache_read_tokens: Some(41_472),
                cache_write_tokens: None,
                prompt_tokens: 42_365,
            }),
            stop_reason: mycode_core::StopReason::Stop,
        };
        let bytes = serde_json::to_vec(&assistant).expect("assistant");
        assert_eq!(
            super::meter_snapshot(mycode_agent::session::EventKind::Message, &bytes),
            Some((42_365, 41_472))
        );
        let older = br#"{"context": 100, "context_cache": 80}"#;
        assert_eq!(
            super::meter_snapshot(mycode_agent::session::EventKind::Usage, older),
            Some((100, 80))
        );
        assert!(super::meter_snapshot(mycode_agent::session::EventKind::Usage, b"{}").is_none());
        assert!(
            super::meter_snapshot(
                mycode_agent::session::EventKind::Message,
                b"plain user text"
            )
            .is_none()
        );
    }
}
