//! Display projections: committed ledger events become the
//! [`ConversationEntry`] rows a frontend renders.

use mycode_agent::ToolResultMessage;
use mycode_agent::session::{EventKind, SessionEvent};
use mycode_agent::{AssistantMessage, ContentBlock};

use crate::ledger::decode_text;
use crate::protocol::{ConversationEntry, EntryKind};

fn assistant_meter(message: &AssistantMessage) -> Option<(u64, u64)> {
    let usage = message.usage.as_ref()?;
    let context = if usage.prompt_tokens > 0 {
        usage.prompt_tokens
    } else {
        usage.input_tokens
    };
    (context > 0).then_some((context, usage.cache_read_tokens.unwrap_or(0)))
}

fn usage_event_meter(payload: &[u8]) -> Option<(u64, u64)> {
    let value: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let context = value.get("context")?.as_u64()?;
    let cache = value
        .get("context_cache")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    (context > 0).then_some((context, cache))
}

#[cfg(test)]
pub(crate) fn meter_snapshot(kind: EventKind, payload: &[u8]) -> Option<(u64, u64)> {
    match kind {
        EventKind::Message => {
            let message: AssistantMessage = serde_json::from_slice(payload).ok()?;
            assistant_meter(&message)
        }
        EventKind::Usage => usage_event_meter(payload),
        EventKind::ToolCall | EventKind::ToolResult | EventKind::Task => None,
    }
}

#[cfg(test)]
pub(crate) fn project_replayed_entry(
    event: &SessionEvent,
    payload: &[u8],
) -> Option<ConversationEntry> {
    project_replayed_entry_with_meter(event, payload).0
}

pub(crate) fn project_replayed_entry_with_meter(
    event: &SessionEvent,
    payload: &[u8],
) -> (Option<ConversationEntry>, Option<(u64, u64)>) {
    match event.kind {
        EventKind::Message => {
            // Assistant messages are typed JSON; a parse miss means the
            // payload is the user's plain-text message. Parse once.
            match serde_json::from_slice::<AssistantMessage>(payload) {
                Ok(message) => (
                    Some(project_assistant_message(event.event_id.as_str(), &message)),
                    assistant_meter(&message),
                ),
                Err(_) => {
                    let text = match mycode_agent::StoredUserMessage::from_payload(payload) {
                        Some(stored) => stored.display_text(),
                        None => decode_text(payload),
                    };
                    (
                        Some(ConversationEntry {
                            event_id: event.event_id.as_str().to_owned(),
                            kind: EntryKind::UserMessage,
                            text: text.into(),
                            call_id: None,
                            thinking: String::new(),
                        }),
                        None,
                    )
                }
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
            (Some(entry), None)
        }
        EventKind::ToolCall => {
            let value: serde_json::Value = serde_json::from_slice(payload).unwrap_or_default();
            let name = value["name"]
                .as_str()
                .or_else(|| value["toolCall"]["name"].as_str())
                .unwrap_or("tool");
            let target = value["target"].as_str().unwrap_or("");
            (
                Some(ConversationEntry {
                    event_id: event.event_id.as_str().to_owned(),
                    kind: EntryKind::ToolCall,
                    text: mycode_agent::tool_label(name, target).into(),
                    call_id: event.call_id.as_ref().map(|call| call.as_str().to_owned()),
                    thinking: String::new(),
                }),
                None,
            )
        }
        EventKind::Usage => (None, usage_event_meter(payload)),
        // Legacy todo snapshots are not conversation.
        EventKind::Task => (None, None),
    }
}

/// Appends the code-mode inner-call breakdown from tool details.
///
/// A `run_code` result's text is only the script's own output; without
/// this, the transcript has nothing that names the tools the script
/// called, and every code-mode call renders as one generic `run_code`
/// row. Each summary becomes one `tool_label` line under
/// [`crate::protocol::CODE_CALLS_HEADING`], so a frontend can dispatch
/// each line to the renderer of the tool it names. The model-facing
/// content stays unchanged — this is display text, like the UI diff.
fn attach_inner_calls(mut text: String, details: Option<&serde_json::Value>) -> String {
    let Some(calls) = details
        .and_then(|details| details.get("inner_calls"))
        .and_then(serde_json::Value::as_array)
        .filter(|calls| !calls.is_empty())
    else {
        return text;
    };
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(crate::protocol::CODE_CALLS_HEADING);
    for call in calls {
        let name = call
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("tool");
        let target = call
            .get("target")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        text.push('\n');
        text.push_str(&mycode_agent::tool_label(name, target));
        let failed = call
            .get("is_error")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        match call.get("phase").and_then(serde_json::Value::as_str) {
            Some("running") => text.push_str(" (running)"),
            Some("cancelled") => text.push_str(" (cancelled)"),
            Some("failed") => text.push_str(" (failed)"),
            _ if failed => text.push_str(" (failed)"),
            _ => {}
        }
        if let Some(diff) = call
            .get("diff")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|diff| !diff.is_empty())
        {
            text.push('\n');
            text.push_str(diff);
        }
    }
    text
}

/// Appends the UI-only diff from tool details. The model-facing content
/// stays unchanged; this text is what the transcript renders.
fn attach_ui_diff(mut text: String, details: Option<&serde_json::Value>) -> String {
    let Some(diff) = details
        .and_then(|details| details.get("diff"))
        .and_then(|diff| diff.as_str())
        .map(str::trim)
        .filter(|diff| !diff.is_empty())
    else {
        return text;
    };
    if !text.contains(diff) {
        text.push('\n');
        text.push_str(diff);
    }
    text
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
    let text = attach_ui_diff(text, result.details.as_ref());
    let mut text = attach_inner_calls(text, result.details.as_ref());
    if result.is_error {
        text.insert_str(0, "failed: ");
    }
    ConversationEntry {
        event_id: event_id.to_owned(),
        kind: EntryKind::ToolResult,
        text: text.into(),
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
    // A user stop already shows as the frontends' red cancel banner; the
    // ledger keeps the marker so replayed context still records the cut.
    mycode_agent::strip_user_interruption(&mut text);
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
    fn a_run_code_result_names_the_tools_its_script_called() {
        let result = mycode_agent::ToolResultMessage {
            tool_call_id: "c1".to_owned(),
            content: vec![mycode_agent::ContentBlock::Text(
                mycode_agent::TextBlock::new("script output"),
            )],
            is_error: false,
            details: Some(serde_json::json!({
                "inner_calls": [
                    {"seq": 0, "name": "read", "target": "src/lib.rs", "is_error": false},
                    {"seq": 1, "name": "edit", "target": "src/main.rs", "is_error": true,
                     "diff": "+ new\n- old"},
                    {"seq": 2, "name": "agent", "target": "artisan", "is_error": true, "phase": "cancelled"},
                    {"seq": 3, "name": "agent", "target": "scout", "is_error": false, "phase": "running"},
                ],
            })),
        };
        let entry = super::project_tool_result_message("evt-x", &result);
        let text = entry.text.as_ref();
        assert!(text.starts_with("script output"), "{text}");
        assert!(text.contains(crate::protocol::CODE_CALLS_HEADING), "{text}");
        assert!(text.contains("read  src/lib.rs"), "{text}");
        assert!(text.contains("edit  src/main.rs (failed)"), "{text}");
        assert!(text.contains("+ new\n- old"), "{text}");
        assert!(text.contains("agent  artisan (cancelled)"), "{text}");
        assert!(text.contains("agent  scout (running)"), "{text}");
        // A result without inner calls gains no heading.
        let plain = mycode_agent::ToolResultMessage {
            details: None,
            ..result
        };
        let entry = super::project_tool_result_message("evt-y", &plain);
        assert!(!entry.text.contains(crate::protocol::CODE_CALLS_HEADING));
    }

    #[test]
    fn legacy_usage_events_replay_as_nothing() {
        let event = event_of_kind(mycode_agent::session::EventKind::Usage);
        assert!(project_replayed_entry(&event, b"{}").is_none());
    }

    #[test]
    fn a_user_cancel_hides_the_marker_but_keeps_the_partial() {
        let cancel = mycode_agent::interrupted_response_text(mycode_agent::USER_INTERRUPT_DETAIL);
        // A thinking-only interrupt paints no bare "[error] …" row under the
        // thinking box; the red cancel banner already says it.
        let thinking_only = mycode_agent::AssistantMessage {
            blocks: vec![
                mycode_agent::ContentBlock::Thinking(mycode_agent::ThinkingBlock::new("planning")),
                mycode_agent::ContentBlock::Text(mycode_agent::TextBlock::new(cancel.clone())),
            ],
            usage: None,
            stop_reason: mycode_agent::StopReason::Error,
        };
        let entry = super::project_assistant_message("evt-x", &thinking_only);
        assert!(
            entry.text.is_empty(),
            "cancel marker leaked: {}",
            entry.text
        );
        assert_eq!(entry.thinking, "planning");
        // A partial reply keeps its text; only the marker line goes.
        let partial = mycode_agent::AssistantMessage {
            blocks: vec![mycode_agent::ContentBlock::Text(
                mycode_agent::TextBlock::new(format!("half an answer\n{cancel}")),
            )],
            usage: None,
            stop_reason: mycode_agent::StopReason::Error,
        };
        let entry = super::project_assistant_message("evt-x", &partial);
        assert_eq!(entry.text.as_ref(), "half an answer");
        // A genuine provider failure keeps its marker: it is the only
        // transcript record of what went wrong.
        let failure = mycode_agent::AssistantMessage {
            blocks: vec![mycode_agent::ContentBlock::Text(
                mycode_agent::TextBlock::new(mycode_agent::interrupted_response_text(
                    "the response ended early",
                )),
            )],
            usage: None,
            stop_reason: mycode_agent::StopReason::Error,
        };
        let entry = super::project_assistant_message("evt-x", &failure);
        assert!(entry.text.contains("[error] the response was interrupted"));
    }

    #[test]
    fn the_last_usage_snapshot_is_the_context_meter() {
        let assistant = mycode_agent::AssistantMessage {
            blocks: vec![mycode_agent::ContentBlock::Text(
                mycode_agent::TextBlock::new("done"),
            )],
            usage: Some(mycode_agent::Usage {
                input_tokens: 900,
                output_tokens: 20,
                cache_read_tokens: Some(41_472),
                cache_write_tokens: None,
                prompt_tokens: 42_365,
            }),
            stop_reason: mycode_agent::StopReason::Stop,
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
