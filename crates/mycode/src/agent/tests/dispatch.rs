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
        crate::core::events::TurnOutcome::Completed
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
                ContentBlock::ToolCall(crate::core::ToolCall::new(id, name, serde_json::json!({})))
            };
            let message = if step == 0 {
                crate::core::AssistantMessage {
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
                crate::core::AssistantMessage {
                    blocks: vec![ContentBlock::Text(crate::core::TextBlock::new("done"))],
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

impl crate::tools::ToolDyn for ProbeTool {
    fn spec(&self) -> crate::core::ToolSpec {
        crate::core::ToolSpec {
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
        ctx: &'a crate::tools::ToolCtx,
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
            let id = ctx.call_id.clone();
            self.log.lock().expect("log").push(format!("start {id}"));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            self.log.lock().expect("log").push(format!("end {id}"));
            Ok(crate::tools::ToolResult::text(id))
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

struct SlowProvider;

impl Provider for SlowProvider {
    fn stream<'a>(
        &'a self,
        _request: &'a Request,
        cancel: CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let (sender, stream) = EventStream::channel(cancel);
            sender
                .send(StreamEvent::Done {
                    message: crate::core::AssistantMessage {
                        blocks: vec![
                            ContentBlock::ToolCall(crate::core::ToolCall::new(
                                "slow",
                                "probe_slow",
                                serde_json::json!({}),
                            )),
                            ContentBlock::ToolCall(crate::core::ToolCall::new(
                                "after",
                                "probe_slow",
                                serde_json::json!({}),
                            )),
                        ],
                        usage: None,
                        stop_reason: StopReason::ToolUse,
                    },
                })
                .await;
            Ok(stream)
        })
    }
}

/// A tool that reports when it starts and then never finishes on its own.
struct SlowTool {
    started: std::sync::Arc<tokio::sync::Notify>,
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl crate::tools::ToolDyn for SlowTool {
    fn spec(&self) -> crate::core::ToolSpec {
        crate::core::ToolSpec {
            name: "probe_slow".to_owned(),
            description: "runs until it is dropped".to_owned(),
            params_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn execute_dyn<'a>(
        &'a self,
        _args: serde_json::Value,
        ctx: &'a crate::tools::ToolCtx,
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
            let id = ctx.call_id.clone();
            self.log.lock().expect("log").push(format!("start {id}"));
            self.started.notify_one();
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            self.log.lock().expect("log").push(format!("end {id}"));
            Ok(crate::tools::ToolResult::text(id))
        })
    }
}

fn result_text(result: &crate::core::ToolResultMessage) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect()
}

/// A cancel drops the tool that is still running instead of waiting it out:
/// a stopped turn must not keep a shell command — or any other tool — alive.
#[tokio::test]
async fn a_cancel_drops_the_running_tool_instead_of_waiting() {
    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let tools = ToolRegistry::new();
    tools.register(std::sync::Arc::new(SlowTool {
        started: std::sync::Arc::clone(&started),
        log: std::sync::Arc::clone(&log),
    }));
    let hooks = HookRunner::new();
    let env = TurnEnv::new(&SlowProvider, &tools, &hooks);
    let mut agent = Agent::new(AgentConfig::new());
    let mut prompt = Box::pin(agent.prompt(Message::User(UserMessage::text("go")), &env));
    tokio::select! {
        () = started.notified() => {}
        result = prompt.as_mut() => panic!("the turn ended before the tool ran: {result:?}"),
    }
    env.cancel.cancel();
    let outcome = prompt.await.expect("aborted turn");
    assert!(matches!(outcome, crate::core::events::TurnOutcome::Aborted));
    assert_eq!(
        *log.lock().expect("log"),
        ["start slow"],
        "the running tool was dropped, not waited out"
    );
    let results: Vec<&crate::core::ToolResultMessage> = agent
        .state()
        .messages()
        .iter()
        .filter_map(|message| match message.as_ref() {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2, "every call is answered");
    assert_eq!(results[0].tool_call_id, "slow");
    assert!(results[0].is_error);
    assert!(
        result_text(results[0]).contains("discarded"),
        "the dropped call reports as discarded: {}",
        result_text(results[0])
    );
    assert_eq!(results[1].tool_call_id, "after");
    assert!(results[1].is_error);
    assert!(
        result_text(results[1]).contains("not executed"),
        "the call that never ran reports that: {}",
        result_text(results[1])
    );
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
                    message: crate::core::AssistantMessage {
                        blocks: vec![ContentBlock::Text(crate::core::TextBlock::new("fits now"))],
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
