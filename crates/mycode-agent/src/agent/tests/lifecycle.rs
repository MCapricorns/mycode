//! Prompt lifetime and cancellation regression tests.

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct LifecycleProvider {
    steps: AtomicUsize,
    complete: bool,
}

impl Provider for LifecycleProvider {
    fn stream<'a>(
        &'a self,
        _request: &'a Request,
        cancel: CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let (sender, stream) = EventStream::channel(cancel);
            let event = if self.steps.fetch_add(1, Ordering::Relaxed) == 0 {
                StreamEvent::Done {
                    message: mycode_tools::AssistantMessage {
                        blocks: vec![ContentBlock::ToolCall(mycode_tools::ToolCall::new(
                            "child",
                            "agent",
                            serde_json::json!({}),
                        ))],
                        usage: None,
                        stop_reason: StopReason::ToolUse,
                    },
                }
            } else if self.complete {
                StreamEvent::Done {
                    message: mycode_tools::AssistantMessage {
                        blocks: vec![ContentBlock::Text(mycode_tools::TextBlock::new("done"))],
                        usage: None,
                        stop_reason: StopReason::Stop,
                    },
                }
            } else {
                StreamEvent::Error(ProviderError::new(ProviderErrorKind::Cancelled))
            };
            sender.send(event).await;
            Ok(stream)
        })
    }
}

struct ChildTokenTool {
    token: Arc<parking_lot::Mutex<Option<CancellationToken>>>,
    started: Arc<tokio::sync::Notify>,
    wait: bool,
    cancel_on_start: bool,
}

impl mycode_tools::ToolDyn for ChildTokenTool {
    fn spec(&self) -> mycode_tools::ToolSpec {
        mycode_tools::ToolSpec {
            name: "agent".to_owned(),
            description: "captures a detached child's cancellation token".to_owned(),
            params_schema: serde_json::json!({"type": "object"}),
        }
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
            *self.token.lock() = Some(ctx.cancel.child_token());
            self.started.notify_one();
            if self.cancel_on_start {
                ctx.cancel.cancel();
            }
            if self.wait {
                std::future::pending::<()>().await;
            }
            Ok(mycode_tools::ToolResult::text("yielded child"))
        })
    }
}

#[tokio::test]
async fn dropping_a_prompt_cancels_detached_children_without_cancelling_the_parent() {
    let token = Arc::new(parking_lot::Mutex::new(None));
    let started = Arc::new(tokio::sync::Notify::new());
    let tools = ToolRegistry::new();
    tools.register(Arc::new(ChildTokenTool {
        token: Arc::clone(&token),
        started: Arc::clone(&started),
        wait: true,
        cancel_on_start: false,
    }));
    let provider = LifecycleProvider {
        steps: AtomicUsize::new(0),
        complete: true,
    };
    let hooks = HookRunner::new();
    let env = TurnEnv::new(&provider, &tools, &hooks);
    let mut agent = Agent::new(AgentConfig::new());
    let mut prompt = Box::pin(agent.prompt(Message::User(UserMessage::text("go")), &env));
    tokio::select! {
        () = started.notified() => {}
        result = prompt.as_mut() => panic!("prompt ended before the child started: {result:?}"),
    }
    drop(prompt);
    assert!(token.lock().as_ref().unwrap().is_cancelled());
    assert!(!env.cancel.is_cancelled());
}

#[tokio::test]
async fn a_provider_abort_cancels_yielded_children_but_completion_keeps_late_results() {
    for complete in [false, true] {
        let token = Arc::new(parking_lot::Mutex::new(None));
        let tools = ToolRegistry::new();
        tools.register(Arc::new(ChildTokenTool {
            token: Arc::clone(&token),
            started: Arc::new(tokio::sync::Notify::new()),
            wait: false,
            cancel_on_start: false,
        }));
        let provider = LifecycleProvider {
            steps: AtomicUsize::new(0),
            complete,
        };
        let hooks = HookRunner::new();
        let env = TurnEnv::new(&provider, &tools, &hooks);
        let mut agent = Agent::new(AgentConfig::new());
        let outcome = agent
            .prompt(Message::User(UserMessage::text("go")), &env)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            if complete {
                mycode_tools::events::TurnOutcome::Completed
            } else {
                mycode_tools::events::TurnOutcome::Aborted
            }
        );
        let child = token.lock().clone().unwrap();
        assert_eq!(child.is_cancelled(), !complete);
        assert!(!env.cancel.is_cancelled());
        env.cancel.cancel();
        assert!(child.is_cancelled());
    }
}

#[tokio::test]
async fn cancelling_a_large_agent_batch_starts_and_closes_every_call_once() {
    let tools = ToolRegistry::new();
    tools.register(Arc::new(ChildTokenTool {
        token: Arc::new(parking_lot::Mutex::new(None)),
        started: Arc::new(tokio::sync::Notify::new()),
        wait: true,
        cancel_on_start: true,
    }));
    let hooks = HookRunner::new();
    let provider = Scripted { events: Vec::new() };
    let (bus, mut events) = tokio::sync::broadcast::channel(512);
    let env = TurnEnv::new(&provider, &tools, &hooks).with_events(bus);
    // join_all switches to a cooperatively polled FuturesOrdered for large
    // batches. Cancellation can leave part of that batch entirely unpolled.
    let calls: Vec<_> = (0..80)
        .map(|index| {
            mycode_tools::ToolCall::new(format!("child-{index}"), "agent", serde_json::json!({}))
        })
        .collect();
    let refs: Vec<_> = calls.iter().collect();
    let results = super::super::dispatch_response_calls(&env, &env.cancel, &refs).await;
    assert_eq!(results.len(), calls.len());
    let mut starts = std::collections::HashMap::new();
    let mut closes = std::collections::HashMap::new();
    while let Ok(event) = events.try_recv() {
        match event {
            mycode_tools::events::AgentEvent::ToolStarted { call_id, .. } => {
                *starts.entry(call_id.to_string()).or_insert(0) += 1;
            }
            mycode_tools::events::AgentEvent::ToolCompleted { call_id, .. } => {
                *closes.entry(call_id.to_string()).or_insert(0) += 1;
            }
            _ => {}
        }
    }
    for (call, result) in calls.iter().zip(results) {
        assert_eq!(
            starts.get(&call.id),
            Some(&1),
            "missing or duplicate start: {}",
            call.id
        );
        assert_eq!(
            closes.get(&call.id),
            Some(&1),
            "missing or duplicate close: {}",
            call.id
        );
        assert!(
            matches!(result.as_ref(), Message::ToolResult(result) if result.tool_call_id == call.id && result.is_error)
        );
    }
}
