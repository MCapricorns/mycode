//! Display projections: committed ledger events become the
//! [`ConversationEntry`] rows a frontend renders.

use mycode_agent::session::{EventKind, SessionEvent};
use mycode_core::ToolResultMessage;
use mycode_core::{AssistantMessage, ContentBlock};

use crate::ledger::decode_text;
use crate::protocol::{ConversationEntry, EntryKind};

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
        EventKind::Usage => Some(project_usage(event.event_id.as_str(), payload)),
        // Legacy todo snapshots. They are not conversation and not usage.
        EventKind::Task => None,
    }
}

pub(crate) fn project_usage(event_id: &str, payload: &[u8]) -> ConversationEntry {
    let value: serde_json::Value = serde_json::from_slice(payload).unwrap_or_default();
    let provider = value["provider"].as_str().unwrap_or_default();
    let model = value["model"].as_str().unwrap_or("unknown");
    let input = value["input"].as_u64().unwrap_or_default();
    let output = value["output"].as_u64().unwrap_or_default();
    let cache = value["cache"].as_u64();
    let elapsed_ms = value["elapsed_ms"].as_u64().unwrap_or_default();
    // The spelling doubles as the rebuild input: the desktop replays these
    // rows to restore per-model totals, so the key must be `provider/model`
    // and the cache count must survive the projection.
    let key = if provider.is_empty() {
        model.to_owned()
    } else {
        format!("{provider}/{model}")
    };
    let context = value["context"].as_u64().unwrap_or(0);
    let context_cache = value["context_cache"].as_u64().unwrap_or(0);
    let mut text = format!("{key}: {input} in / {output} out");
    if context > 0 {
        text.push_str(&format!(" \u{b7} ctx {context}"));
    }
    if context_cache > 0 {
        text.push_str(&format!(" \u{b7} hit {context_cache}"));
    }
    if let Some(cache) = cache {
        text.push_str(&format!(" \u{b7} cache {cache}"));
    }
    if elapsed_ms > 0 {
        let per_second = output as f64 / (elapsed_ms as f64 / 1000.0);
        text.push_str(&format!(" \u{b7} {per_second:.0} tok/s"));
    }
    if let Some(cache) = cache
        && input > 0
    {
        text.push_str(&format!(" \u{b7} {}% cached", cache * 100 / input));
    }
    ConversationEntry {
        event_id: event_id.to_owned(),
        kind: EntryKind::Usage,
        text: text.into(),
        call_id: None,
        thinking: String::new(),
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
    use super::project_usage;

    #[test]
    fn usage_projection_keeps_the_latest_cache_read() {
        let payload = serde_json::json!({
            "provider": "zai",
            "model": "glm-5.3",
            "input": 100,
            "context": 12000,
            "context_cache": 11800,
            "output": 20,
            "cache": 11800,
        });
        let bytes = serde_json::to_vec(&payload).unwrap();
        let entry = project_usage("evt", &bytes);
        assert!(entry.text.contains("ctx 12000"));
        assert!(entry.text.contains("hit 11800"));
        assert!(entry.text.contains("cache 11800"));
    }
}
