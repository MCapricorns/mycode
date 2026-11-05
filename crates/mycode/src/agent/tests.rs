use crate::core::{
    ContentBlock, EventStream, Message, Provider, ProviderError, ProviderErrorKind, Request,
    StopReason, StreamEvent, UserMessage,
};
use crate::tools::ToolRegistry;
use tokio_util::sync::CancellationToken;

use super::{Agent, AgentConfig};
use crate::agent::env::{SteerInbox, TurnEnv};
use crate::agent::hooks::HookRunner;

struct Scripted {
    events: Vec<StreamEvent>,
}

impl Provider for Scripted {
    fn stream<'a>(
        &'a self,
        _request: &'a Request,
        cancel: CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let (sender, stream) = EventStream::channel(cancel);
            for event in self.events.clone() {
                sender.send(event).await;
            }
            Ok(stream)
        })
    }
}

fn assistant_text(agent: &Agent) -> String {
    agent
        .state()
        .messages()
        .iter()
        .rev()
        .find_map(|message| match message.as_ref() {
            Message::Assistant(assistant) => Some(assistant.text()),
            _ => None,
        })
        .unwrap_or_default()
}

fn assistant_thinking(agent: &Agent) -> String {
    agent
        .state()
        .messages()
        .iter()
        .rev()
        .find_map(|message| match message.as_ref() {
            Message::Assistant(assistant) => Some(
                assistant
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Thinking(thinking) => Some(thinking.text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn provider_error_keeps_streamed_thinking() {
    let provider = Scripted {
        events: vec![
            StreamEvent::ThinkingDelta("plan the edit".into()),
            StreamEvent::TextDelta("partial".into()),
            StreamEvent::Error(ProviderError::with_message(
                ProviderErrorKind::Unavailable,
                "connection reset",
            )),
        ],
    };
    let tools = ToolRegistry::new();
    let hooks = HookRunner::new();
    let env = TurnEnv::new(&provider, &tools, &hooks);
    let mut agent = Agent::new(AgentConfig::new());
    let outcome = agent
        .prompt(Message::User(UserMessage::text("开始写")), &env)
        .await
        .expect("completed turn");
    assert!(matches!(
        outcome,
        crate::core::events::TurnOutcome::Completed
    ));
    assert_eq!(assistant_thinking(&agent), "plan the edit");
    let text = assistant_text(&agent);
    assert!(text.contains("partial"));
    assert!(text.contains("[error] the response was interrupted:"));
    assert!(text.contains("connection reset"));
    let stop = agent
        .state()
        .messages()
        .iter()
        .rev()
        .find_map(|message| match message.as_ref() {
            Message::Assistant(assistant) => Some(assistant.stop_reason),
            _ => None,
        });
    assert_eq!(stop, Some(StopReason::Error));
}

#[tokio::test]
async fn cancel_keeps_the_partial_and_marks_it_interrupted() {
    // A user cancel keeps the partial and the interruption line; the turn
    // outcome stays aborted. An unnamed tool-call delta is not executed and
    // does not enter history.
    let provider = Scripted {
        events: vec![
            StreamEvent::ThinkingDelta("discard me".into()),
            StreamEvent::TextDelta("keep me".into()),
            StreamEvent::ToolCallDelta {
                id: "call-partial".into(),
                partial_json: "{\"path\":".into(),
            },
            StreamEvent::Error(ProviderError::new(ProviderErrorKind::Cancelled)),
        ],
    };
    let tools = ToolRegistry::new();
    let hooks = HookRunner::new();
    let env = TurnEnv::new(&provider, &tools, &hooks);
    let mut agent = Agent::new(AgentConfig::new());
    let outcome = agent
        .prompt(Message::User(UserMessage::text("开始写")), &env)
        .await
        .expect("aborted turn");
    assert!(matches!(outcome, crate::core::events::TurnOutcome::Aborted));
    assert_eq!(assistant_thinking(&agent), "discard me");
    let text = assistant_text(&agent);
    assert!(text.contains("keep me"), "{text}");
    assert!(
        text.contains("[error] the response was interrupted: interrupted by user"),
        "{text}"
    );
    assert!(
        agent
            .state()
            .messages()
            .iter()
            .all(|message| match message.as_ref() {
                Message::Assistant(assistant) => !assistant
                    .blocks
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ToolCall(_))),
                Message::ToolResult(_) => false,
                Message::User(_) => true,
            }),
        "an incomplete tool call was kept or executed"
    );
}

struct StepProvider {
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl Provider for StepProvider {
    fn stream<'a>(
        &'a self,
        request: &'a Request,
        cancel: CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let rendered = request
                .messages
                .iter()
                .map(|message| match message.as_ref() {
                    Message::User(user) => user
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                    Message::Assistant(assistant) => assistant.text(),
                    Message::ToolResult(result) => result
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect(),
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.seen.lock().expect("seen").push(rendered);
            let step = self.seen.lock().expect("seen").len();
            let (sender, stream) = EventStream::channel(cancel);
            if step == 1 {
                sender
                    .send(StreamEvent::Done {
                        message: crate::core::AssistantMessage {
                            blocks: vec![ContentBlock::ToolCall(crate::core::ToolCall::new(
                                "call-note",
                                "hang",
                                serde_json::json!({}),
                            ))],
                            usage: None,
                            stop_reason: StopReason::ToolUse,
                        },
                    })
                    .await;
            } else {
                sender
                    .send(StreamEvent::Done {
                        message: crate::core::AssistantMessage {
                            blocks: vec![ContentBlock::Text(crate::core::TextBlock::new(
                                "second step",
                            ))],
                            usage: None,
                            stop_reason: StopReason::Stop,
                        },
                    })
                    .await;
            }
            Ok(stream)
        })
    }
}

struct NoteTool {
    inbox: std::sync::Arc<SteerInbox>,
}

impl crate::tools::ToolDyn for NoteTool {
    fn spec(&self) -> crate::core::ToolSpec {
        crate::core::ToolSpec {
            name: "hang".to_owned(),
            description: "records a note".to_owned(),
            params_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn execute_dyn<'a>(
        &'a self,
        _args: serde_json::Value,
        _ctx: &'a crate::tools::ToolCtx,
        _out: &'a mut crate::tools::ToolStream,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<crate::tools::ToolResult, crate::tools::ToolError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.inbox.push("STEER_KEEP_GOING");
            Ok(crate::tools::ToolResult::text("noted"))
        })
    }
}

mod dispatch;
