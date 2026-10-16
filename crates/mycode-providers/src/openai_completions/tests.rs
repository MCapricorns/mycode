use mycode_core::{
    AssistantMessage, ContentBlock, Message, Request, StopReason, TextBlock, ThinkingBlock,
    UserMessage,
};

use super::CompletionsReducer;
use crate::driver::FrameReducer;
use crate::openai_completions::build_body;

fn text_of(message: &AssistantMessage) -> String {
    message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

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

fn take_done(events: Vec<mycode_core::StreamEvent>) -> AssistantMessage {
    events
        .into_iter()
        .find_map(|event| match event {
            mycode_core::StreamEvent::Done { message } => Some(message),
            _ => None,
        })
        .expect("done")
}

#[test]
fn minimax_xml_ask_user_arrives_as_a_typed_tool_call() {
    let mut reducer = CompletionsReducer::new();
    let xml = concat!(
        "<minimax:tool_call>",
        "<invoke name=\"ask_user\">",
        "<parameter name=\"questions\">",
        r#"[{"question":"Which?","choices":["red","blue"],"multiple":true}]"#,
        "</parameter>",
        "</invoke>",
        "</minimax:tool_call>",
    );
    let chunk = serde_json::json!({
        "choices": [{"delta": {"content": xml}, "finish_reason": "stop"}]
    });
    reducer.feed(&chunk.to_string());
    let message = take_done(reducer.feed("[DONE]"));
    let call = message.blocks.iter().find_map(|block| match block {
        ContentBlock::ToolCall(call) => Some(call),
        _ => None,
    });
    let call = call.expect("xml ask_user becomes a tool call");
    assert_eq!(call.name, "ask_user");
    assert_eq!(
        call.arguments["questions"][0]["choices"],
        serde_json::json!(["red", "blue"])
    );
    assert_eq!(call.arguments["questions"][0]["multiple"], true);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert!(text_of(&message).is_empty(), "{}", text_of(&message));
}

#[test]
fn glm_error_finish_keeps_thinking() {
    let mut reducer = CompletionsReducer::new();
    let deltas = reducer.feed(
        r#"{"choices":[{"delta":{"reasoning_content":"plan the page"},"finish_reason":null}]}"#,
    );
    assert!(deltas.iter().any(|event| matches!(
        event,
        mycode_core::StreamEvent::ThinkingDelta(text) if text == "plan the page"
    )));
    reducer
        .feed(r#"{"choices":[{"delta":{},"finish_reason":"error"}],"usage":{"prompt_tokens":3}}"#);
    let message = take_done(reducer.feed("[DONE]"));
    assert_eq!(thinking_of(&message), "plan the page");
    assert!(text_of(&message).contains("[error] the response was interrupted: error"));
    assert_eq!(message.stop_reason, StopReason::Error);
}

fn calls_of(message: &AssistantMessage) -> Vec<(String, String, serde_json::Value)> {
    message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => {
                Some((call.id.clone(), call.name.clone(), call.arguments.clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn parallel_calls_that_all_claim_index_zero_stay_separate() {
    let mut reducer = CompletionsReducer::new();
    for (id, path) in [("call_a", "a.rs"), ("call_b", "b.rs")] {
        let start = serde_json::json!({"choices": [{"delta": {"tool_calls": [{
            "index": 0, "id": id, "type": "function",
            "function": {"name": "read", "arguments": ""}
        }]}}]});
        reducer.feed(&start.to_string());
        let arguments = serde_json::json!({"path": path}).to_string();
        let (head, tail) = arguments.split_at(5);
        for piece in [head, tail] {
            let more = serde_json::json!({"choices": [{"delta": {"tool_calls": [{
                "index": 0, "function": {"arguments": piece}
            }]}}]});
            reducer.feed(&more.to_string());
        }
    }
    reducer.feed(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#);
    let message = take_done(reducer.feed("[DONE]"));
    let calls = calls_of(&message);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].0, "call_a");
    assert_eq!(calls[0].2["path"], "a.rs");
    assert_eq!(calls[1].0, "call_b");
    assert_eq!(calls[1].2["path"], "b.rs");
    assert_eq!(message.stop_reason, StopReason::ToolUse);
}

#[test]
fn a_name_resent_on_every_fragment_is_not_repeated() {
    let mut reducer = CompletionsReducer::new();
    for piece in [r#"{"path":"#, r#""a.rs"}"#] {
        let chunk = serde_json::json!({"choices": [{"delta": {"tool_calls": [{
            "index": 0, "id": "call_a",
            "function": {"name": "read", "arguments": piece}
        }]}}]});
        reducer.feed(&chunk.to_string());
    }
    // A name split across fragments still joins.
    for piece in ["gr", "ep"] {
        let chunk = serde_json::json!({"choices": [{"delta": {"tool_calls": [{
            "index": 1, "id": "call_b",
            "function": {"name": piece, "arguments": ""}
        }]}}]});
        reducer.feed(&chunk.to_string());
    }
    reducer.feed(r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#);
    let message = take_done(reducer.feed("[DONE]"));
    let calls = calls_of(&message);
    assert_eq!(calls[0].1, "read");
    assert_eq!(calls[0].2["path"], "a.rs");
    assert_eq!(calls[1].1, "grep");
}

#[test]
fn a_native_call_after_an_xml_call_keeps_its_own_arguments() {
    let mut reducer = CompletionsReducer::new();
    let xml = concat!(
        "<minimax:tool_call>",
        "<invoke name=\"find\">",
        "<parameter name=\"pattern\">*.rs</parameter>",
        "</invoke>",
        "</minimax:tool_call>",
    );
    reducer.feed(&serde_json::json!({"choices": [{"delta": {"content": xml}}]}).to_string());
    let native = serde_json::json!({"choices": [{"delta": {"tool_calls": [{
            "index": 0, "id": "call_native",
            "function": {"name": "read", "arguments": "{\"path\":\"a.rs\"}"}
        }]}, "finish_reason": "tool_calls"}]});
    reducer.feed(&native.to_string());
    let message = take_done(reducer.feed("[DONE]"));
    let calls = calls_of(&message);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].1, "find");
    assert_eq!(calls[0].2["pattern"], "*.rs");
    assert_eq!(calls[1].0, "call_native");
    assert_eq!(calls[1].1, "read");
    assert_eq!(calls[1].2["path"], "a.rs");
}

#[test]
fn thinking_only_stop_is_success() {
    let mut reducer = CompletionsReducer::new();
    reducer.feed(r#"{"choices":[{"delta":{"reasoning_content":"only thought"}}]}"#);
    reducer.feed(r#"{"choices":[{"finish_reason":"stop"}]}"#);
    let message = take_done(reducer.feed("[DONE]"));
    assert_eq!(thinking_of(&message), "only thought");
    assert!(text_of(&message).is_empty());
    assert_eq!(message.stop_reason, StopReason::Stop);
}

#[test]
fn usage_only_and_blank_frames_do_not_wipe() {
    let mut reducer = CompletionsReducer::new();
    reducer.feed(r#"{"choices":[{"delta":{"reasoning_content":"kept"}}]}"#);
    assert!(reducer.feed("").is_empty());
    assert!(reducer.feed("   ").is_empty());
    assert!(
        reducer
            .feed(r#"{"choices":[],"usage":{"completion_tokens":1}}"#)
            .is_empty()
    );
    let message = take_done(reducer.feed("[DONE]"));
    assert_eq!(thinking_of(&message), "kept");
}

#[test]
fn invalid_json_after_thinking_assembles_the_partial() {
    let mut reducer = CompletionsReducer::new();
    reducer.feed(r#"{"choices":[{"delta":{"content":"hello"}}]}"#);
    let message = take_done(reducer.feed("not-json"));
    assert!(text_of(&message).contains("hello"));
    assert!(text_of(&message).contains("invalid completions frame"));
    assert_eq!(message.stop_reason, StopReason::Error);
}

#[test]
fn eof_without_finish_reason_notes_the_partial() {
    let mut reducer = CompletionsReducer::new();
    reducer.feed(r#"{"choices":[{"delta":{"reasoning_content":"mid"}}]}"#);
    let mycode_core::StreamEvent::Done { message } = reducer.finish() else {
        panic!("done");
    };
    assert_eq!(thinking_of(&message), "mid");
    assert!(text_of(&message).contains("finish reason"));
    assert_eq!(message.stop_reason, StopReason::Error);
}

#[test]
fn provider_error_object_is_a_visible_message() {
    let mut reducer = CompletionsReducer::new();
    reducer.feed(r#"{"choices":[{"delta":{"reasoning_content":"before"}}]}"#);
    let message = take_done(reducer.feed(r#"{"error":{"message":"rate limit"}}"#));
    assert_eq!(thinking_of(&message), "before");
    assert!(text_of(&message).contains("rate limit"));
}

#[test]
fn glm_replays_reasoning_content_including_thinking_only() {
    let request = Request {
        messages: vec![
            std::sync::Arc::new(Message::User(UserMessage::text("go"))),
            std::sync::Arc::new(Message::Assistant(AssistantMessage {
                blocks: vec![ContentBlock::Thinking(ThinkingBlock::new("hidden"))],
                usage: None,
                stop_reason: StopReason::Stop,
            })),
        ],
        ..Request::default()
    };
    let glm = build_body("glm-4.7", "https://open.bigmodel.cn/api/paas/v4", &request);
    let replayed = &glm["messages"][1];
    assert_eq!(replayed["reasoning_content"], "hidden");
    assert_eq!(replayed["content"], "");

    let openai = build_body("gpt-5", "https://api.openai.com/v1", &request);
    assert_eq!(openai["messages"].as_array().unwrap().len(), 1);

    let minimax = build_body(
        "MiniMax-M2.5",
        "https://api.minimaxi.com/v1",
        &Request {
            messages: vec![std::sync::Arc::new(Message::Assistant(AssistantMessage {
                blocks: vec![
                    ContentBlock::Thinking(ThinkingBlock::new("why")),
                    ContentBlock::Text(TextBlock::new("answer")),
                ],
                usage: None,
                stop_reason: StopReason::Stop,
            }))],
            ..Request::default()
        },
    );
    assert_eq!(minimax["messages"][0]["content"], "answer");
    assert!(minimax["messages"][0].get("reasoning_content").is_none());
    assert_eq!(
        minimax["messages"][0]["reasoning_details"],
        serde_json::json!([{ "type": "reasoning.text", "text": "why" }])
    );

    let minimax_thinking_only = build_body(
        "MiniMax-M3",
        "https://api.minimaxi.com/v1",
        &Request {
            messages: vec![std::sync::Arc::new(Message::Assistant(AssistantMessage {
                blocks: vec![ContentBlock::Thinking(ThinkingBlock::new("why"))],
                usage: None,
                stop_reason: StopReason::Stop,
            }))],
            ..Request::default()
        },
    );
    assert_eq!(minimax_thinking_only["messages"][0]["content"], "");
    assert_eq!(
        minimax_thinking_only["messages"][0]["reasoning_details"][0]["text"],
        "why"
    );
}

#[test]
fn minimax_replays_reasoning_details_verbatim_and_stays_stable() {
    let details = serde_json::json!([
        {
            "type": "reasoning.text",
            "id": "reasoning-text-1",
            "format": "MiniMax-response-v1",
            "index": 0,
            "text": "plan the edit exactly"
        }
    ]);
    let replay = serde_json::json!({"reasoning_details": details}).to_string();
    let request = Request {
        messages: vec![std::sync::Arc::new(Message::Assistant(AssistantMessage {
            blocks: vec![
                ContentBlock::Thinking(
                    ThinkingBlock::new("plan the edit exactly").with_replay(replay),
                ),
                ContentBlock::Text(TextBlock::new("visible answer")),
            ],
            usage: None,
            stop_reason: StopReason::Stop,
        }))],
        reasoning: Some(mycode_core::ReasoningLevel::Max),
        ..Request::default()
    };
    let endpoint = "https://api.minimaxi.com/v1/chat/completions";
    let first = build_body("MiniMax-M3", endpoint, &request);
    let second = build_body("MiniMax-M3", endpoint, &request);
    assert_eq!(first["thinking"]["type"], "adaptive");
    assert_eq!(first["reasoning_split"], true);
    assert!(first.get("reasoning_effort").is_none());
    assert_eq!(first["messages"][0]["content"], "visible answer");
    assert_eq!(first["messages"][0]["reasoning_details"], details);
    assert!(first["messages"][0].get("reasoning_content").is_none());
    assert_eq!(first, second);
}

#[test]
fn minimax_reasoning_content_replays_as_content_not_details() {
    let request = Request {
        messages: vec![std::sync::Arc::new(Message::Assistant(AssistantMessage {
            blocks: vec![
                ContentBlock::Thinking(
                    ThinkingBlock::new("hidden").with_replay(r#"{"reasoning_content":true}"#),
                ),
                ContentBlock::Text(TextBlock::new("answer")),
            ],
            usage: None,
            stop_reason: StopReason::Stop,
        }))],
        ..Request::default()
    };
    let body = build_body("MiniMax-M3", "https://api.minimax.io/v1", &request);
    assert_eq!(body["messages"][0]["reasoning_content"], "hidden");
    assert!(body["messages"][0].get("reasoning_details").is_none());
    assert_eq!(body["messages"][0]["content"], "answer");
}

#[test]
fn leading_think_tag_becomes_a_thinking_block() {
    let mut reducer = CompletionsReducer::new();
    let deltas = reducer
        .feed(r#"{"choices":[{"delta":{"content":"<think>plan the page</think>\nhello"}}]}"#);
    assert!(deltas.iter().any(|event| matches!(
        event,
        mycode_core::StreamEvent::ThinkingDelta(text) if text == "plan the page"
    )));
    assert!(deltas.iter().any(|event| matches!(
        event,
        mycode_core::StreamEvent::TextDelta(text) if text == "hello"
    )));
    let message = take_done(reducer.feed("[DONE]"));
    assert_eq!(thinking_of(&message), "plan the page");
    assert_eq!(text_of(&message), "hello");
    let replay = message.blocks.iter().find_map(|block| match block {
        ContentBlock::Thinking(thinking) => thinking.replay.clone(),
        _ => None,
    });
    assert!(replay.is_none(), "a parsed tag has no vendor blob");
}

#[test]
fn think_tag_split_across_chunks_stays_out_of_the_reply() {
    let mut reducer = CompletionsReducer::new();
    assert!(
        reducer
            .feed(r#"{"choices":[{"delta":{"content":"<thi"}}]}"#)
            .iter()
            .all(|event| !matches!(event, mycode_core::StreamEvent::TextDelta(_)))
    );
    reducer.feed(r#"{"choices":[{"delta":{"content":"nk>plan</thi"}}]}"#);
    reducer.feed(r#"{"choices":[{"delta":{"content":"nk>\nanswer"}}]}"#);
    let message = take_done(reducer.feed("[DONE]"));
    assert_eq!(thinking_of(&message), "plan");
    assert_eq!(text_of(&message), "answer");
    assert!(!text_of(&message).contains("<think>"));
}

#[test]
fn plain_text_is_not_held_for_a_think_tag() {
    let mut reducer = CompletionsReducer::new();
    let deltas = reducer.feed(r#"{"choices":[{"delta":{"content":"hello"}}]}"#);
    assert!(deltas.iter().any(|event| matches!(
        event,
        mycode_core::StreamEvent::TextDelta(text) if text == "hello"
    )));
}

#[test]
fn minimax_reasoning_details_map_to_a_thinking_block() {
    let mut reducer = CompletionsReducer::new();
    let chunk = serde_json::json!({
        "choices": [{
            "delta": {
                "reasoning_details": [{
                    "type": "reasoning.text",
                    "id": "reasoning-text-1",
                    "format": "MiniMax-response-v1",
                    "index": 0,
                    "text": "plan the edit exactly"
                }],
                "content": "<think>duplicate</think>\nvisible answer"
            },
            "finish_reason": "stop"
        }]
    });
    let deltas = reducer.feed(&chunk.to_string());
    assert!(deltas.iter().any(|event| matches!(
        event,
        mycode_core::StreamEvent::ThinkingDelta(text) if text == "plan the edit exactly"
    )));
    assert!(deltas.iter().all(|event| !matches!(
        event,
        mycode_core::StreamEvent::ThinkingDelta(text) if text.contains("duplicate")
    )));
    let message = take_done(reducer.feed("[DONE]"));
    assert_eq!(thinking_of(&message), "plan the edit exactly");
    assert_eq!(text_of(&message), "visible answer");
    let replay = message.blocks.iter().find_map(|block| match block {
        ContentBlock::Thinking(thinking) => thinking.replay.clone(),
        _ => None,
    });
    let replay = replay.expect("details blob");
    let request = Request {
        messages: vec![std::sync::Arc::new(Message::Assistant(message))],
        ..Request::default()
    };
    let body = build_body("MiniMax-M3", "https://api.minimax.io/v1", &request);
    let echoed = &body["messages"][0]["reasoning_details"];
    assert_eq!(echoed[0]["id"], "reasoning-text-1");
    assert_eq!(echoed[0]["format"], "MiniMax-response-v1");
    assert_eq!(echoed[0]["text"], "plan the edit exactly");
    assert_eq!(body["messages"][0]["content"], "visible answer");
    let again = build_body("MiniMax-M3", "https://api.minimax.io/v1", &request);
    assert_eq!(again["messages"][0]["reasoning_details"], *echoed);
    assert!(replay.contains("reasoning-text-1"));
}

#[test]
fn reasoning_details_fragments_with_the_same_id_concatenate() {
    let mut reducer = CompletionsReducer::new();
    reducer.feed(
            r#"{"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","id":"reasoning-text-1","index":0,"text":"plan"}]}}]}"#,
        );
    reducer.feed(
            r#"{"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","id":"reasoning-text-1","index":0,"text":" the edit"}]}}]}"#,
        );
    reducer.feed(r#"{"choices":[{"delta":{"content":"answer"},"finish_reason":"stop"}]}"#);
    let message = take_done(reducer.feed("[DONE]"));
    assert_eq!(thinking_of(&message), "plan the edit");
    assert_eq!(text_of(&message), "answer");
}

#[test]
fn distinct_detail_items_are_charged_against_the_stream_cap() {
    // Frames that keep appending new detail items (no shared prefix, so
    // the cumulative-merge paths do not apply) must count their retained
    // bytes toward the accumulation ceiling instead of growing the
    // array without bound.
    let mut reducer = CompletionsReducer::new();
    let filler = "x".repeat(1_050_000);
    let mut capped = false;
    for index in 0..9_u32 {
        let marker = char::from_u32('z' as u32 - index).expect("marker");
        let frame = serde_json::json!({
            "choices": [{
                "delta": {
                    "reasoning_details": [{
                        "type": "reasoning.text",
                        "id": format!("reasoning-text-{index}"),
                        "format": filler,
                        "text": marker.to_string(),
                    }]
                }
            }]
        });
        let events = reducer.feed(&frame.to_string());
        if events.iter().any(|event| {
            matches!(
                event,
                mycode_core::StreamEvent::Error(error) if error
                    .message()
                    .is_some_and(|message| message.contains("stream exceeded the output limit"))
            )
        }) {
            capped = true;
            break;
        }
    }
    assert!(
        capped,
        "distinct-id detail frames must hit the accumulation cap"
    );
}
