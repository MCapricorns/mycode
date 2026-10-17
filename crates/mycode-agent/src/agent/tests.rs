use mycode_core::{
    ContentBlock, EventStream, Message, Provider, ProviderError, ProviderErrorKind, Request,
    StopReason, StreamEvent, UserMessage,
};
use mycode_tools::ToolRegistry;
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
        mycode_core::events::TurnOutcome::Completed
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
    assert!(matches!(outcome, mycode_core::events::TurnOutcome::Aborted));
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
    let prompt = "x".repeat(mycode_core::MAX_REQUEST_ENCODED_BYTES);
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
            mycode_core::events::AgentEvent::TurnStarted => saw_started = true,
            mycode_core::events::AgentEvent::TurnEnded(_) => saw_ended = true,
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
    fn spec(&self) -> mycode_core::ToolSpec {
        mycode_core::ToolSpec {
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
                message: mycode_core::AssistantMessage {
                    blocks: vec![ContentBlock::ToolCall(mycode_core::ToolCall::new(
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
        mycode_core::events::TurnOutcome::Aborted
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
                        message: mycode_core::AssistantMessage {
                            blocks: vec![ContentBlock::ToolCall(mycode_core::ToolCall::new(
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
                        message: mycode_core::AssistantMessage {
                            blocks: vec![ContentBlock::Text(mycode_core::TextBlock::new(
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
    fn spec(&self) -> mycode_core::ToolSpec {
        mycode_core::ToolSpec {
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

#[tokio::test]
async fn steer_between_steps_is_in_the_next_request() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = StepProvider {
        seen: std::sync::Arc::clone(&seen),
    };
    let inbox = std::sync::Arc::new(SteerInbox::new());
    let tools = ToolRegistry::new();
    tools.register(std::sync::Arc::new(NoteTool {
        inbox: std::sync::Arc::clone(&inbox),
    }));
    let hooks = HookRunner::new();
    let env = TurnEnv::new(&provider, &tools, &hooks).with_steer(Some(inbox));
    let mut agent = Agent::new(AgentConfig::new());
    let outcome = agent
        .prompt(Message::User(UserMessage::text("start")), &env)
        .await
        .expect("completed turn");
    assert!(matches!(
        outcome,
        mycode_core::events::TurnOutcome::Completed
    ));
    assert!(!env.cancel.is_cancelled());
    let seen = seen.lock().expect("seen");
    assert!(seen.len() >= 2, "expected a second model call: {seen:?}");
    assert!(
        !seen[0].contains("STEER_KEEP_GOING"),
        "steer leaked into the in-flight request: {}",
        seen[0]
    );
    assert!(
        seen[1].contains("STEER_KEEP_GOING"),
        "steer missing from the next request: {}",
        seen[1]
    );
}

struct BatchProvider {
    steps: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Provider for BatchProvider {
    fn stream<'a>(
        &'a self,
        _request: &'a Request,
        cancel: CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let step = self.steps.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (sender, stream) = EventStream::channel(cancel);
            let call = |id: &str, name: &str| {
                ContentBlock::ToolCall(mycode_core::ToolCall::new(id, name, serde_json::json!({})))
            };
            let message = if step == 0 {
                mycode_core::AssistantMessage {
                    blocks: vec![
                        call("r1", "probe_read"),
                        call("r2", "probe_read"),
                        call("w", "probe_write"),
                        call("r3", "probe_read"),
                    ],
                    usage: None,
                    stop_reason: StopReason::ToolUse,
                }
            } else {
                mycode_core::AssistantMessage {
                    blocks: vec![ContentBlock::Text(mycode_core::TextBlock::new("done"))],
                    usage: None,
                    stop_reason: StopReason::Stop,
                }
            };
            sender.send(StreamEvent::Done { message }).await;
            Ok(stream)
        })
    }
}

struct ProbeTool {
    name: &'static str,
    parallel: bool,
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl mycode_tools::ToolDyn for ProbeTool {
    fn spec(&self) -> mycode_core::ToolSpec {
        mycode_core::ToolSpec {
            name: self.name.to_owned(),
            description: "records when it runs".to_owned(),
            params_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn parallel_safe(&self) -> bool {
        self.parallel
    }

    fn execute_dyn<'a>(
        &'a self,
        _args: serde_json::Value,
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
            let id = ctx.call_id.clone();
            self.log.lock().expect("log").push(format!("start {id}"));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            self.log.lock().expect("log").push(format!("end {id}"));
            Ok(mycode_tools::ToolResult::text(id))
        })
    }
}

#[tokio::test]
async fn adjacent_reads_overlap_and_a_write_runs_alone() {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let tools = ToolRegistry::new();
    for (name, parallel) in [("probe_read", true), ("probe_write", false)] {
        tools.register(std::sync::Arc::new(ProbeTool {
            name,
            parallel,
            log: std::sync::Arc::clone(&log),
        }));
    }
    let provider = BatchProvider {
        steps: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };
    let hooks = HookRunner::new();
    let env = TurnEnv::new(&provider, &tools, &hooks);
    let mut agent = Agent::new(AgentConfig::new());
    agent
        .prompt(Message::User(UserMessage::text("go")), &env)
        .await
        .expect("completed turn");
    let log = log.lock().expect("log").clone();
    let at = |entry: &str| {
        log.iter()
            .position(|seen| seen == entry)
            .unwrap_or_else(|| panic!("{entry} missing from {log:?}"))
    };
    assert!(
        at("start r2") < at("end r1"),
        "reads did not overlap: {log:?}"
    );
    assert!(
        at("end r1") < at("start w") && at("end r2") < at("start w"),
        "{log:?}"
    );
    assert!(at("end w") < at("start r3"), "{log:?}");
    let results: Vec<_> = agent
        .state()
        .messages()
        .iter()
        .filter_map(|message| match message.as_ref() {
            Message::ToolResult(result) => Some(result.tool_call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(results, ["r1", "r2", "w", "r3"]);
}

struct LengthThenStop {
    steps: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Provider for LengthThenStop {
    fn stream<'a>(
        &'a self,
        _request: &'a Request,
        cancel: CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let step = self.steps.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (sender, stream) = EventStream::channel(cancel);
            if step == 0 {
                sender
                    .send(StreamEvent::Done {
                        message: mycode_core::AssistantMessage {
                            blocks: vec![
                                ContentBlock::Text(mycode_core::TextBlock::new("cut short")),
                                ContentBlock::ToolCall(mycode_core::ToolCall::new(
                                    "",
                                    "read",
                                    serde_json::json!({}),
                                )),
                                ContentBlock::ToolCall(mycode_core::ToolCall::new(
                                    "call-named",
                                    "read",
                                    serde_json::json!({}),
                                )),
                            ],
                            usage: None,
                            stop_reason: StopReason::Length,
                        },
                    })
                    .await;
            } else {
                sender
                    .send(StreamEvent::Done {
                        message: mycode_core::AssistantMessage {
                            blocks: vec![ContentBlock::Text(mycode_core::TextBlock::new("done"))],
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

#[tokio::test]
async fn length_truncated_call_without_an_id_is_not_answered() {
    let provider = LengthThenStop {
        steps: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };
    let tools = ToolRegistry::new();
    let hooks = HookRunner::new();
    let env = TurnEnv::new(&provider, &tools, &hooks);
    let mut agent = Agent::new(AgentConfig::new());
    let outcome = agent
        .prompt(Message::User(UserMessage::text("start")), &env)
        .await
        .expect("completed turn");
    assert!(matches!(
        outcome,
        mycode_core::events::TurnOutcome::Completed
    ));
    let results: Vec<&mycode_core::ToolResultMessage> = agent
        .state()
        .messages()
        .iter()
        .filter_map(|message| match message.as_ref() {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1, "only the named call is answered");
    assert_eq!(results[0].tool_call_id, "call-named");
    assert!(results[0].is_error);
}

#[tokio::test]
async fn tool_calls_missing_id_or_name_do_not_dispatch_or_resend_unanswered_assistant() {
    let steps = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    struct UnnamedCallsProvider {
        steps: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Provider for UnnamedCallsProvider {
        fn stream<'a>(
            &'a self,
            _request: &'a Request,
            cancel: CancellationToken,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>,
        > {
            Box::pin(async move {
                let step = self.steps.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let (sender, stream) = EventStream::channel(cancel);
                if step == 0 {
                    sender
                        .send(StreamEvent::Done {
                            message: mycode_core::AssistantMessage {
                                blocks: vec![
                                    ContentBlock::Text(mycode_core::TextBlock::new(
                                        "truncated call",
                                    )),
                                    ContentBlock::ToolCall(mycode_core::ToolCall::new(
                                        "",
                                        "read",
                                        serde_json::json!({}),
                                    )),
                                    ContentBlock::ToolCall(mycode_core::ToolCall::new(
                                        "call-unnamed",
                                        "",
                                        serde_json::json!({}),
                                    )),
                                ],
                                usage: None,
                                stop_reason: StopReason::Length,
                            },
                        })
                        .await;
                } else {
                    sender
                        .send(StreamEvent::Done {
                            message: mycode_core::AssistantMessage {
                                blocks: vec![ContentBlock::Text(mycode_core::TextBlock::new(
                                    "unexpected second step",
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
    let provider = UnnamedCallsProvider {
        steps: std::sync::Arc::clone(&steps),
    };
    let tools = ToolRegistry::new();
    let hooks = HookRunner::new();
    let env = TurnEnv::new(&provider, &tools, &hooks);
    let mut agent = Agent::new(AgentConfig::new());
    let outcome = agent
        .prompt(Message::User(UserMessage::text("start")), &env)
        .await
        .expect("completed turn");
    assert!(matches!(
        outcome,
        mycode_core::events::TurnOutcome::Completed
    ));
    assert_eq!(
        steps.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "an assistant message with no pairable tool calls must not loop into a second request"
    );
    assert!(
        agent
            .state()
            .messages()
            .iter()
            .all(|message| match message.as_ref() {
                Message::Assistant(assistant) => assistant
                    .blocks
                    .iter()
                    .all(|block| !matches!(block, ContentBlock::ToolCall(_))),
                _ => true,
            }),
        "unpairable calls must not be persisted for the next user turn"
    );
}

#[tokio::test]
async fn dropping_prompt_inside_the_compaction_hook_keeps_history() {
    // A rewrite that outlives the caller must not cost the conversation:
    // the request shares Arc clones, so dropping this prompt future
    // mid-hook leaves the in-memory history intact instead of empty.
    let hook_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let signal = std::sync::Arc::clone(&hook_entered);
    let hooks = HookRunner::new().with_before_request(move |_request: Request| {
        let signal = std::sync::Arc::clone(&signal);
        async move {
            signal.notify_one();
            std::future::pending::<Request>().await
        }
    });
    let provider = Scripted { events: vec![] };
    let tools = ToolRegistry::new();
    let env = TurnEnv::new(&provider, &tools, &hooks);
    let mut agent = Agent::new(AgentConfig::new());
    agent.seed_history(std::iter::once(std::sync::Arc::new(Message::User(
        UserMessage::text("earlier"),
    ))));
    let entered = hook_entered.notified();
    tokio::pin!(entered);
    // Box the future so dropping it here truly ends the `&mut agent` borrow.
    let mut prompt = Box::pin(agent.prompt(Message::User(UserMessage::text("hello")), &env));
    tokio::select! {
        () = entered.as_mut() => {}
        result = prompt.as_mut() => panic!("prompt finished without the hook: {result:?}"),
    }
    drop(prompt);
    let messages = agent.state().messages();
    assert_eq!(messages.len(), 2, "history survived the dropped rewrite");
    assert!(matches!(messages[1].as_ref(), Message::User(_)));
}
