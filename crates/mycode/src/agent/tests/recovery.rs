//! Failures preserve completed work and require an explicit new prompt to resume.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::core::{AssistantMessage, MycodeError, TextBlock, ToolCall};

struct RecoveringProvider {
    requests: Mutex<Vec<Vec<Arc<Message>>>>,
}

impl Provider for RecoveringProvider {
    fn stream<'a>(
        &'a self,
        request: &'a Request,
        cancel: CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let step = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request.messages.to_vec());
                requests.len()
            };
            let (sender, stream) = EventStream::channel(cancel);
            if step == 2 {
                sender
                    .send(StreamEvent::TextDelta("saved partial".into()))
                    .await;
                sender
                    .send(StreamEvent::Error(ProviderError::with_message(
                        ProviderErrorKind::Unavailable,
                        "connection reset",
                    )))
                    .await;
            } else {
                let (blocks, stop_reason) = if step == 1 {
                    (
                        vec![ContentBlock::ToolCall(ToolCall::new(
                            "write-once",
                            "count_write",
                            serde_json::json!({}),
                        ))],
                        StopReason::ToolUse,
                    )
                } else {
                    (
                        vec![ContentBlock::Text(TextBlock::new("resumed"))],
                        StopReason::Stop,
                    )
                };
                sender
                    .send(StreamEvent::Done {
                        message: AssistantMessage {
                            blocks,
                            usage: None,
                            stop_reason,
                        },
                    })
                    .await;
            }
            Ok(stream)
        })
    }
}

struct CountWrite(Arc<AtomicUsize>);

impl crate::tools::ToolDyn for CountWrite {
    fn spec(&self) -> crate::core::ToolSpec {
        crate::core::ToolSpec {
            name: "count_write".into(),
            description: "records a side effect".into(),
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
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(crate::tools::ToolResult::text("write committed"))
        })
    }
}

#[tokio::test]
async fn explicit_continuation_keeps_completed_tools_without_replaying_side_effects() {
    let provider = RecoveringProvider {
        requests: Mutex::new(Vec::new()),
    };
    let writes = Arc::new(AtomicUsize::new(0));
    let tools = ToolRegistry::new();
    tools.register(Arc::new(CountWrite(Arc::clone(&writes))));
    let hooks = HookRunner::new();
    let (events, mut receiver) = tokio::sync::broadcast::channel(64);
    let env = TurnEnv::new(&provider, &tools, &hooks).with_events(events);
    let mut agent = Agent::new(AgentConfig::new());
    assert!(matches!(
        agent
            .prompt(Message::User(UserMessage::text("make the change")), &env)
            .await,
        Err(MycodeError::Provider(_))
    ));
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        2,
        "failure must not restart the task"
    );
    let mut failed = false;
    while let Ok(event) = receiver.try_recv() {
        match event {
            crate::core::events::AgentEvent::Error(MycodeError::Provider(_)) => failed = true,
            crate::core::events::AgentEvent::TurnEnded(
                crate::core::events::TurnOutcome::Completed,
            ) => {
                panic!("provider failure was reported as completed")
            }
            _ => {}
        }
    }
    assert!(failed);

    // A fresh runtime is seeded exactly as reopening the durable ledger does.
    let mut resumed = Agent::new(AgentConfig::new());
    resumed.seed_history(agent.state().messages().iter().cloned());
    let continuation = Message::User(UserMessage::text("continue from saved progress"));
    let next_env = TurnEnv::new(&provider, &tools, &hooks);
    assert_eq!(
        resumed.prompt(continuation, &next_env).await.unwrap(),
        crate::core::events::TurnOutcome::Completed
    );
    assert_eq!(
        writes.load(Ordering::SeqCst),
        1,
        "the old write was replayed"
    );
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].iter().any(|message| matches!(
        message.as_ref(), Message::ToolResult(result) if result.tool_call_id == "write-once" && !result.is_error
    )));
    assert!(requests[2].iter().any(|message| matches!(
        message.as_ref(), Message::Assistant(assistant) if assistant.text().contains("saved partial")
    )));
}

#[tokio::test]
async fn an_unterminated_stream_is_a_failure_with_its_partial_preserved() {
    let provider = Scripted {
        events: vec![StreamEvent::TextDelta("before disconnect".into())],
    };
    let tools = ToolRegistry::new();
    let hooks = HookRunner::new();
    let env = TurnEnv::new(&provider, &tools, &hooks);
    let mut agent = Agent::new(AgentConfig::new());
    assert!(matches!(
        agent
            .prompt(Message::User(UserMessage::text("start")), &env)
            .await,
        Err(MycodeError::Provider(_))
    ));
    assert!(assistant_text(&agent).contains("before disconnect"));
    assert!(assistant_text(&agent).contains(crate::core::INTERRUPTED_RESPONSE_PREFIX));
}

#[tokio::test]
async fn an_error_terminal_never_dispatches_its_tool_calls() {
    let provider = Scripted {
        events: vec![StreamEvent::Done {
            message: AssistantMessage {
                blocks: vec![ContentBlock::ToolCall(ToolCall::new(
                    "bad",
                    "count_write",
                    serde_json::json!({}),
                ))],
                usage: None,
                stop_reason: StopReason::Error,
            },
        }],
    };
    let writes = Arc::new(AtomicUsize::new(0));
    let tools = ToolRegistry::new();
    tools.register(Arc::new(CountWrite(Arc::clone(&writes))));
    let hooks = HookRunner::new();
    let env = TurnEnv::new(&provider, &tools, &hooks);
    let mut agent = Agent::new(AgentConfig::new());
    assert!(matches!(
        agent
            .prompt(Message::User(UserMessage::text("start")), &env)
            .await,
        Err(MycodeError::Provider(_))
    ));
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    assert!(agent.state().messages().iter().any(|message| matches!(
        message.as_ref(), Message::ToolResult(result) if result.tool_call_id == "bad" && result.is_error
    )));
}

#[tokio::test]
async fn an_output_limit_preserves_partial_text_and_does_not_continue_automatically() {
    let provider = Scripted {
        events: vec![StreamEvent::Done {
            message: AssistantMessage {
                blocks: vec![ContentBlock::Text(TextBlock::new("unfinished answer"))],
                usage: None,
                stop_reason: StopReason::Length,
            },
        }],
    };
    let tools = ToolRegistry::new();
    let hooks = HookRunner::new();
    let steers = Arc::new(SteerInbox::new());
    steers.push("a queued follow-up");
    let (events, mut receiver) = tokio::sync::broadcast::channel(32);
    let env = TurnEnv::new(&provider, &tools, &hooks)
        .with_steer(Some(steers))
        .with_events(events);
    let mut agent = Agent::new(AgentConfig::new());
    assert!(matches!(
        agent.prompt(Message::User(UserMessage::text("start")), &env).await,
        Err(MycodeError::Provider(ref detail)) if detail.contains("output limit")
    ));
    assert_eq!(assistant_text(&agent), "unfinished answer");
    assert_eq!(
        agent.state().messages().len(),
        2,
        "no automatic continuation or steer replay"
    );
    let mut failure_events = 0;
    while let Ok(event) = receiver.try_recv() {
        match event {
            crate::core::events::AgentEvent::Error(MycodeError::Provider(_)) => failure_events += 1,
            crate::core::events::AgentEvent::TurnEnded(
                crate::core::events::TurnOutcome::Completed,
            ) => {
                panic!("truncated output was reported as completed")
            }
            _ => {}
        }
    }
    assert_eq!(failure_events, 1);
}
