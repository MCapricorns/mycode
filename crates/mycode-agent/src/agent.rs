//! The agent loop: stream-then-tool cycles until the model stops calling
//! tools. See `docs/agent.md`. The shape follows pi's `runAgentLoop`.
//!
//! # Turn model
//!
//! One `prompt()` call is one **turn** (`TurnStarted` … `TurnEnded`).
//! A turn consists of any number of LLM response cycles.
//!
//! # Abort
//!
//! Firing `TurnEnv::cancel` cancels the turn's cancellation token — a
//! child of the caller's token. The in-flight provider stream terminates
//! with `Cancelled`. Thinking, text, and any fully formed tool calls
//! already on that assistant message stay in history, with one visible
//! interruption line (`interrupted by user`). Incomplete tool calls are
//! not executed. `prompt()` returns [`TurnOutcome::Aborted`] — never a
//! half `TurnEnded::Completed`.
//!
//! A provider or transport failure is the same shape of partial, with
//! the provider's detail on the interruption line, but the turn completes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mycode_core::MycodeError;
use mycode_core::events::{AgentEvent, TurnOutcome};
use mycode_core::message::{ContentBlock, Message, StopReason, ToolCall, ToolResultMessage};
use tokio_util::sync::CancellationToken;

use crate::env::TurnEnv;
use crate::turn::{self, TurnFailure};

/// Static provider-neutral agent configuration.
#[derive(Debug, Clone, Default)]
pub struct AgentConfig {
    /// System prompt parts, emitted in order ahead of the history.
    pub system_prompt: Vec<String>,
    /// Requested reasoning effort for providers that support it.
    pub reasoning: Option<mycode_core::ReasoningLevel>,
    /// models.dev output cap forwarded onto each provider request.
    pub max_output_tokens: Option<u64>,
    /// Published effort spelling that is not a built-in level.
    pub reasoning_token: Option<String>,
    /// Session id forwarded as a provider prompt-cache key.
    pub prompt_cache_key: Option<String>,
}

impl AgentConfig {
    /// Creates a config with no explicit system prompt.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one system prompt part.
    #[must_use]
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt.push(prompt.into());
        self
    }

    /// Requests a reasoning effort level.
    #[must_use]
    pub fn with_reasoning(mut self, level: impl Into<Option<mycode_core::ReasoningLevel>>) -> Self {
        self.reasoning = level.into();
        self
    }

    /// Sets the models.dev output cap. `None` and `0` leave the field unset.
    #[must_use]
    pub fn with_max_output_tokens(mut self, limit: Option<u64>) -> Self {
        self.max_output_tokens = limit.filter(|tokens| *tokens > 0);
        self
    }

    /// Forwards a models.dev effort token the built-in levels do not name.
    #[must_use]
    pub fn with_reasoning_token(mut self, token: impl Into<Option<String>>) -> Self {
        self.reasoning_token = token.into();
        self
    }

    /// Sets the session key providers use for prompt-cache affinity.
    #[must_use]
    pub fn with_prompt_cache_key(mut self, key: impl Into<Option<String>>) -> Self {
        self.prompt_cache_key = key.into().filter(|key| !key.trim().is_empty());
        self
    }
}

/// The agent's conversation state.
#[derive(Debug, Default)]
pub struct AgentState {
    /// Conversation history: user inputs, assistant messages, tool
    /// results. A user cancel keeps the partial assistant message
    /// (thinking, text, fully formed tool calls, and the interruption
    /// line). Incomplete tool calls never enter. Entries are shared
    /// with provider requests.
    pub messages: Vec<Arc<Message>>,
}

impl AgentState {
    /// The message history.
    pub fn messages(&self) -> &[Arc<Message>] {
        &self.messages
    }
}

/// The UI-free, session-free agent: state + the loop.
///
/// ```text
/// prompt(msg)
/// ├─ TurnStarted, msg enters history
/// ├─ loop: while tool calls
/// │    drain steers, then stream response (MessageDelta …) → history
/// │    dispatch tool calls (registry → ToolResult → hist.)
/// │  break when the response has no tool calls and no pending steer
/// └─ TurnEnded(Completed | Aborted)
/// ```
pub struct Agent {
    config: AgentConfig,
    state: AgentState,
}

impl Agent {
    /// An agent with empty history and the given static config.
    pub fn new(config: AgentConfig) -> Self {
        Self {
            config,
            state: AgentState::default(),
        }
    }

    /// Appends replayed messages before the first prompt.
    pub fn seed_history(&mut self, messages: impl IntoIterator<Item = Arc<Message>>) {
        self.state.messages.extend(messages);
    }

    /// Read-only access to the conversation state.
    pub fn state(&self) -> &AgentState {
        &self.state
    }

    /// Run one turn: push `msg` (a user message) into the history,
    /// stream responses and dispatch tools until the model stops.
    ///
    /// Cancellation (`env.cancel`) ends the turn with
    /// [`TurnOutcome::Aborted`] and keeps the in-flight partial
    /// (thinking, text, fully formed tool calls, and an interruption
    /// line). Incomplete tool calls are not executed. A provider
    /// failure keeps the same partial and completes the turn.
    /// Tool-level failures never end the turn (they become `is_error`
    /// tool results the model can react to). Steers queued on
    /// [`TurnEnv::steer`] are appended at loop boundaries and do not
    /// cancel the turn.
    pub async fn prompt(
        &mut self,
        msg: Message,
        env: &TurnEnv<'_>,
    ) -> Result<TurnOutcome, MycodeError> {
        // Child token: a cancelled child never leaks into the parent or
        // the next turn.
        let token = env.cancel.child_token();
        run_turn(&self.config, &mut self.state, msg, env, &token).await
    }
}

/// One full turn: `TurnStarted … TurnEnded` bracketing the loop.
///
/// `TurnStarted` is emitted only after the request validates. A prompt that
/// never starts a turn does not end with [`TurnOutcome::Aborted`].
async fn run_turn(
    config: &AgentConfig,
    state: &mut AgentState,
    msg: Message,
    env: &TurnEnv<'_>,
    token: &CancellationToken,
) -> Result<TurnOutcome, MycodeError> {
    let mut started = false;
    let outcome = agent_loop(config, state, msg, env, token, &mut started).await;
    if started {
        match &outcome {
            Ok(outcome) => turn::emit(env, AgentEvent::TurnEnded(*outcome)),
            // The Error event was already emitted at the failure site; the
            // turn still ends for event subscribers.
            Err(_) => turn::emit(env, AgentEvent::TurnEnded(TurnOutcome::Aborted)),
        }
    }
    outcome
}

/// The loop itself (no turn bracket events).
async fn agent_loop(
    config: &AgentConfig,
    state: &mut AgentState,
    prompt_msg: Message,
    env: &TurnEnv<'_>,
    token: &CancellationToken,
    started: &mut bool,
) -> Result<TurnOutcome, MycodeError> {
    let saved = state.messages.clone();
    let prompt = Arc::new(prompt_msg);
    state.messages.push(Arc::clone(&prompt));
    // Announced only after validation, so a rejected prompt is not a turn.
    let mut pending_user = Some(prompt);

    let mut aborted = false;

    // Stream→tool cycles while the model keeps calling tools.
    let mut has_tool_calls = true;
    while has_tool_calls {
        if token.is_cancelled() {
            aborted = true;
            break;
        }
        // A steer that arrived during the previous response or its tools
        // is folded in before the next model call. The first call waits
        // until the turn has started, so the opening prompt is not doubled.
        if *started {
            drain_steers(env, state);
        }

        let assistant =
            match turn::stream_assistant(env, token, config, state, started, &mut pending_user)
                .await
            {
                Ok(message) => message,
                Err(TurnFailure::Aborted) => {
                    aborted = true;
                    break;
                }
                Err(TurnFailure::NotStarted(err)) => {
                    state.messages = saved;
                    return Err(err);
                }
                Err(TurnFailure::Error(err)) => return Err(err),
            };

        let calls: Vec<ToolCall> = assistant
            .blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call.clone()),
                _ => None,
            })
            .collect();

        if calls.is_empty() || assistant.stop_reason == StopReason::Error {
            // A failed stream may still name tool calls. Pair each one
            // with an error result and do not run them. A queued steer
            // still continues this turn; otherwise it ends.
            if assistant.stop_reason == StopReason::Error {
                for call in calls
                    .iter()
                    .filter(|call| !call.id.is_empty() && !call.name.is_empty())
                {
                    let message = turn::fail_interrupted_call(env, call);
                    turn::push_message(env, state, Message::ToolResult(message));
                }
            }
            // No further tool calls: this response would end the turn.
            // A steer that landed while it streamed starts another cycle
            // in the same turn instead of being dropped.
            has_tool_calls = drain_steers(env, state);
        } else if assistant.stop_reason == StopReason::Length {
            // Truncated arguments are never executed (pi parity);
            // the model re-issues the calls.
            for call in &calls {
                let message = turn::fail_truncated_call(env, call);
                turn::push_message(env, state, Message::ToolResult(message));
            }
            has_tool_calls = true;
        } else {
            // `agent` calls overlap the rest of this response, so a child
            // and another tool requested together actually run together.
            // Non-agent tools run one after another in model order.
            // Results are written back in call order.
            if token.is_cancelled() {
                for call in &calls {
                    let message = turn::fail_cancelled_call(env, call);
                    turn::push_message(env, state, Message::ToolResult(message));
                }
                aborted = true;
                break;
            }
            let results = dispatch_response_calls(env, token, &calls).await;
            for message in results {
                turn::push_message(env, state, Message::ToolResult(message));
            }
            if token.is_cancelled() {
                aborted = true;
                break;
            }
            has_tool_calls = true;
        }
    }

    if aborted {
        if *started {
            // The turn is already ending, so this does not start another
            // model call. It keeps a steer the loop had not reached yet.
            drain_steers(env, state);
        } else {
            state.messages = saved;
        }
        return Ok(TurnOutcome::Aborted);
    }
    Ok(TurnOutcome::Completed)
}

/// `agent` calls from one assistant message share a batch. The host
/// semaphore still caps how many children actually run.
fn is_agent_call(call: &ToolCall) -> bool {
    call.name == "agent"
}

/// Runs one response's tool calls.
///
/// Every `agent` call starts immediately. The other calls run in their original
/// order at the same time, so an MCP lookup is not stuck behind a child.
/// Each call still gets one result, placed back in the model's call order.
async fn dispatch_response_calls(
    env: &TurnEnv<'_>,
    token: &CancellationToken,
    calls: &[ToolCall],
) -> Vec<ToolResultMessage> {
    let task_indexes: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter(|(_, call)| is_agent_call(call))
        .map(|(index, _)| index)
        .collect();
    let other_indexes: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter(|(_, call)| !is_agent_call(call))
        .map(|(index, _)| index)
        .collect();
    // Set on the first poll of the agent batch. Dispatch emits ToolStarted
    // before its first await, so a polled batch must not emit ToolStarted again.
    let agent_polled = Arc::new(AtomicBool::new(false));
    let polled = Arc::clone(&agent_polled);
    let agent_ids = task_indexes.clone();
    let other_ids = other_indexes.clone();
    let agents = async move {
        polled.store(true, Ordering::SeqCst);
        let futures = agent_ids
            .iter()
            .map(|index| turn::dispatch_tool_call(env, token, &calls[*index]));
        futures_util::future::join_all(futures).await
    };
    let others = async move {
        let mut messages = Vec::with_capacity(other_ids.len());
        for index in &other_ids {
            if token.is_cancelled() {
                messages.push(turn::fail_cancelled_call(env, &calls[*index]));
            } else {
                messages.push(turn::dispatch_tool_call(env, token, &calls[*index]).await);
            }
        }
        messages
    };
    tokio::pin!(agents);
    tokio::pin!(others);
    let mut agent_messages: Option<Vec<ToolResultMessage>> = None;
    let mut other_messages: Option<Vec<ToolResultMessage>> = None;
    let mut abandoned = false;
    loop {
        tokio::select! {
            biased;
            () = token.cancelled(), if !abandoned && agent_messages.is_none() => {
                // Stop polling nested agent calls. Dropping `agents` when this
                // function returns aborts work that ignores the token. Their
                // real answers are not used.
                abandoned = true;
                if other_messages.is_some() {
                    break;
                }
            }
            messages = &mut agents, if !abandoned && agent_messages.is_none() => {
                agent_messages = Some(messages);
                if other_messages.is_some() {
                    break;
                }
            }
            messages = &mut others, if other_messages.is_none() => {
                other_messages = Some(messages);
                if abandoned || agent_messages.is_some() {
                    break;
                }
            }
        }
    }
    let other_messages = other_messages.unwrap_or_default();
    let mut slots: Vec<Option<ToolResultMessage>> = Vec::with_capacity(calls.len());
    slots.resize_with(calls.len(), || None);
    if abandoned {
        let discard = agent_polled.load(Ordering::SeqCst);
        for index in task_indexes {
            slots[index] = Some(if discard {
                turn::discard_running_call(env, &calls[index])
            } else {
                turn::fail_cancelled_call(env, &calls[index])
            });
        }
    } else {
        for (index, message) in task_indexes
            .into_iter()
            .zip(agent_messages.unwrap_or_default())
        {
            slots[index] = Some(message);
        }
    }
    for (index, message) in other_indexes.into_iter().zip(other_messages) {
        slots[index] = Some(message);
    }
    slots
        .into_iter()
        .map(|message| message.expect("every call was dispatched"))
        .collect()
}

/// Appends drained steers as user messages. Returns whether any were added.
///
/// Called only after the turn has started, and only at a loop boundary, so
/// the in-flight response and its subagents are left alone.
fn drain_steers(env: &TurnEnv<'_>, state: &mut AgentState) -> bool {
    let Some(inbox) = env.steer.as_ref() else {
        return false;
    };
    let mut appended = false;
    for text in inbox.drain() {
        if text.trim().is_empty() {
            continue;
        }
        turn::push_message(
            env,
            state,
            Message::User(mycode_core::UserMessage::text(text)),
        );
        appended = true;
    }
    appended
}

#[cfg(test)]
mod tests {
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

    #[async_trait::async_trait]
    impl Provider for Scripted {
        async fn stream(
            &self,
            _request: &Request,
            cancel: CancellationToken,
        ) -> Result<EventStream, ProviderError> {
            let (sender, stream) = EventStream::channel(cancel);
            for event in self.events.clone() {
                sender.send(event).await;
            }
            Ok(stream)
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
        let stop =
            agent
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
    async fn cancel_still_drops_the_partial() {
        // The name is historical. A user cancel keeps the partial and the
        // interruption line; the turn outcome stays aborted. An unnamed
        // tool-call delta is not executed and does not enter history.
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

    #[async_trait::async_trait]
    impl Provider for CountingProvider {
        async fn stream(
            &self,
            _request: &Request,
            cancel: CancellationToken,
        ) -> Result<EventStream, ProviderError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (sender, stream) = EventStream::channel(cancel);
            sender
                .send(StreamEvent::Error(ProviderError::with_message(
                    ProviderErrorKind::Unavailable,
                    "should not stream",
                )))
                .await;
            Ok(stream)
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

    #[async_trait::async_trait]
    impl mycode_tools::ToolDyn for HangAgent {
        fn spec(&self) -> mycode_core::ToolSpec {
            mycode_core::ToolSpec {
                name: "agent".to_owned(),
                description: "nested test agent".to_owned(),
                params_schema: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute_dyn(
            &self,
            _args: serde_json::Value,
            _ctx: &mycode_tools::ToolCtx,
            _out: &mut mycode_tools::ToolStream,
        ) -> Result<mycode_tools::ToolResult, mycode_tools::ToolError> {
            self.started
                .store(true, std::sync::atomic::Ordering::SeqCst);
            while !self.release.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            Ok(mycode_tools::ToolResult::text(
                "NESTED_ANSWER_SHOULD_NOT_APPEND",
            ))
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
                            "agent",
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
        assert!(history.contains("discarded"), "{history}");
        release.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    struct StepProvider {
        seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl Provider for StepProvider {
        async fn stream(
            &self,
            request: &Request,
            cancel: CancellationToken,
        ) -> Result<EventStream, ProviderError> {
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
                                "note",
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
        }
    }

    struct NoteTool {
        inbox: std::sync::Arc<SteerInbox>,
    }

    #[async_trait::async_trait]
    impl mycode_tools::ToolDyn for NoteTool {
        fn spec(&self) -> mycode_core::ToolSpec {
            mycode_core::ToolSpec {
                name: "note".to_owned(),
                description: "records a note".to_owned(),
                params_schema: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute_dyn(
            &self,
            _args: serde_json::Value,
            _ctx: &mycode_tools::ToolCtx,
            _out: &mut mycode_tools::ToolStream,
        ) -> Result<mycode_tools::ToolResult, mycode_tools::ToolError> {
            self.inbox.push("STEER_KEEP_GOING");
            Ok(mycode_tools::ToolResult::text("noted"))
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
}
