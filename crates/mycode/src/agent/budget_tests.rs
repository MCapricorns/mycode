//! Behavior at the shared model-request boundary, including delegated turns.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tokio_util::sync::CancellationToken;

use super::{Agent, AgentConfig, DEFAULT_MAX_MODEL_REQUESTS, HookRunner, TurnBudget, TurnEnv};
use crate::core::events::{AgentEvent, TurnOutcome};
use crate::core::{
    AssistantMessage, ContentBlock, EventStream, Message, MycodeError, Provider, ProviderError,
    ProviderErrorKind, Request, StopReason, StreamEvent, TextBlock, ToolCall, ToolSpec,
    UserMessage,
};
use crate::tools::{ToolCtx, ToolDyn, ToolError, ToolRegistry, ToolResult, ToolStream};

#[derive(Default)]
struct LoopProvider {
    calls: AtomicU32,
    finish_after: Option<u32>,
}

impl Provider for LoopProvider {
    fn stream<'a>(
        &'a self,
        _request: &'a Request,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            let step = self.calls.fetch_add(1, Ordering::Relaxed);
            // Let concurrent agents compete for the shared allowance.
            tokio::task::yield_now().await;
            let done = self.finish_after.is_some_and(|limit| step >= limit);
            let message = AssistantMessage {
                blocks: vec![if done {
                    ContentBlock::Text(TextBlock::new("finished"))
                } else {
                    ContentBlock::ToolCall(ToolCall::new(
                        format!("call-{step}"),
                        "record",
                        serde_json::json!({}),
                    ))
                }],
                usage: None,
                stop_reason: if done {
                    StopReason::Stop
                } else {
                    StopReason::ToolUse
                },
            };
            let (sender, stream) = EventStream::channel(cancel);
            sender.send(StreamEvent::Done { message }).await;
            Ok(stream)
        })
    }
}

#[derive(Default)]
struct RecordTool(AtomicU32);

impl ToolDyn for RecordTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "record".into(),
            description: "Records completed work".into(),
            params_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn execute_dyn<'a>(
        &'a self,
        _args: serde_json::Value,
        ctx: &'a ToolCtx,
        _out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(ToolResult::text(format!("completed {}", ctx.call_id)))
        })
    }
}

fn prompt(text: &str) -> Message {
    Message::User(UserMessage::text(text))
}

fn completed_tools(agent: &Agent) -> usize {
    agent
        .state()
        .messages()
        .iter()
        .filter(
            |message| matches!(message.as_ref(), Message::ToolResult(result) if !result.is_error),
        )
        .count()
}

#[tokio::test]
async fn exhausted_turn_keeps_completed_tools_and_continues_with_a_fresh_allowance() {
    let provider = LoopProvider {
        finish_after: Some(2),
        ..Default::default()
    };
    let tool = Arc::new(RecordTool::default());
    let tools = ToolRegistry::new();
    tools.register(tool.clone());
    let hooks = HookRunner::new();
    let budget = Arc::new(TurnBudget::new(2));
    let env = TurnEnv::new(&provider, &tools, &hooks).with_budget(Arc::clone(&budget));
    let mut events = env.events.subscribe();
    let mut agent = Agent::new(AgentConfig::new());

    let error = agent.prompt(prompt("work"), &env).await.unwrap_err();
    assert!(matches!(error, MycodeError::BudgetExceeded(_)));
    assert_eq!(provider.calls.load(Ordering::Relaxed), 2);
    assert_eq!(budget.model_requests_started(), 2);
    assert_eq!(completed_tools(&agent), 2);
    assert!(!env.cancel.is_cancelled());
    let mut errors = 0;
    let mut endings = Vec::new();
    while let Ok(event) = events.try_recv() {
        match event {
            AgentEvent::Error(MycodeError::BudgetExceeded(_)) => errors += 1,
            AgentEvent::TurnEnded(outcome) => endings.push(outcome),
            _ => {}
        }
    }
    assert_eq!(errors, 1);
    assert_eq!(endings, [TurnOutcome::Aborted]);

    let resumed = TurnEnv::new(&provider, &tools, &hooks)
        .with_cancel(env.cancel.clone())
        .with_budget(Arc::new(TurnBudget::new(1)));
    assert_eq!(
        agent.prompt(prompt("continue"), &resumed).await.unwrap(),
        TurnOutcome::Completed
    );
    assert_eq!(provider.calls.load(Ordering::Relaxed), 3);
    assert_eq!(tool.0.load(Ordering::Relaxed), 2);
    assert_eq!(completed_tools(&agent), 2);
}

#[tokio::test]
async fn concurrent_agents_cannot_multiply_the_shared_turn_allowance() {
    let provider = LoopProvider::default();
    let tool = Arc::new(RecordTool::default());
    let tools = ToolRegistry::new();
    tools.register(tool.clone());
    let hooks = HookRunner::new();
    let budget = Arc::new(TurnBudget::new(7));
    let parent = TurnEnv::new(&provider, &tools, &hooks).with_budget(Arc::clone(&budget));
    let child = TurnEnv::new(&provider, &tools, &hooks).with_budget(Arc::clone(&budget));
    let mut parent_agent = Agent::new(AgentConfig::new());
    let mut child_agent = Agent::new(AgentConfig::new());
    let (parent_result, child_result) = tokio::join!(
        parent_agent.prompt(prompt("parent"), &parent),
        child_agent.prompt(prompt("child"), &child),
    );
    for outcome in [parent_result, child_result] {
        assert!(matches!(outcome, Err(MycodeError::BudgetExceeded(_))));
    }
    assert_eq!(provider.calls.load(Ordering::Relaxed), 7);
    assert_eq!(tool.0.load(Ordering::Relaxed), 7);
    assert_eq!(budget.model_requests_started(), 7);
    assert!(completed_tools(&parent_agent) > 0);
    assert!(completed_tools(&child_agent) > 0);
    assert_eq!(
        completed_tools(&parent_agent) + completed_tools(&child_agent),
        7
    );
    assert!(!parent.cancel.is_cancelled());
    assert!(!child.cancel.is_cancelled());
}

struct OverflowProvider {
    calls: AtomicU32,
    streamed: bool,
}

impl Provider for OverflowProvider {
    fn stream<'a>(
        &'a self,
        _request: &'a Request,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let error = ProviderError::with_message(
                ProviderErrorKind::Rejected,
                "HTTP 400: This model's maximum context length is 8192 tokens",
            )
            .with_status(400);
            if !self.streamed {
                return Err(error);
            }
            let (sender, stream) = EventStream::channel(cancel);
            sender.send(StreamEvent::Error(error)).await;
            Ok(stream)
        })
    }
}

#[tokio::test]
async fn context_overflow_retries_need_another_shared_request_slot() {
    for streamed in [false, true] {
        let provider = OverflowProvider {
            calls: AtomicU32::new(0),
            streamed,
        };
        let shrinks = Arc::new(AtomicU32::new(0));
        let hook_calls = Arc::clone(&shrinks);
        let hooks = HookRunner::new().with_context_overflow(move |mut request| {
            hook_calls.fetch_add(1, Ordering::Relaxed);
            request.messages.remove(0);
            async move { Some(request) }
        });
        let tools = ToolRegistry::new();
        let budget = Arc::new(TurnBudget::new(1));
        let env = TurnEnv::new(&provider, &tools, &hooks).with_budget(Arc::clone(&budget));
        let mut agent = Agent::new(AgentConfig::new());
        agent.seed_history([Arc::new(prompt("old context"))]);
        assert!(matches!(
            agent.prompt(prompt("work"), &env).await,
            Err(MycodeError::BudgetExceeded(_))
        ));
        assert_eq!(shrinks.load(Ordering::Relaxed), 1);
        assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
        assert_eq!(budget.model_requests_started(), 1);
        assert!(!env.cancel.is_cancelled());
    }
}

#[tokio::test]
async fn reusing_default_environment_starts_a_fresh_budget_for_each_prompt() {
    let provider = LoopProvider::default();
    let tools = ToolRegistry::new();
    tools.register(Arc::new(RecordTool::default()));
    let hooks = HookRunner::new();
    let env = TurnEnv::new(&provider, &tools, &hooks);
    let mut agent = Agent::new(AgentConfig::new());
    for turn in 1..=2 {
        assert!(matches!(
            agent.prompt(prompt("work"), &env).await,
            Err(MycodeError::BudgetExceeded(_))
        ));
        assert_eq!(
            provider.calls.load(Ordering::Relaxed),
            turn * DEFAULT_MAX_MODEL_REQUESTS
        );
    }
}

#[tokio::test]
async fn cancellation_before_admission_does_not_spend_the_allowance() {
    let provider = LoopProvider::default();
    let tools = ToolRegistry::new();
    let hooks = HookRunner::new();
    let budget = Arc::new(TurnBudget::new(1));
    let env = TurnEnv::new(&provider, &tools, &hooks).with_budget(Arc::clone(&budget));
    env.cancel.cancel();
    let mut agent = Agent::new(AgentConfig::new());
    assert_eq!(
        agent.prompt(prompt("work"), &env).await.unwrap(),
        TurnOutcome::Aborted
    );
    assert_eq!(provider.calls.load(Ordering::Relaxed), 0);
    assert_eq!(budget.model_requests_started(), 0);
}
