//! Inner-loop mechanics: one LLM response cycle and one tool dispatch.
//! See `docs/agent.md`.
//!
//! Everything here is a free function over `(&TurnEnv, &mut AgentState)`
//! so the agent's double loop in [`crate::agent`] can call it with
//! field-level borrows. Event emission follows the `AgentEvent`
//! vocabulary of `mycode-core`.

use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use mycode_core::events::{AgentEvent, MessageDelta};
use mycode_core::message::{AssistantMessage, ContentBlock, Message, ToolCall, ToolResultMessage};
use mycode_core::{CallId, MycodeError};
use mycode_core::{Request, StreamEvent};
use mycode_tools::{
    PreparedFile, PreparedSearch, ToolCtx, ToolDyn, ToolError, ToolResult, ToolStream,
    ToolStreamItem, prepare_file_async, prepare_search_async_with_access,
};
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentConfig, AgentState};
use crate::env::TurnEnv;
use crate::prompt::build_system_prompt;

/// Why an in-flight response cycle ended unsuccessfully.
pub(crate) enum TurnFailure {
    /// The turn's cancellation token fired (`env.cancel` or a child of it).
    Aborted,
    /// A provider-level failure. The [`AgentEvent::Error`] event has
    /// already been emitted at the failure site.
    Error(MycodeError),
    /// Request validation failed before this turn emitted [`AgentEvent::TurnStarted`].
    /// The prompt must not stay in history and the turn must not end as aborted.
    NotStarted(MycodeError),
}

/// Publish an Agent event; receiver errors (nobody listening, lagged)
/// are ignored — observers are best-effort.
pub(crate) fn emit(env: &TurnEnv<'_>, event: AgentEvent) {
    let _ = env.events.send(event);
}

/// Append a message to the history and announce it.
pub(crate) fn push_message(env: &TurnEnv<'_>, state: &mut AgentState, msg: Message) {
    let msg = Arc::new(msg);
    state.messages.push(Arc::clone(&msg));
    emit(env, AgentEvent::MessageAdded(msg));
}

/// Stream one assistant response into the history: build the request,
/// iterate the provider stream, mirror every delta as a
/// [`AgentEvent::MessageDelta`], and keep the fully assembled
/// [`AssistantMessage`] from the terminal `Done` event.
///
/// Cancellation surfaces as [`TurnFailure::Aborted`] — either via the
/// stream's own `Error(Cancelled)` termination or a boundary check.
pub(crate) async fn stream_assistant(
    env: &TurnEnv<'_>,
    token: &CancellationToken,
    config: &AgentConfig,
    state: &mut AgentState,
    started: &mut bool,
    pending_user: &mut Option<Arc<Message>>,
) -> Result<AssistantMessage, TurnFailure> {
    let system_prompt = if config.system_prompt.is_empty() {
        vec![build_system_prompt(env.tools)]
    } else {
        config.system_prompt.clone()
    };
    // Move history into the request so compaction can rewrite it in place.
    // Writing it back clones `Arc`s, not tool output. The provider borrows
    // the request.
    let request = Request {
        system_prompt,
        messages: std::mem::take(&mut state.messages),
        tools: env.tools.specs(),
        reasoning: config.reasoning,
        max_output_tokens: config.max_output_tokens,
        reasoning_token: config.reasoning_token.clone(),
        prompt_cache_key: config.prompt_cache_key.clone(),
    };
    let request = env.hooks.prepare_request(request).await;
    state.messages.clone_from(&request.messages);

    if token.is_cancelled() {
        return Err(TurnFailure::Aborted);
    }
    if let Err(error) = request.validate() {
        let error = MycodeError::from(error);
        emit(env, AgentEvent::Error(error.clone()));
        return Err(if *started {
            TurnFailure::Error(error)
        } else {
            TurnFailure::NotStarted(error)
        });
    }
    if !*started {
        emit(env, AgentEvent::TurnStarted);
        *started = true;
        if let Some(message) = pending_user.take() {
            emit(env, AgentEvent::MessageAdded(message));
        }
    }
    if token.is_cancelled() {
        return Err(TurnFailure::Aborted);
    }

    // One request gets a child token: dropping its receiver must stop that
    // producer without cancelling later response cycles in the same turn.
    let request_cancel = token.child_token();
    let mut draft_text = String::new();
    let mut draft_thinking = String::new();
    let mut stream = match env.provider.stream(&request, request_cancel).await {
        Ok(stream) => stream,
        Err(error) if error.is_cancelled() => return Err(TurnFailure::Aborted),
        // Nothing has streamed. Keep a visible assistant line instead of
        // dropping the turn.
        Err(error) => {
            return Ok(commit_interrupted(
                env,
                state,
                String::new(),
                String::new(),
                &error.to_string(),
                &[],
            ));
        }
    };
    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::TextDelta(delta) => {
                draft_text.push_str(&delta);
                emit(
                    env,
                    AgentEvent::MessageDelta(MessageDelta::TextDelta(delta)),
                );
            }
            StreamEvent::ThinkingDelta(delta) => {
                draft_thinking.push_str(&delta);
                emit(
                    env,
                    AgentEvent::MessageDelta(MessageDelta::ThinkingDelta(delta)),
                );
            }
            StreamEvent::ToolCallDelta { id, partial_json } => {
                emit(
                    env,
                    AgentEvent::MessageDelta(MessageDelta::ToolCallDelta { id, partial_json }),
                );
            }
            StreamEvent::Done { message } => {
                let message = merge_draft(message, &draft_thinking, &draft_text);
                let shared = Arc::new(Message::Assistant(message.clone()));
                state.messages.push(Arc::clone(&shared));
                emit(env, AgentEvent::MessageAdded(shared));
                return Ok(message);
            }
            StreamEvent::Error(error) => {
                if error.is_cancelled() {
                    return Err(keep_cancelled_partial(
                        env,
                        state,
                        draft_thinking,
                        draft_text,
                        &[],
                    ));
                }
                return Ok(commit_interrupted(
                    env,
                    state,
                    draft_thinking,
                    draft_text,
                    &error.to_string(),
                    &[],
                ));
            }
        }
    }
    // EventStream itself synthesizes a terminal for cancellation and producer
    // drop. Keep a fail-closed guard in case its contract is ever violated.
    if token.is_cancelled() {
        return Err(keep_cancelled_partial(
            env,
            state,
            draft_thinking,
            draft_text,
            &[],
        ));
    }
    Ok(commit_interrupted(
        env,
        state,
        draft_thinking,
        draft_text,
        "provider stream ended without a terminal event",
        &[],
    ))
}

/// User cancel: keep what already arrived, mark it interrupted, and abort.
///
/// Fully formed tool calls (non-empty id and name) stay on the assistant
/// message and are paired with an error result so the ids stay matched.
/// They are not executed. A delta that never became a named call is omitted.
fn keep_cancelled_partial(
    env: &TurnEnv<'_>,
    state: &mut AgentState,
    thinking: String,
    text: String,
    calls: &[ToolCall],
) -> TurnFailure {
    let formed = calls
        .iter()
        .any(|call| !call.id.is_empty() && !call.name.is_empty());
    if thinking.is_empty() && text.is_empty() && !formed {
        return TurnFailure::Aborted;
    }
    let message = commit_interrupted(env, state, thinking, text, "interrupted by user", calls);
    for block in &message.blocks {
        let ContentBlock::ToolCall(call) = block else {
            continue;
        };
        if call.id.is_empty() || call.name.is_empty() {
            continue;
        }
        let result = fail_cancelled_call(env, call);
        push_message(env, state, Message::ToolResult(result));
    }
    TurnFailure::Aborted
}

/// Keeps streamed thinking and text, then adds the shared interruption line.
///
/// `calls` are tool calls already fully formed on this assistant message.
/// A call with an empty id or name is incomplete: it is omitted and must
/// not be executed. Tool-call deltas carry no name, so a mid-stream cancel
/// passes none. A call that already completed on an earlier message stays
/// in history on its own.
fn commit_interrupted(
    env: &TurnEnv<'_>,
    state: &mut AgentState,
    thinking: String,
    mut text: String,
    detail: &str,
    calls: &[ToolCall],
) -> AssistantMessage {
    let note = mycode_core::interrupted_response_text(detail);
    if !text.contains(note.as_str()) {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&note);
    }
    let mut blocks = Vec::new();
    if !thinking.is_empty() {
        blocks.push(ContentBlock::Thinking(mycode_core::ThinkingBlock::new(
            thinking,
        )));
    }
    blocks.push(ContentBlock::Text(mycode_core::TextBlock::new(text)));
    for call in calls {
        if call.id.is_empty() || call.name.is_empty() {
            continue;
        }
        blocks.push(ContentBlock::ToolCall(call.clone()));
    }
    let message = AssistantMessage {
        blocks,
        usage: None,
        stop_reason: mycode_core::StopReason::Error,
    };
    let shared = Arc::new(Message::Assistant(message.clone()));
    state.messages.push(Arc::clone(&shared));
    emit(env, AgentEvent::MessageAdded(shared));
    message
}

/// Fills thinking or text the reducer omitted when the draft already has it.
fn merge_draft(mut message: AssistantMessage, thinking: &str, text: &str) -> AssistantMessage {
    let has_thinking = message
        .blocks
        .iter()
        .any(|block| matches!(block, ContentBlock::Thinking(_)));
    if !has_thinking && !thinking.is_empty() {
        message.blocks.insert(
            0,
            ContentBlock::Thinking(mycode_core::ThinkingBlock::new(thinking)),
        );
    }
    if message.text().is_empty() && !text.is_empty() {
        message
            .blocks
            .push(ContentBlock::Text(mycode_core::TextBlock::new(text)));
    }
    message
}

/// Fail a tool call from a `Length`-truncated message without executing
/// it: streamed arguments that parse may still be silently incomplete,
/// so none of them are safe to run (pi parity). The model re-issues the
/// call with complete arguments.
pub(crate) fn fail_truncated_call(env: &TurnEnv<'_>, call: &ToolCall) -> ToolResultMessage {
    unexecuted_call(
        env,
        call,
        "tool call was not executed: the response hit the output token limit, so its \
            arguments may be truncated; re-issue the call with complete arguments"
            .into(),
    )
}

/// Synthesize an `is_error` tool result for a call that arrived on a
/// failed provider turn. The call is not executed. The result keeps the
/// assistant `tool_call` id paired for the next request.
pub(crate) fn fail_interrupted_call(env: &TurnEnv<'_>, call: &ToolCall) -> ToolResultMessage {
    unexecuted_call(
        env,
        call,
        "tool call was not executed: the response was interrupted before the call \
            could run"
            .into(),
    )
}

/// Synthesize an `is_error` tool result for a call that was never
/// dispatched because the turn aborted mid-dispatch of a multi-call
/// response. The assistant message carrying every call is already in
/// history, and the wire format requires every assistant `tool_call` id
/// to be answered, so the loop writes these results before unwinding.
pub(crate) fn fail_cancelled_call(env: &TurnEnv<'_>, call: &ToolCall) -> ToolResultMessage {
    unexecuted_call(
        env,
        call,
        "tool call was not executed: the turn was aborted before this call \
            was dispatched"
            .into(),
    )
}

/// An `agent` call that already started, then the turn cancelled.
///
/// Dispatch already emitted [`AgentEvent::ToolStarted`]. This emits only
/// [`AgentEvent::ToolCompleted`] so the nested answer cannot be appended.
pub(crate) fn discard_running_call(env: &TurnEnv<'_>, call: &ToolCall) -> ToolResultMessage {
    let call_id = CallId::from(call.id.as_str());
    completed_error(
        env,
        &call_id,
        call,
        "tool call was aborted and its result was discarded".into(),
    )
}

fn unexecuted_call(env: &TurnEnv<'_>, call: &ToolCall, reason: String) -> ToolResultMessage {
    let call_id = CallId::from(call.id.as_str());
    emit(
        env,
        AgentEvent::ToolStarted {
            call_id: call_id.clone(),
            name: call.name.clone(),
            target: call.target(),
        },
    );
    completed_error(env, &call_id, call, reason)
}

fn canonical_tool_name(name: &str) -> &str {
    match name {
        "ask" => "ask_user",
        other => other,
    }
}

/// Dispatch one registered, schema-valid tool call and return the
/// resulting [`ToolResultMessage`].
///
/// Lookup, search/file capability binding, argument validation,
/// cancellation, and tool errors are lifecycle failures: they
/// return **as an `is_error` tool result** so the loop continues and the
/// model can react. See `docs/agent.md`. Nothing here waits for a
/// Core permission prompt. Turns do not snapshot files before a tool runs.
///
/// Tools that declare `search_access` / `file_access` resolve the final
/// arguments once on a cancellable worker; the retained capability is passed
/// to execution and never re-resolved. Same-name plugin overrides remain
/// unbound unless they explicitly declare a search or file access mode.
pub(crate) async fn dispatch_tool_call(
    env: &TurnEnv<'_>,
    token: &CancellationToken,
    call: &ToolCall,
) -> ToolResultMessage {
    let call_id = CallId::from(call.id.as_str());
    emit(
        env,
        AgentEvent::ToolStarted {
            call_id: call_id.clone(),
            name: call.name.clone(),
            target: call.target(),
        },
    );

    let tool_name = canonical_tool_name(&call.name);
    let Some(tool) = env.tools.get(tool_name) else {
        return completed_error(env, &call_id, call, format!("unknown tool: {}", call.name));
    };

    let args = call.arguments.clone();
    let prepared = match bind_prepared(env, token, tool.as_ref(), &args).await {
        Ok(bound) => bound,
        Err(message) => return completed_error(env, &call_id, call, message),
    };

    // Execute. Progress items stream out live while the tool runs; the
    // dispatcher pushes the returned terminal result onto the tool stream
    // (first terminal wins, so a self-terminating tool
    // keeps its own result).
    let mut ctx = ToolCtx::new(env.cwd.clone())
        .with_cancel(token.clone())
        .with_call_id(call.id.clone());
    if let Some(search) = prepared.search {
        ctx = ctx.with_prepared_search(search);
    }
    if let Some(file) = prepared.file {
        ctx = ctx.with_prepared_file(file);
    }
    let (mut producer, mut consumer) = ToolStream::channel();
    let mut terminal_pusher = Some(producer.clone());
    let execute =
        CatchUnwind::new(async move { tool.execute_dyn(args, &ctx, &mut producer).await });
    tokio::pin!(execute);

    // Structured select: progress is consumed live, and dropping this
    // future drops `execute` instead of detaching a spawned task. A ready
    // execution is polled first so a clone that continuously sends progress
    // cannot starve the dispatcher's terminal claim. The stream's shared
    // state then stops new progress while channel FIFO preserves progress
    // already queued before the terminal. Poll panics and completion-path
    // Drop panics become owned-string Err values. Poll-panic cleanup
    // preserves its first error and discards a later destructor error;
    // cancel/abort Drop likewise discards destructor errors. Unknown panic
    // payloads are forgotten at the catch boundary so their Drop cannot
    // unwind the prompt.
    let mut exec_result = None;
    let mut streamed_terminal = None;
    loop {
        tokio::select! {
            biased;
            result = &mut execute, if exec_result.is_none() => {
                let result = match result {
                    Ok(Ok(value)) => value,
                    Ok(Err(err)) => ToolResult::error(err.to_string()),
                    Err(message) => panic_tool_result(message),
                };
                if let Some(pusher) = terminal_pusher.take() {
                    let _ = pusher.terminal(result.clone());
                }
                exec_result = Some(result);
            }
            item = consumer.recv() => {
                match item {
                    Some(ToolStreamItem::Progress(progress)) => emit(
                        env,
                        AgentEvent::ToolProgress {
                            call_id: call_id.clone(),
                            message: progress.message,
                        },
                    ),
                    Some(ToolStreamItem::Terminal(result)) => streamed_terminal = Some(result),
                    None => break,
                }
            }
        }
    }
    let result = streamed_terminal
        .or(exec_result)
        .unwrap_or_else(|| ToolResult::error("tool task ended without a result".to_owned()));

    let message = ToolResultMessage {
        tool_call_id: call.id.clone(),
        content: result.content,
        is_error: result.is_error,
        details: result.details,
    };
    emit(
        env,
        AgentEvent::ToolCompleted {
            call_id,
            result: message.clone(),
        },
    );
    // Yield so a ToolCompleted subscriber can commit the result before the
    // next call in this response is dispatched.
    tokio::task::yield_now().await;
    message
}

struct BoundPrepared {
    search: Option<std::sync::Arc<PreparedSearch>>,
    file: Option<std::sync::Arc<PreparedFile>>,
}

fn anchored_search(env: &TurnEnv<'_>, raw: Option<&str>) -> (std::path::PathBuf, Option<String>) {
    let Some(raw) = raw else {
        return (env.cwd.clone(), None);
    };
    let (root, relative) = mycode_tools::anchor_tool_path(&env.cwd, &env.extra_roots, raw);
    if root == env.cwd {
        return (env.cwd.clone(), Some(raw.to_owned()));
    }
    if relative.is_empty() {
        (root, None)
    } else {
        (root, Some(relative))
    }
}

fn anchored_file(env: &TurnEnv<'_>, raw: &str) -> (std::path::PathBuf, String) {
    let (root, relative) = mycode_tools::anchor_tool_path(&env.cwd, &env.extra_roots, raw);
    if root == env.cwd {
        (env.cwd.clone(), raw.to_owned())
    } else if relative.is_empty() {
        (root, ".".to_owned())
    } else {
        (root, relative)
    }
}

async fn bind_prepared(
    env: &TurnEnv<'_>,
    token: &CancellationToken,
    tool: &dyn ToolDyn,
    args: &serde_json::Value,
) -> Result<BoundPrepared, String> {
    if let Some(access) = tool.search_access() {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let (cwd, path) = anchored_search(env, path.as_deref());
        let prepared = prepare_search_async_with_access(cwd, path, token.clone(), access)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(BoundPrepared {
            search: Some(std::sync::Arc::new(prepared)),
            file: None,
        });
    }
    if let Some(access) = tool.file_access() {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "file tool is missing a path argument".to_owned())?;
        let (cwd, path) = anchored_file(env, path);
        let prepared = prepare_file_async(cwd, path, token.clone(), access)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(BoundPrepared {
            search: None,
            file: Some(std::sync::Arc::new(prepared)),
        });
    }
    Ok(BoundPrepared {
        search: None,
        file: None,
    })
}

/// Synthesize an `is_error` tool result, emit its `ToolCompleted` event,
/// and return it for the loop to write back into the context.
fn completed_error(
    env: &TurnEnv<'_>,
    call_id: &CallId,
    call: &ToolCall,
    reason: String,
) -> ToolResultMessage {
    let message = ToolResultMessage {
        tool_call_id: call.id.clone(),
        content: vec![ContentBlock::Text(reason.into())],
        is_error: true,
        details: None,
    };
    emit(
        env,
        AgentEvent::ToolCompleted {
            call_id: call_id.clone(),
            result: message.clone(),
        },
    );
    message
}

fn panic_tool_result(message: String) -> ToolResult {
    ToolResult::error(ToolError::PluginTrap(message).to_string())
}

/// Generic message used when the payload is not a `String` or `&str`.
///
/// Unknown payloads are forgotten rather than dropped, so this string is
/// the only text an unknown/non-string `panic_any` payload can force onto
/// the model.
const TOOL_PANIC_MESSAGE: &str = "tool panicked";

/// Copies a panic payload into an owned message at the catch boundary.
///
/// `String` and `&str` are safe to drop after the copy. Every other
/// payload is forgotten: a tool can `panic_any` a type whose `Drop`
/// panics, and dropping that box outside `catch_unwind` would unwind
/// the prompt task.
fn owned_panic_message(payload: Box<dyn Any + Send>) -> String {
    let payload = match payload.downcast::<String>() {
        Ok(text) => return *text,
        Err(payload) => payload,
    };
    let payload = match payload.downcast::<&str>() {
        Ok(text) => return (*text).to_owned(),
        Err(payload) => payload,
    };
    std::mem::forget(payload);
    TOOL_PANIC_MESSAGE.to_owned()
}

/// Runs `f` under `catch_unwind` and never lets a raw panic payload escape.
fn catch_unwind_message<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => Ok(value),
        Err(payload) => Err(owned_panic_message(payload)),
    }
}

/// Catches panics from a pinned tool future without detaching it.
///
/// The inner future lives in `Pin<Box<F>>` so poll and drop never move a
/// `!Unpin` future after it is pinned. `poll` and every `inner` destructor
/// run inside `catch_unwind`. Caught payloads are reduced to an owned
/// `String` before leaving the catch boundary; unknown payloads are
/// forgotten so a panicking `Drop` cannot unwind the prompt task. A
/// destructor panic after `Poll::Ready` is returned as `Err`. After a poll
/// panic, cleanup preserves that first error and discards a later destructor
/// panic; the wrapper's `Drop` (cancel/abort) also discards a destructor
/// panic so it cannot unwind the prompt task.
struct CatchUnwind<F> {
    inner: Option<Pin<Box<F>>>,
}

impl<F> CatchUnwind<F> {
    fn new(inner: F) -> Self {
        Self {
            inner: Some(Box::pin(inner)),
        }
    }

    /// Drops `F` at its pinned heap address. Moving `Pin<Box<F>>` moves the
    /// pointer, not `F`. The result is an owned message, never a raw payload.
    fn drop_inner(inner: Pin<Box<F>>) -> Result<(), String> {
        catch_unwind_message(move || drop(inner))
    }
}

impl<F> Drop for CatchUnwind<F> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            // The result is already an owned `String`; dropping it is safe.
            drop(Self::drop_inner(inner));
        }
    }
}

impl<F: Future> Future for CatchUnwind<F> {
    type Output = Result<F::Output, String>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // `Pin<Box<F>>` is always `Unpin`, so the wrapper is `Unpin`.
        let this = self.get_mut();
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(Err(TOOL_PANIC_MESSAGE.to_owned()));
        };
        match catch_unwind_message(|| inner.as_mut().poll(cx)) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(output)) => match this.inner.take() {
                Some(inner) => match Self::drop_inner(inner) {
                    Ok(()) => Poll::Ready(Ok(output)),
                    Err(message) => Poll::Ready(Err(message)),
                },
                None => Poll::Ready(Ok(output)),
            },
            Err(message) => {
                if let Some(inner) = this.inner.take() {
                    drop(Self::drop_inner(inner));
                }
                Poll::Ready(Err(message))
            }
        }
    }
}
