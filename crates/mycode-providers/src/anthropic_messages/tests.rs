use mycode_core::{AssistantMessage, ContentBlock, Message, Request, StopReason, ThinkingBlock};

use super::MessagesReducer;
use crate::anthropic_messages::build_body;
use crate::driver::FrameReducer;

fn thinking_of(message: &AssistantMessage) -> String {
    message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Thinking(thinking) => Some(thinking.text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn eof_before_message_stop_keeps_thinking() {
    let mut reducer = MessagesReducer::new();
    reducer.feed(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        );
    reducer.feed(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"still here"}}"#,
        );
    let mycode_core::StreamEvent::Done { message } = reducer.finish() else {
        panic!("done");
    };
    assert_eq!(thinking_of(&message), "still here");
    assert!(message.text().contains("message_stop"));
    assert_eq!(message.stop_reason, StopReason::Error);
}

fn done(events: Vec<mycode_core::StreamEvent>) -> AssistantMessage {
    match events.into_iter().last() {
        Some(mycode_core::StreamEvent::Done { message }) => message,
        other => panic!("expected done, got {other:?}"),
    }
}

#[test]
fn redacted_thinking_replays_its_encrypted_data() {
    let mut reducer = MessagesReducer::new();
    reducer.feed(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"ENCRYPTED"}}"#,
        );
    reducer.feed(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read","input":{}}}"#,
        );
    reducer.feed(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.rs\"}"}}"#,
        );
    reducer.feed(r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#);
    let message = done(reducer.feed(r#"{"type":"message_stop"}"#));
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    let body = build_body(
        "claude-sonnet-5-5",
        "https://api.anthropic.com/v1/messages",
        &Request {
            messages: vec![std::sync::Arc::new(Message::Assistant(message))],
            ..Request::default()
        },
    );
    let content = &body["messages"][0]["content"];
    assert_eq!(content[0]["type"], "redacted_thinking");
    assert_eq!(content[0]["data"], "ENCRYPTED");
    assert_eq!(content[1]["type"], "tool_use");
}

#[test]
fn a_native_block_never_overwrites_an_xml_tool_call() {
    let mut reducer = MessagesReducer::new();
    reducer.feed(
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
    );
    let xml = concat!(
        "<minimax:tool_call>",
        "<invoke name=\"find\">",
        "<parameter name=\"pattern\">*.rs</parameter>",
        "</invoke>",
        "</minimax:tool_call>",
    );
    let delta = serde_json::json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "text_delta", "text": xml},
    });
    reducer.feed(&delta.to_string());
    // The XML call took position 1; the native block at wire index 1
    // must get its own slot.
    reducer.feed(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_native","name":"read","input":{}}}"#,
        );
    reducer.feed(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.rs\"}"}}"#,
        );
    let message = done(reducer.feed(r#"{"type":"message_stop"}"#));
    let names: Vec<_> = message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["find", "read"]);
}

#[test]
fn content_block_deltas_route_by_wire_index_and_join_signature_deltas() {
    let mut reducer = MessagesReducer::new();
    reducer.feed(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        );
    reducer.feed(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_a","name":"read","input":{}}}"#,
        );
    reducer.feed(
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_b","name":"grep","input":{}}}"#,
        );
    // Deltas for block 1 and block 0 arriving after block 2 started must
    // still reach their own blocks instead of landing on `self.current`.
    reducer.feed(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.rs\"}"}}"#,
        );
    reducer.feed(
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"pattern\":\"x\"}"}}"#,
        );
    reducer.feed(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"plan"}}"#,
        );
    reducer.feed(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-"}}"#,
        );
    reducer.feed(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"part2"}}"#,
        );
    reducer.feed(r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#);
    let message = done(reducer.feed(r#"{"type":"message_stop"}"#));
    assert_eq!(thinking_of(&message), "plan");
    let signature = message.blocks.iter().find_map(|block| match block {
        ContentBlock::Thinking(thinking) => thinking.signature.as_deref(),
        _ => None,
    });
    assert_eq!(signature, Some("sig-part2"));
    let calls: Vec<_> = message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => {
                Some((call.id.as_str(), call.name.as_str(), &call.arguments))
            }
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "toolu_a");
    assert_eq!(calls[0].2["path"], "a.rs");
    assert_eq!(calls[1].0, "toolu_b");
    assert_eq!(calls[1].2["pattern"], "x");
}

#[test]
fn error_event_keeps_prior_thinking() {
    let mut reducer = MessagesReducer::new();
    reducer.feed(
        r#"{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"partial"}}"#,
    );
    let events = reducer.feed(r#"{"type":"error","error":{"message":"overloaded"}}"#);
    let mycode_core::StreamEvent::Done { message } = events.into_iter().next().unwrap() else {
        panic!("done");
    };
    assert_eq!(thinking_of(&message), "partial");
    assert!(message.text().contains("overloaded"));
}

#[test]
fn an_output_free_error_frame_is_a_request_failure() {
    let mut reducer = MessagesReducer::new();
    reducer.feed(r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#);
    let events = reducer
        .feed(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#);
    let [mycode_core::StreamEvent::Error(error)] = events.as_slice() else {
        panic!("expected one error terminal, got {events:?}");
    };
    assert_eq!(error.status(), Some(529));
    assert_eq!(error.message(), Some("Overloaded"));
}

#[test]
fn parallel_tool_results_and_a_steer_share_one_user_turn() {
    use mycode_core::{ToolCall, ToolResultMessage, UserMessage};
    let result = |id: &str| {
        std::sync::Arc::new(Message::ToolResult(ToolResultMessage {
            tool_call_id: id.to_owned(),
            content: vec![ContentBlock::Text(mycode_core::TextBlock::new("ok"))],
            is_error: false,
            details: None,
        }))
    };
    let request = Request {
        messages: vec![
            std::sync::Arc::new(Message::User(UserMessage::text("go"))),
            std::sync::Arc::new(Message::Assistant(AssistantMessage {
                blocks: vec![
                    ContentBlock::ToolCall(ToolCall::new("a", "read", serde_json::json!({}))),
                    ContentBlock::ToolCall(ToolCall::new("b", "grep", serde_json::json!({}))),
                ],
                usage: None,
                stop_reason: StopReason::ToolUse,
            })),
            result("a"),
            result("b"),
            std::sync::Arc::new(Message::User(UserMessage::text("also check tests"))),
        ],
        ..Request::default()
    };
    let body = build_body(
        "kimi-for-coding",
        "https://api.kimi.com/coding/v1/messages",
        &request,
    );
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 3, "{body}");
    let content = messages[2]["content"].as_array().expect("content");
    let kinds: Vec<_> = content
        .iter()
        .map(|block| block["type"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(kinds, ["tool_result", "tool_result", "text"]);
    assert_eq!(content[0]["tool_use_id"], "a");
    assert_eq!(content[1]["tool_use_id"], "b");
}

#[test]
fn a_tool_result_never_joins_a_text_turn() {
    use mycode_core::{ToolResultMessage, UserMessage};
    let request = Request {
        messages: vec![
            std::sync::Arc::new(Message::User(UserMessage::text("summary"))),
            std::sync::Arc::new(Message::ToolResult(ToolResultMessage {
                tool_call_id: "orphan".to_owned(),
                content: Vec::new(),
                is_error: false,
                details: None,
            })),
        ],
        ..Request::default()
    };
    let body = build_body(
        "glm-5.3",
        "https://api.z.ai/api/anthropic/v1/messages",
        &request,
    );
    assert_eq!(body["messages"].as_array().map(Vec::len), Some(2));
}

#[test]
fn a_rejected_signature_retries_without_any_thinking() {
    use mycode_core::{ProviderError, ProviderErrorKind};
    let body = serde_json::json!({
        "model": "claude-opus-5-5",
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "go"}]},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "", "signature": "sig"},
                {"type": "tool_use", "id": "a", "name": "read", "input": {}}
            ]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": "ok"}]},
            {"role": "assistant", "content": [{"type": "redacted_thinking", "data": "x"}]},
            {"role": "user", "content": [{"type": "text", "text": "next"}]}
        ]
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(super::replays_thinking(&body));
    let rejected = ProviderError::with_message(
        ProviderErrorKind::Rejected,
        "HTTP 400 Bad Request: messages.1.content.0: Invalid `signature` in `thinking` block. \
             The block is bound to a different conversation.",
    );
    let retried = super::retry_body_without_thinking(&rejected, &bytes).expect("retry");
    let retried: serde_json::Value = serde_json::from_slice(&retried).unwrap();
    let messages = retried["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4, "the thinking-only turn is dropped");
    assert_eq!(messages[1]["content"][0]["type"], "tool_use");
    assert!(
        !serde_json::to_string(&retried)
            .unwrap()
            .contains("thinking")
    );

    let other = ProviderError::with_message(ProviderErrorKind::Rejected, "HTTP 400: bad tool");
    assert!(super::retry_body_without_thinking(&other, &bytes).is_none());
    let plain = serde_json::json!({"messages": [
        {"role": "user", "content": [{"type": "text", "text": r#"{"type":"thinking"}"#}]},
        {"role": "assistant", "content": [{"type": "text", "text": r#""type":"thinking""#}]}
    ]});
    assert!(
        !super::replays_thinking(&plain),
        "text mentioning thinking is not a block"
    );
    let plain = serde_json::to_vec(&plain).unwrap();
    assert!(super::retry_body_without_thinking(&rejected, &plain).is_none());
}

#[test]
fn refusal_ends_the_message_as_a_visible_error() {
    let mut reducer = MessagesReducer::new();
    reducer.feed(r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#);
    let message = done(reducer.feed(r#"{"type":"message_stop"}"#));
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(message.text().contains("refusal"), "{}", message.text());

    let mut reducer = MessagesReducer::new();
    reducer.feed(
        r#"{"type":"message_delta","delta":{"stop_reason":"model_context_window_exceeded"}}"#,
    );
    let message = done(reducer.feed(r#"{"type":"message_stop"}"#));
    assert_eq!(message.stop_reason, StopReason::Length);
}

#[test]
fn glm_replays_unsigned_thinking() {
    let request = Request {
        messages: vec![std::sync::Arc::new(Message::Assistant(AssistantMessage {
            blocks: vec![ContentBlock::Thinking(ThinkingBlock::new("unsigned"))],
            usage: None,
            stop_reason: StopReason::Stop,
        }))],
        ..Request::default()
    };
    let glm = build_body(
        "glm-4.7",
        "https://open.bigmodel.cn/api/anthropic/v1/messages",
        &request,
    );
    assert_eq!(glm["messages"][0]["content"][0]["thinking"], "unsigned");
    let claude = build_body(
        "claude-sonnet-4-6",
        "https://api.anthropic.com/v1/messages",
        &request,
    );
    assert!(claude["messages"].as_array().unwrap().is_empty());
}
