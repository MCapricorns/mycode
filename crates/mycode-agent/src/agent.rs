//! The agent loop: stream-then-tool cycles until the model stops calling
//! tools. See `wiki/index.html`. The shape follows pi's `runAgentLoop`.
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
//! with `Cancelled`. Thinking and text already streamed stay in history,
//! with one visible interruption line (`interrupted by user`); a formed
//! tool call only arrives with `Done`, so the cancelled message carries
//! none. `prompt()` returns [`TurnOutcome::Aborted`] — never a half
//! `TurnEnded::Completed`.
//!
//! A provider or transport failure is the same shape of partial, with
//! the provider's detail on the interruption line, but the turn completes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mycode_tools::MycodeError;
use mycode_tools::events::{AgentEvent, TurnOutcome};
use mycode_tools::message::{ContentBlock, Message, SharedMessage, StopReason, ToolCall};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::env::TurnEnv;
use crate::turn::{self, TurnFailure};

/// Static provider-neutral agent configuration.
#[derive(Debug, Clone, Default)]
pub struct AgentConfig {
    /// System prompt parts, emitted in order ahead of the history. Shared
    /// with every request of the session.
    pub system_prompt: Arc<[String]>,
    /// Requested reasoning effort for providers that support it.
    pub reasoning: Option<mycode_tools::ReasoningLevel>,
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
        self.system_prompt = self
            .system_prompt
            .iter()
            .cloned()
            .chain([prompt.into()])
            .collect();
        self
    }

    /// Requests a reasoning effort level.
    #[must_use]
    pub fn with_reasoning(
        mut self,
        level: impl Into<Option<mycode_tools::ReasoningLevel>>,
    ) -> Self {
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
    /// (thinking, text, and the interruption line); a formed tool call
    /// only arrives with `Done`. Incomplete tool calls never enter.
    /// Entries are shared with provider requests.
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
    /// (thinking, text, and an interruption line); a formed tool call
    /// only arrives with `Done`. Incomplete tool calls are not executed.
    /// A provider failure keeps the same partial and completes the turn.
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
        // The response cycle only ever appends an assistant message; reuse
        // that shared message instead of copying it back out of history.
        let Message::Assistant(assistant) = assistant.as_ref() else {
            unreachable!("stream_assistant returns only assistant messages");
        };

        let calls: Vec<&ToolCall> = assistant
            .blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call),
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
                    turn::push_shared(env, state, message);
                }
            }
            // No further tool calls: this response would end the turn.
            // A steer that landed while it streamed starts another cycle
            // in the same turn instead of being dropped.
            has_tool_calls = drain_steers(env, state);
        } else if assistant.stop_reason == StopReason::Length {
            // Truncated arguments are never executed (pi parity);
            // the model re-issues the calls. Calls without an id or name
            // cannot be paired on the wire, so they are dropped, matching
            // the error branch above.
            let mut answered = false;
            for call in calls
                .iter()
                .filter(|call| !call.id.is_empty() && !call.name.is_empty())
            {
                let message = turn::fail_truncated_call(env, call);
                turn::push_shared(env, state, message);
                answered = true;
            }
            has_tool_calls = answered || drain_steers(env, state);
        } else {
            // `agent` calls overlap the rest of this response, so a child
            // and another tool requested together actually run together.
            // Adjacent read-only calls run as one batch; every other tool
            // runs alone, in model order. Results go back in call order.
            let valid_calls: Vec<&ToolCall> = calls
                .into_iter()
                .filter(|call| !call.id.is_empty() && !call.name.is_empty())
                .collect();
            if valid_calls.is_empty() {
                has_tool_calls = drain_steers(env, state);
                continue;
            }
            if token.is_cancelled() {
                for call in &valid_calls {
                    let message = turn::fail_cancelled_call(env, call);
                    turn::push_shared(env, state, message);
                }
                aborted = true;
                break;
            }
            let results = dispatch_response_calls(env, token, &valid_calls).await;
            for message in results {
                turn::push_shared(env, state, message);
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

/// Whether `call` names a registered tool that may overlap other such calls.
fn is_parallel_safe(env: &TurnEnv<'_>, call: &ToolCall) -> bool {
    env.tools
        .get(&call.name)
        .is_some_and(|tool| tool.parallel_safe_call(&call.arguments))
}

/// Runs one response's tool calls.
///
/// Every `agent` call starts immediately. The other calls run beside them, so
/// an MCP lookup is not stuck behind a child. Among those, each run of
/// adjacent parallel-safe calls (reads, searches, web lookups) is one batch;
/// a call that can change something waits for the batch before it and runs
/// alone, so reads never observe a half-applied edit. Each call still gets
/// one result, placed back in the model's call order.
async fn dispatch_response_calls(
    env: &TurnEnv<'_>,
    token: &CancellationToken,
    calls: &[&ToolCall],
) -> Vec<SharedMessage> {
    let agent_indexes: Vec<usize> = calls
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
    // Each `agent` result, stored in the poll that emits its ToolCompleted.
    // A cancel can stop the batch right after that poll; the call keeps its
    // result then, because completing it again closes its ledger call twice.
    let agent_results: Mutex<Vec<Option<SharedMessage>>> =
        Mutex::new(vec![None; agent_indexes.len()]);
    // The same two facts for the other calls: a result stored in the poll
    // that emits its ToolCompleted, and whether the call was polled at all
    // (its ToolStarted row is out, so a stop owes it a closing row too).
    let other_results: Mutex<Vec<Option<SharedMessage>>> =
        Mutex::new(vec![None; other_indexes.len()]);
    let other_started: Mutex<Vec<bool>> = Mutex::new(vec![false; other_indexes.len()]);
    let results = &agent_results;
    let agent_ids = agent_indexes.clone();
    let other_ids = other_indexes.clone();
    let agents = async move {
        polled.store(true, Ordering::SeqCst);
        let futures = agent_ids
            .iter()
            .enumerate()
            .map(|(slot, index)| async move {
                let message = turn::run_tool_call(env, token, calls[*index]).await;
                results.lock()[slot] = Some(message);
                // Yield so a ToolCompleted subscriber can commit the result
                // before the next call in this response is dispatched.
                tokio::task::yield_now().await;
            })
            .collect::<Vec<_>>();
        futures_util::future::join_all(futures).await;
    };
    let other_done = &other_results;
    let other_polled = &other_started;
    let others = async move {
        let mut next = 0;
        while next < other_ids.len() {
            if token.is_cancelled() {
                return;
            }
            // Adjacent parallel-safe calls share one batch; a call that can
            // change something runs alone.
            let batch_len = other_ids[next..]
                .iter()
                .take_while(|index| is_parallel_safe(env, calls[**index]))
                .count()
                .max(1);
            let futures = other_ids[next..next + batch_len]
                .iter()
                .enumerate()
                .map(|(offset, index)| {
                    let slot = next + offset;
                    async move {
                        // Polling this future is what emits ToolStarted, so
                        // the flag lands in the same poll.
                        other_polled.lock()[slot] = true;
                        let message = turn::run_tool_call(env, token, calls[*index]).await;
                        other_done.lock()[slot] = Some(message);
                        // Yield so a ToolCompleted subscriber can commit the
                        // result before the next call is dispatched.
                        tokio::task::yield_now().await;
                    }
                })
                .collect::<Vec<_>>();
            futures_util::future::join_all(futures).await;
            next += batch_len;
        }
    };
    tokio::pin!(agents);
    tokio::pin!(others);
    let mut agents_done = false;
    let mut others_done = false;
    loop {
        tokio::select! {
            biased;
            () = token.cancelled(), if !(agents_done && others_done) => {
                // Stop both batches now. Dropping `agents` and `others` when
                // this function returns is what ends a running tool: a shell
                // command's process tree dies with its dropped future instead
                // of holding the stopped turn open until it finishes.
                break;
            }
            () = &mut agents, if !agents_done => {
                agents_done = true;
                if others_done {
                    break;
                }
            }
            () = &mut others, if !others_done => {
                others_done = true;
                if agents_done {
                    break;
                }
            }
        }
    }
    let mut slots: Vec<Option<SharedMessage>> = Vec::with_capacity(calls.len());
    slots.resize_with(calls.len(), || None);
    // A finished batch filled every result; only an abandoned one has gaps.
    let discard = agent_polled.load(Ordering::SeqCst);
    let finished = std::mem::take(&mut *agent_results.lock());
    for (index, result) in agent_indexes.into_iter().zip(finished) {
        slots[index] = Some(match result {
            Some(message) => message,
            None if discard => turn::discard_running_call(env, calls[index]),
            None => turn::fail_cancelled_call(env, calls[index]),
        });
    }
    let finished = std::mem::take(&mut *other_results.lock());
    let started = std::mem::take(&mut *other_started.lock());
    for (slot, (result, started)) in finished.into_iter().zip(started).enumerate() {
        let index = other_indexes[slot];
        slots[index] = Some(match result {
            Some(message) => message,
            // Its ToolStarted row is already out, so it only owes a close.
            None if started => turn::discard_running_call(env, calls[index]),
            None => turn::fail_cancelled_call(env, calls[index]),
        });
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
    // Blank steers never enter the inbox (`SteerInbox::push` filters them).
    for text in inbox.drain() {
        turn::push_message(
            env,
            state,
            Message::User(mycode_tools::UserMessage::text(text)),
        );
        appended = true;
    }
    appended
}

#[cfg(test)]
mod tests;
