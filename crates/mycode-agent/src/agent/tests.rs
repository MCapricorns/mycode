use mycode_tools::ToolRegistry;
use mycode_tools::{
    ContentBlock, EventStream, Message, Provider, ProviderError, ProviderErrorKind, Request,
    StopReason, StreamEvent, UserMessage,
};
use tokio_util::sync::CancellationToken;

use super::{Agent, AgentConfig};
use crate::env::{SteerInbox, TurnEnv};
use crate::hooks::HookRunner;

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
        mycode_tools::events::TurnOutcome::Completed
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
    assert!(matches!(
        outcome,
        mycode_tools::events::TurnOutcome::Aborted
    ));
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

struct CountingProvider {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Provider for CountingProvider {
    fn stream<'a>(
        &'a self,
        _request: &'a Request,
        cancel: CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (sender, stream) = EventStream::channel(cancel);
            sender
                .send(StreamEvent::Error(ProviderError::with_message(
                    ProviderErrorKind::Unavailable,
                    "should not stream",
                )))
                .await;
            Ok(stream)
        })
    }
}

#[tokio::test]
async fn validation_failure_does_not_end_a_turn_that_never_started() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider = CountingProvider {
        calls: std::sync::Arc::clone(&calls),
    };
    let tools = ToolRegistry::new();
    let hooks = HookRunner::new();
    let env = TurnEnv::new(&provider, &tools, &hooks);
    let mut events = env.events.subscribe();
    let mut agent = Agent::new(AgentConfig::new());
    let prompt = "x".repeat(mycode_tools::MAX_REQUEST_ENCODED_BYTES);
    let error = agent
        .prompt(Message::User(UserMessage::text(prompt)), &env)
        .await
        .expect_err("oversized request");
    assert!(error.to_string().contains("encoded size") || !error.to_string().is_empty());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(agent.state().messages().is_empty());
    let mut saw_started = false;
    let mut saw_ended = false;
    while let Ok(event) = events.try_recv() {
        match event {
            mycode_tools::events::AgentEvent::TurnStarted => saw_started = true,
            mycode_tools::events::AgentEvent::TurnEnded(_) => saw_ended = true,
            _ => {}
        }
    }
    assert!(!saw_started);
    assert!(!saw_ended);
}

struct HangAgent {
    started: std::sync::Arc<std::sync::atomic::AtomicBool>,
    release: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl mycode_tools::ToolDyn for HangAgent {
    fn spec(&self) -> mycode_tools::ToolSpec {
        mycode_tools::ToolSpec {
            name: "hang".to_owned(),
            description: "nested test agent".to_owned(),
            params_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn execute_dyn<'a>(
        &'a self,
        _args: serde_json::Value,
        _ctx: &'a mycode_tools::ToolCtx,
        _out: &'a mut mycode_tools::ToolStream,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<mycode_tools::ToolResult, mycode_tools::ToolError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.started
                .store(true, std::sync::atomic::Ordering::SeqCst);
            while !self.release.load(std::sync::atomic::Ordering::SeqCst) {
                if _ctx.cancel.is_cancelled() {
                    return Err(mycode_tools::ToolError::Execution("cancelled".to_owned()));
                }
                tokio::task::yield_now().await;
            }
            Ok(mycode_tools::ToolResult::text(
                "NESTED_ANSWER_SHOULD_NOT_APPEND",
            ))
        })
    }
}

fn history_text(agent: &Agent) -> String {
    agent
        .state()
        .messages()
        .iter()
        .map(|message| match message.as_ref() {
            Message::ToolResult(result) => result
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
            Message::Assistant(assistant) => assistant.text(),
            Message::User(user) => user
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn cancelled_nested_agent_result_does_not_enter_history() {
    let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&started);
    let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let release_for_tool = std::sync::Arc::clone(&release);
    let cancel = CancellationToken::new();
    let cancel_for_task = cancel.clone();
    let handle = tokio::spawn(async move {
        let provider = Scripted {
            events: vec![StreamEvent::Done {
                message: mycode_tools::AssistantMessage {
                    blocks: vec![ContentBlock::ToolCall(mycode_tools::ToolCall::new(
                        "call-agent",
                        "hang",
                        serde_json::json!({}),
                    ))],
                    usage: None,
                    stop_reason: StopReason::ToolUse,
                },
            }],
        };
        let tools = ToolRegistry::new();
        tools.register(std::sync::Arc::new(HangAgent {
            started: flag,
            release: release_for_tool,
        }));
        let hooks = HookRunner::new();
        let env = TurnEnv::new(&provider, &tools, &hooks).with_cancel(cancel_for_task);
        let mut agent = Agent::new(AgentConfig::new());
        let outcome = agent
            .prompt(Message::User(UserMessage::text("delegate")), &env)
            .await;
        (outcome, history_text(&agent))
    });
    let started_in_time = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !started.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(started_in_time.is_ok(), "nested agent never started");
    cancel.cancel();
    let (outcome, history) = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("nested agent kept running after cancel")
        .expect("task");
    assert!(matches!(
        outcome.expect("aborted"),
        mycode_tools::events::TurnOutcome::Aborted
    ));
    assert!(
        !history.contains("NESTED_ANSWER_SHOULD_NOT_APPEND"),
        "{history}"
    );
    assert!(
        history.contains("cancelled") || history.contains("discarded"),
        "{history}"
    );
    release.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// `agent` children: a call with `wait` runs until the turn is cancelled,
/// any other call answers at once.
struct SplitAgents;

impl mycode_tools::ToolDyn for SplitAgents {
    fn spec(&self) -> mycode_tools::ToolSpec {
        mycode_tools::ToolSpec {
            name: "agent".to_owned(),
            description: "nested test agents".to_owned(),
            params_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn execute_dyn<'a>(
        &'a self,
        args: serde_json::Value,
        ctx: &'a mycode_tools::ToolCtx,
        _out: &'a mut mycode_tools::ToolStream,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<mycode_tools::ToolResult, mycode_tools::ToolError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            if args.get("wait").and_then(serde_json::Value::as_bool) == Some(true) {
                ctx.cancel.cancelled().await;
                return Err(mycode_tools::ToolError::Execution("cancelled".to_owned()));
            }
            Ok(mycode_tools::ToolResult::text("FAST_ANSWER"))
        })
    }
}

/// A child that already answered keeps its answer when the turn is stopped
/// under a sibling, and every call completes exactly once: the ledger pump
/// closes a call per completion, and a second one fails the turn.
#[tokio::test]
async fn cancelling_parallel_agents_completes_each_call_once() {
    use mycode_tools::events::{AgentEvent, TurnOutcome};
    use std::time::Duration;

    let (events, mut bus) = tokio::sync::broadcast::channel(256);
    let cancel = CancellationToken::new();
    let cancel_for_task = cancel.clone();
    let handle = tokio::spawn(async move {
        let call = |id: &str, args: serde_json::Value| {
            ContentBlock::ToolCall(mycode_tools::ToolCall::new(id, "agent", args))
        };
        let provider = Scripted {
            events: vec![StreamEvent::Done {
                message: mycode_tools::AssistantMessage {
                    blocks: vec![
                        call("call-fast", serde_json::json!({})),
                        call("call-slow", serde_json::json!({"wait": true})),
                    ],
                    usage: None,
                    stop_reason: StopReason::ToolUse,
                },
            }],
        };
        let tools = ToolRegistry::new();
        tools.register(std::sync::Arc::new(SplitAgents));
        let hooks = HookRunner::new();
        let env = TurnEnv::new(&provider, &tools, &hooks)
            .with_cancel(cancel_for_task)
            .with_events(events);
        let mut agent = Agent::new(AgentConfig::new());
        let outcome = agent
            .prompt(Message::User(UserMessage::text("delegate")), &env)
            .await;
        (outcome, history_text(&agent))
    });
    let mut completed = Vec::new();
    let mut cancelled = false;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), bus.recv())
            .await
            .expect("the turn stalled");
        match event {
            Ok(AgentEvent::ToolCompleted { call_id, .. }) => {
                completed.push(call_id.to_string());
                // Stop the turn once the fast child answered.
                if !cancelled && call_id.as_str() == "call-fast" {
                    cancelled = true;
                    cancel.cancel();
                }
            }
            Ok(AgentEvent::TurnEnded(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let (outcome, history) = handle.await.expect("task");
    assert!(matches!(outcome.expect("aborted"), TurnOutcome::Aborted));
    for id in ["call-fast", "call-slow"] {
        assert_eq!(
            completed.iter().filter(|done| *done == id).count(),
            1,
            "{id} must complete once: {completed:?}"
        );
    }
    assert!(history.contains("FAST_ANSWER"), "{history}");
    assert!(history.contains("discarded"), "{history}");
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
                        message: mycode_tools::AssistantMessage {
                            blocks: vec![ContentBlock::ToolCall(mycode_tools::ToolCall::new(
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
                        message: mycode_tools::AssistantMessage {
                            blocks: vec![ContentBlock::Text(mycode_tools::TextBlock::new(
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

impl mycode_tools::ToolDyn for NoteTool {
    fn spec(&self) -> mycode_tools::ToolSpec {
        mycode_tools::ToolSpec {
            name: "hang".to_owned(),
            description: "records a note".to_owned(),
            params_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn execute_dyn<'a>(
        &'a self,
        _args: serde_json::Value,
        _ctx: &'a mycode_tools::ToolCtx,
        _out: &'a mut mycode_tools::ToolStream,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<mycode_tools::ToolResult, mycode_tools::ToolError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.inbox.push("STEER_KEEP_GOING");
            Ok(mycode_tools::ToolResult::text("noted"))
        })
    }
}

mod dispatch;
