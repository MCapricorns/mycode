//! Tests for the tool dispatch loop: steering between steps, parallel reads,

use super::*;

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

/// Refuses every request holding more than `fits` messages as over the
/// context window, in the stream's first event as most providers do, and
/// answers the rest. Records each request's message count.
struct WindowProvider {
    fits: usize,
    sizes: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
}

impl Provider for WindowProvider {
    fn stream<'a>(
        &'a self,
        request: &'a Request,
        cancel: CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.sizes
                .lock()
                .expect("sizes")
                .push(request.messages.len());
            let (sender, stream) = EventStream::channel(cancel);
            let event = if request.messages.len() > self.fits {
                StreamEvent::Error(
                    ProviderError::with_message(
                        ProviderErrorKind::Rejected,
                        "HTTP 400: This model's maximum context length is 8192 tokens",
                    )
                    .with_status(400),
                )
            } else {
                StreamEvent::Done {
                    message: mycode_core::AssistantMessage {
                        blocks: vec![ContentBlock::Text(mycode_core::TextBlock::new("fits now"))],
                        usage: None,
                        stop_reason: StopReason::Stop,
                    },
                }
            };
            sender.send(event).await;
            Ok(stream)
        })
    }
}

/// Keeps the newest `keep` messages of every refused request.
fn shrinking_hooks(
    keep: usize,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> HookRunner {
    HookRunner::new().with_context_overflow(move |mut request: Request| {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        async move {
            let drop = request.messages.len().saturating_sub(keep);
            request.messages.drain(..drop);
            Some(request)
        }
    })
}

fn seeded_agent(earlier: usize) -> Agent {
    let mut agent = Agent::new(AgentConfig::new());
    agent.seed_history((0..earlier).map(|index| {
        std::sync::Arc::new(Message::User(UserMessage::text(format!("earlier {index}"))))
    }));
    agent
}

#[tokio::test]
async fn a_request_over_the_window_is_shrunk_and_sent_again() {
    let sizes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = WindowProvider {
        fits: 2,
        sizes: std::sync::Arc::clone(&sizes),
    };
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hooks = shrinking_hooks(2, std::sync::Arc::clone(&calls));
    let tools = ToolRegistry::new();
    let env = TurnEnv::new(&provider, &tools, &hooks);
    let mut agent = seeded_agent(4);
    agent
        .prompt(Message::User(UserMessage::text("now")), &env)
        .await
        .expect("completed turn");
    assert_eq!(*sizes.lock().expect("sizes"), [5, 2]);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(assistant_text(&agent), "fits now");
    // The shrunk history is what later requests build on.
    assert_eq!(agent.state().messages().len(), 3);
}

#[tokio::test]
async fn a_request_still_over_the_window_is_not_shrunk_twice() {
    let sizes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = WindowProvider {
        fits: 1,
        sizes: std::sync::Arc::clone(&sizes),
    };
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hooks = shrinking_hooks(3, std::sync::Arc::clone(&calls));
    let tools = ToolRegistry::new();
    let env = TurnEnv::new(&provider, &tools, &hooks);
    let mut agent = seeded_agent(4);
    agent
        .prompt(Message::User(UserMessage::text("now")), &env)
        .await
        .expect("completed turn");
    assert_eq!(*sizes.lock().expect("sizes"), [5, 3]);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let text = assistant_text(&agent);
    assert!(text.contains("maximum context length"), "{text}");
}
