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
                    message: crate::core::AssistantMessage {
                        blocks: vec![ContentBlock::ToolCall(crate::core::ToolCall::new(
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
                    message: crate::core::AssistantMessage {
                        blocks: vec![ContentBlock::Text(crate::core::TextBlock::new("done"))],
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

impl crate::tools::ToolDyn for ChildTokenTool {
    fn spec(&self) -> crate::core::ToolSpec {
        crate::core::ToolSpec {
            name: "agent".to_owned(),
            description: "captures a detached child's cancellation token".to_owned(),
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
            *self.token.lock() = Some(ctx.cancel.child_token());
            self.started.notify_one();
            if self.cancel_on_start {
                ctx.cancel.cancel();
            }
            if self.wait {
                std::future::pending::<()>().await;
            }
            Ok(crate::tools::ToolResult::text("yielded child"))
        })
    }
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
                crate::core::events::TurnOutcome::Completed
            } else {
                crate::core::events::TurnOutcome::Aborted
            }
        );
        let child = token.lock().clone().unwrap();
        assert_eq!(child.is_cancelled(), !complete);
        assert!(!env.cancel.is_cancelled());
        env.cancel.cancel();
        assert!(child.is_cancelled());
    }
}
