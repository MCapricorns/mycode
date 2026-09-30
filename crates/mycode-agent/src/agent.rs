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
    /// [`TurnOutcome::Aborted`]. A provider failure emits
    /// [`AgentEvent::Error`] + `TurnEnded(Aborted)` and returns
    /// `Err`; tool-level failures never end the turn (they become
    /// `is_error` tool results the model can react to).
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

        if calls.is_empty() {
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
            // `task` calls overlap everything else in this response, so a
            // scout and an MCP lookup requested together actually run
            // together. Non-task tools stay in order (`search_tool` before
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

/// `task` calls from one assistant message share a batch. The host
/// semaphore still caps how many children actually run.
fn is_concurrent_task(call: &ToolCall) -> bool {
    call.name == "task"
}

/// Runs one response's tool calls.
///
/// Every `task` starts immediately. The other calls run in their original
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
        .filter(|(_, call)| is_concurrent_task(call))
        .map(|(index, _)| index)
        .collect();
    let other_indexes: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter(|(_, call)| !is_concurrent_task(call))
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
