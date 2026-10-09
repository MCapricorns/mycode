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
//! with `Cancelled`, partial assistant messages are *not* kept (only
//! completed messages enter history), `is_streaming` resets, and
//! `prompt()` returns [`TurnOutcome::Aborted`] — never a half
//! `TurnEnded::Completed`.
//!
//! A provider or transport failure is different: thinking and text that
//! already arrived stay in history, with one visible interruption line.
//! Named tool calls on that message are not executed. The turn completes.

use std::sync::Arc;

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
    /// results. Only completed messages live here — a response aborted
    /// mid-stream never enters. Entries are shared with provider requests.
    pub messages: Vec<Arc<Message>>,
    /// Whether a turn is currently streaming.
    pub is_streaming: bool,
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
/// │    stream response (MessageDelta …) → history
/// │    dispatch tool calls (registry → ToolResult → hist.)
/// │  would-stop: stop_gate → else break
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

    /// Read-only access to the conversation state.
    /// Loads replay history before the first prompt.
    pub fn seed_history(&mut self, messages: impl IntoIterator<Item = Arc<Message>>) {
        self.state.messages.extend(messages);
    }

    pub fn state(&self) -> &AgentState {
        &self.state
    }

    /// The static agent config.
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Run one turn: push `msg` (a user message) into the history,
    /// stream responses and dispatch tools until the model stops.
    ///
    /// Cancellation (`env.cancel`) ends the turn with
    /// [`TurnOutcome::Aborted`] and drops an in-flight partial.
    /// A provider failure keeps the partial assistant message and
    /// completes the turn. Tool-level failures never end the turn
    /// (they become `is_error` tool results the model can react to).
    /// A missing `git` binary is one of those tool failures when a
    /// worktree lease cannot be created; it does not rewind the turn.
    pub async fn prompt(
        &mut self,
        msg: Message,
        env: &TurnEnv<'_>,
    ) -> Result<TurnOutcome, MycodeError> {
        // Child token: a cancelled child never leaks into the parent or
        // the next turn.
        let token = env.cancel.child_token();
        self.state.is_streaming = true;

        let result = run_turn(&self.config, &mut self.state, msg, env, &token).await;

        self.state.is_streaming = false;
        result
    }
}

/// One full turn: `TurnStarted … TurnEnded` bracketing the loop.
async fn run_turn(
    config: &AgentConfig,
    state: &mut AgentState,
    msg: Message,
    env: &TurnEnv<'_>,
    token: &CancellationToken,
) -> Result<TurnOutcome, MycodeError> {
    turn::emit(env, AgentEvent::TurnStarted);

    let outcome = agent_loop(config, state, msg, env, token).await;

    match &outcome {
        Ok(outcome) => turn::emit(env, AgentEvent::TurnEnded(*outcome)),
        // The Error event was already emitted at the failure site; the
        // turn still ends for event subscribers.
        Err(_) => turn::emit(env, AgentEvent::TurnEnded(TurnOutcome::Aborted)),
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
) -> Result<TurnOutcome, MycodeError> {
    turn::push_message(env, state, prompt_msg);

    let mut aborted = false;

    // Stream→tool cycles while the model keeps calling tools.
    let mut has_tool_calls = true;
    while has_tool_calls {
        if token.is_cancelled() {
            aborted = true;
            break;
        }

        let assistant = match turn::stream_assistant(env, token, config, state).await {
            Ok(message) => message,
            Err(TurnFailure::Aborted) => {
                aborted = true;
                break;
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
            // with an error result and stop; do not run them or retry.
            if assistant.stop_reason == StopReason::Error {
                for call in calls
                    .iter()
                    .filter(|call| !call.id.is_empty() && !call.name.is_empty())
                {
                    let message = turn::fail_interrupted_call(env, call);
                    turn::push_message(env, state, Message::ToolResult(message));
                }
            }
            has_tool_calls = false;
        } else if assistant.stop_reason == StopReason::Length {
            // Truncated arguments are never executed (pi parity);
            // the model re-issues the calls.
            for call in &calls {
                let message = turn::fail_truncated_call(env, call);
                turn::push_message(env, state, Message::ToolResult(message));
            }
            has_tool_calls = true;
        } else {
            // `agent` calls overlap everything else in this response, so a
            // scout and an MCP lookup requested together actually run
            // together. Other tools stay in order (`search_tool` before
            // `use_tool`). Results are written back in call order.
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
    let tasks = async {
        let futures = task_indexes
            .iter()
            .map(|index| turn::dispatch_tool_call(env, token, &calls[*index]));
        futures_util::future::join_all(futures).await
    };
    let others = async {
        let mut messages = Vec::with_capacity(other_indexes.len());
        for index in &other_indexes {
            if token.is_cancelled() {
                messages.push(turn::fail_cancelled_call(env, &calls[*index]));
            } else {
                messages.push(turn::dispatch_tool_call(env, token, &calls[*index]).await);
            }
        }
        messages
    };
    let (task_messages, other_messages) = tokio::join!(tasks, others);
    let mut slots: Vec<Option<ToolResultMessage>> = Vec::with_capacity(calls.len());
    slots.resize_with(calls.len(), || None);
    for (index, message) in task_indexes.into_iter().zip(task_messages) {
        slots[index] = Some(message);
    }
    for (index, message) in other_indexes.into_iter().zip(other_messages) {
        slots[index] = Some(message);
    }
    slots
        .into_iter()
        .map(|message| message.expect("every call was dispatched"))
        .collect()
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
    use crate::env::TurnEnv;
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
        let provider = Scripted {
            events: vec![
                StreamEvent::ThinkingDelta("discard me".into()),
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
        assert!(
            agent
                .state()
                .messages()
                .iter()
                .all(|message| { !matches!(message.as_ref(), Message::Assistant(_)) })
        );
    }
}
