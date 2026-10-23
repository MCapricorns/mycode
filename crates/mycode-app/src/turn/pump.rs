//! The ledger pump: folds the agent's event bus into ledger writes and
//! bridge events. It owns the branch head for the life of the turn: tool
//! calls and results commit as they happen, tool-requesting steps commit
//! when they arrive, and the closing text reply commits at turn end.

use std::sync::Arc;

use mycode_agent::events::{AgentEvent, MessageDelta, TurnOutcome};
use mycode_agent::session::EventKind;
use mycode_agent::{AssistantMessage, ContentBlock, Message, UserMessage};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::BridgeEvent;
use crate::ledger::{HeadWriter, render_error};
use crate::projection::{project_assistant_message, project_tool_result_message};
use crate::protocol::{CHAT_CANCELLED, ConversationEntry, EntryKind};

use super::compact::{DeferredSummaries, watch_prompt_cache};

/// Everything the pump needs besides the agent bus.
pub(super) struct LedgerPump {
    pub(super) events: crate::BridgeEventTx,
    pub(super) writer: HeadWriter,
    /// Session identity spelling. Shared so the per-fragment events
    /// (`ChatText`/`ChatThinking`) route without a String copy per token.
    pub(super) session_id: Arc<str>,
    /// Summaries the request hook produced mid-turn; published after the
    /// closing reply so a card cannot split a tool call from its result.
    pub(super) deferred: DeferredSummaries,
    pub(super) cancel: CancellationToken,
    /// Model id the usage meter names.
    pub(super) usage_model: String,
    pub(super) started: std::time::Instant,
}

impl LedgerPump {
    /// Runs until the turn ends and returns its one terminal event
    /// (`ChatDone` or `ChatFailed`). The turn task sends it once the turn's
    /// controls are unpublished, so the desktop that hears it never reopens
    /// the session onto a turn the core still lists as running.
    pub(super) async fn run(self, agent_rx: broadcast::Receiver<AgentEvent>) -> BridgeEvent {
        let mut agent_rx = relay(agent_rx);
        // A text-only reply is held until the turn ends or a steer commits
        // it: a later step or a steer may still follow it.
        let mut pending_assistant: Option<Arc<Message>> = None;
        let mut last_step: Option<ConversationEntry> = None;
        let mut turn_usage = TurnUsage::default();
        // The opening user message is already in the ledger. Later user
        // messages are steers injected into this same turn.
        let mut saw_opening_user = false;
        // `agent` calls the model is still writing, by provider call id.
        let mut preparing = PreparingCalls::default();
        loop {
            let event = match agent_rx.recv().await {
                Some(Ok(event)) => event,
                Some(Err(RecvError::Lagged(_))) => {
                    return self.fail("agent event stream lagged").await;
                }
                Some(Err(RecvError::Closed)) | None => {
                    // Every terminal event returns below, so a closed bus
                    // means the agent stopped before its turn started: a
                    // cancel during the first request's hook ends the
                    // prompt without TurnStarted or TurnEnded. The desktop
                    // still needs one terminal to leave the sending state.
                    let message = if self.cancel.is_cancelled() {
                        CHAT_CANCELLED
                    } else {
                        "the turn ended before it started"
                    };
                    return self.fail(message).await;
                }
            };
            match event {
                AgentEvent::MessageDelta(MessageDelta::TextDelta(delta)) => {
                    self.send(BridgeEvent::ChatText {
                        session_id: Arc::clone(&self.session_id),
                        delta,
                    });
                }
                AgentEvent::MessageDelta(MessageDelta::ThinkingDelta(delta)) => {
                    self.send(BridgeEvent::ChatThinking {
                        session_id: Arc::clone(&self.session_id),
                        delta,
                    });
                }
                AgentEvent::MessageDelta(MessageDelta::ToolCallStarted { id, name }) => {
                    if let Some(target) = preparing.open(&id, &name) {
                        self.send_preparing(id, name, target);
                    }
                }
                AgentEvent::MessageDelta(MessageDelta::ToolCallDelta { id, partial_json }) => {
                    if let Some(target) = preparing.extend(&id, &partial_json) {
                        self.send_preparing(id, AGENT_TOOL.to_owned(), target);
                    }
                }
                AgentEvent::TurnStarted => {}
                AgentEvent::ToolStarted {
                    call_id,
                    name,
                    target,
                } => {
                    let provider_call_id = call_id.to_string();
                    preparing.close(&provider_call_id);
                    // The ToolCall event must commit before its result; the
                    // ledger's ordering check rejects results for calls that
                    // were never opened.
                    let identity = match self
                        .writer
                        .open_call(&provider_call_id, &name, &target)
                        .await
                    {
                        Ok(identity) => identity,
                        Err(error) => return self.fail(render_error(error)).await,
                    };
                    self.send(BridgeEvent::ToolStarted {
                        session_id: self.session_id.to_string(),
                        call_id: identity.as_str().to_owned(),
                        provider_call_id,
                        name,
                        target,
                    });
                }
                AgentEvent::ToolProgress { call_id, message } => {
                    // Progress rides the transcript's ledger identity so the
                    // live job it updates matches its rows. A provider id the
                    // open-call map no longer holds is a straggler from a call
                    // that already closed — forwarding it would resurrect a
                    // finished job under an id no result can drop.
                    let provider_call_id = call_id.to_string();
                    let Some(identity) = self.writer.call_identity(&provider_call_id).await else {
                        continue;
                    };
                    self.send(BridgeEvent::ToolProgress {
                        session_id: self.session_id.to_string(),
                        call_id: identity,
                        provider_call_id,
                        name: String::new(),
                        message,
                    });
                }
                AgentEvent::ToolCompleted { call_id, result } => {
                    let Message::ToolResult(tool_result) = result.as_ref() else {
                        continue;
                    };
                    let Ok(payload) = serde_json::to_vec(tool_result) else {
                        return self.fail("tool result could not be encoded").await;
                    };
                    match self.writer.close_call(call_id.as_str(), &payload).await {
                        Ok((event_id, identity)) => {
                            let mut entry = project_tool_result_message(&event_id, tool_result);
                            // The payload's tool_call_id is the provider id;
                            // live rows pair on the ledger identity the way
                            // replayed rows do.
                            entry.call_id = Some(identity);
                            self.send(BridgeEvent::ToolCompleted {
                                session_id: self.session_id.to_string(),
                                entry,
                            });
                        }
                        Err(error) => return self.fail(render_error(error)).await,
                    }
                }
                AgentEvent::MessageAdded(message) => match message.as_ref() {
                    Message::User(user) => {
                        if !saw_opening_user {
                            saw_opening_user = true;
                            continue;
                        }
                        let text = plain_user_text(user);
                        if text.trim().is_empty() {
                            continue;
                        }
                        if let Err(failure) =
                            self.commit_steer(text, pending_assistant.take()).await
                        {
                            return self.fail(failure).await;
                        }
                    }
                    Message::Assistant(assistant) => {
                        // Usage is reported per response cycle, so it has to
                        // be summed here: reading it off the closing message
                        // alone would bill a ten-step turn as one.
                        if let Some(usage) = assistant.usage.as_ref() {
                            turn_usage.fold(usage);
                            self.send_usage(&turn_usage);
                        }
                        // A step that requests tools is not the end of the
                        // turn. Commit it now so the ledger and the transcript
                        // keep the model's real order instead of collapsing
                        // the turn into its last message.
                        let is_step = assistant
                            .blocks
                            .iter()
                            .any(|block| matches!(block, ContentBlock::ToolCall(_)));
                        if !is_step {
                            pending_assistant = Some(message);
                            continue;
                        }
                        match self.commit_assistant(assistant).await {
                            Ok(entry) => {
                                last_step = Some(entry.clone());
                                self.send(BridgeEvent::AssistantStep {
                                    session_id: self.session_id.to_string(),
                                    entry,
                                });
                            }
                            Err(failure) => return self.fail(failure).await,
                        }
                    }
                    Message::ToolResult(_) => {}
                },
                AgentEvent::CacheDisrupted => {
                    watch_prompt_cache(
                        &self.events,
                        &self.session_id,
                        crate::CacheDisruption::Compaction,
                    );
                }
                AgentEvent::Compacting(active) => {
                    self.send(BridgeEvent::ContextCompacting {
                        session_id: self.session_id.to_string(),
                        active,
                    });
                }
                AgentEvent::TurnEnded(TurnOutcome::Aborted) => {
                    // A cancel keeps the partial. Write it when it has not
                    // already been committed as a tool step, then fail
                    // quietly so the desktop can send the queue. Never
                    // ChatDone: that would look like a finished turn.
                    let message = match self.commit_held_step(pending_assistant.take()).await {
                        Ok(()) => CHAT_CANCELLED.to_owned(),
                        Err(failure) => failure,
                    };
                    // A checkpoint written mid-turn is already what the next
                    // request sends; its card must not vanish with the turn.
                    self.publish_deferred().await;
                    return self.fail(message).await;
                }
                AgentEvent::TurnEnded(_) => {
                    let closing = match pending_assistant.take() {
                        Some(held) => match held.as_ref() {
                            Message::Assistant(assistant) => {
                                self.commit_assistant(assistant).await.map(Some)
                            }
                            _ => Ok(None),
                        },
                        // A failed stream that already committed a tool step
                        // (or any earlier step) is done. Reporting "no
                        // assistant message" would clear a live bubble whose
                        // step was the whole turn.
                        None => Ok(last_step.take()),
                    };
                    return match closing {
                        Ok(Some(entry)) => self.finish(entry).await,
                        Ok(None) => {
                            self.publish_deferred().await;
                            self.fail("the turn ended without an assistant message")
                                .await
                        }
                        Err(failure) => {
                            self.publish_deferred().await;
                            self.fail(failure).await
                        }
                    };
                }
                AgentEvent::Error(error) => {
                    // Best effort: the error is the report either way.
                    let _ = self.commit_held_step(pending_assistant.take()).await;
                    self.publish_deferred().await;
                    return self.fail(format!("agent error: {error}")).await;
                }
            }
        }
    }

    fn send(&self, event: BridgeEvent) {
        let _ = self.events.send(event);
    }

    fn send_preparing(&self, provider_call_id: String, name: String, target: String) {
        self.send(BridgeEvent::ToolPreparing {
            session_id: self.session_id.to_string(),
            provider_call_id,
            name,
            target,
        });
    }

    /// Ends the turn as failed. The agent is cancelled too: a pump that
    /// stops recording (a lagged bus, a failed ledger write) must not leave
    /// the model running tools that nothing records and the UI no longer
    /// shows. After a terminal agent event the cancel is a no-op.
    async fn fail(&self, message: impl Into<String>) -> BridgeEvent {
        self.cancel.cancel();
        BridgeEvent::ChatFailed {
            session_id: self.session_id.to_string(),
            message: message.into(),
            head: self.writer.head().await,
        }
    }

    async fn publish_deferred(&self) {
        self.deferred
            .publish(&self.writer, &self.events, &self.session_id)
            .await;
    }

    fn send_usage(&self, usage: &TurnUsage) {
        self.send(BridgeEvent::UsageSnapshot {
            session_id: self.session_id.to_string(),
            model: self.usage_model.clone(),
            input: usage.input,
            context: usage.latest_input,
            context_cache: usage.latest_cache,
            output: usage.output,
            cache: usage.cache,
            requests: usage.requests,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
        });
    }

    /// Writes one assistant message and returns its transcript row.
    async fn commit_assistant(
        &self,
        assistant: &AssistantMessage,
    ) -> Result<ConversationEntry, String> {
        let payload = serde_json::to_vec(assistant)
            .map_err(|_| "assistant message could not be encoded".to_owned())?;
        let event_id = self
            .writer
            .write(EventKind::Message, &payload)
            .await
            .map_err(render_error)?;
        Ok(project_assistant_message(&event_id, assistant))
    }

    /// Commits a held text reply as a transcript step, when there is one.
    async fn commit_held_step(&self, held: Option<Arc<Message>>) -> Result<(), String> {
        let Some(held) = held else {
            return Ok(());
        };
        let Message::Assistant(assistant) = held.as_ref() else {
            return Ok(());
        };
        let entry = self.commit_assistant(assistant).await?;
        self.send(BridgeEvent::AssistantStep {
            session_id: self.session_id.to_string(),
            entry,
        });
        Ok(())
    }

    /// Commits one steer the agent injected into this turn. A held text
    /// reply lands first: the steer continues that turn, so the reply has to
    /// precede it or the ledger order flips.
    async fn commit_steer(&self, text: String, held: Option<Arc<Message>>) -> Result<(), String> {
        self.commit_held_step(held).await?;
        let event_id = self
            .writer
            .write(EventKind::Message, text.as_bytes())
            .await
            .map_err(render_error)?;
        self.send(BridgeEvent::SteerCommitted {
            session_id: self.session_id.to_string(),
            head: self.writer.head().await,
            entry: ConversationEntry {
                event_id,
                kind: EntryKind::UserMessage,
                text: text.into(),
                call_id: None,
                thinking: String::new(),
            },
        });
        // The steer is already in the next request. Arm before that
        // request's usage snapshot.
        watch_prompt_cache(
            &self.events,
            &self.session_id,
            crate::CacheDisruption::Steer,
        );
        Ok(())
    }

    /// Publishes the deferred summary cards, then ends the turn with
    /// `entry`. The cards follow the closing reply, so they cannot split a
    /// tool call from its result; ChatDone then carries the head that
    /// includes them.
    async fn finish(&self, entry: ConversationEntry) -> BridgeEvent {
        self.publish_deferred().await;
        BridgeEvent::ChatDone {
            session_id: self.session_id.to_string(),
            head: self.writer.head().await,
            entry,
        }
    }
}

/// Name of the delegation tool, whose calls get a card while still streaming.
const AGENT_TOOL: &str = "agent";

/// `agent` calls the model is still writing. Their briefs can run to
/// thousands of characters, so each gets a card when it opens instead of
/// all of them appearing at once when the response ends.
#[derive(Default)]
struct PreparingCalls {
    /// Calls still being written, by provider call id.
    calls: std::collections::HashMap<String, PreparingCall>,
}

#[derive(Default)]
struct PreparingCall {
    /// Arguments so far; dropped once the label is final.
    arguments: String,
    /// The label last sent.
    shown: String,
    /// Role and description were both read: the rest of the brief cannot
    /// change the label, so later bytes are neither kept nor scanned.
    settled: bool,
}

impl PreparingCalls {
    /// Tracks a newly opened call; returns its first (empty) label when it
    /// is an `agent` call.
    fn open(&mut self, id: &str, name: &str) -> Option<String> {
        if name != AGENT_TOOL || id.is_empty() {
            return None;
        }
        self.calls.insert(id.to_owned(), PreparingCall::default());
        Some(String::new())
    }

    /// Appends argument bytes; returns the label when it just changed.
    fn extend(&mut self, id: &str, partial_json: &str) -> Option<String> {
        let call = self.calls.get_mut(id)?;
        if call.settled {
            return None;
        }
        call.arguments.push_str(partial_json);
        let role = partial_string_field(&call.arguments, "agent");
        let description = partial_string_field(&call.arguments, "description");
        if role.is_some() && description.is_some() {
            call.settled = true;
            call.arguments = String::new();
        }
        let known = serde_json::json!({
            "agent": role.unwrap_or_default(),
            "description": description.unwrap_or_default(),
        });
        let target = mycode_agent::tool_target(AGENT_TOOL, &known);
        if target == call.shown {
            return None;
        }
        call.shown.clone_from(&target);
        Some(target)
    }

    /// The call started for real; its card takes over.
    fn close(&mut self, id: &str) {
        self.calls.remove(id);
    }
}

/// A top-level string field of a JSON object that is still streaming, once
/// its value is complete. A key spelled inside another string is escaped
/// there (`\"agent\"`), so the quoted key only matches a real key.
fn partial_string_field(json: &str, key: &str) -> Option<String> {
    let quoted = format!("\"{key}\"");
    let mut from = 0;
    while let Some(found) = json[from..].find(&quoted) {
        let after = from + found + quoted.len();
        from = after;
        let rest = json[after..].trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        let rest = rest.trim_start();
        if !rest.starts_with('"') {
            continue;
        }
        let mut escaped = false;
        for (index, ch) in rest.char_indices().skip(1) {
            match ch {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => {
                    return serde_json::from_str::<String>(&rest[..=index]).ok();
                }
                _ => escaped = false,
            }
        }
        return None;
    }
    None
}

/// Moves the agent bus onto an unbounded queue as fast as it fills.
///
/// The pump awaits a ledger fsync for every tool call, result, and step.
/// Meanwhile a chatty tool or a few subagents can publish hundreds of
/// progress lines, and a lagged broadcast receiver loses events for good —
/// an assistant step or a tool result among them. The relay only moves
/// events, so it keeps up; the queue absorbs the burst.
fn relay(
    mut agent_rx: broadcast::Receiver<AgentEvent>,
) -> mpsc::UnboundedReceiver<Result<AgentEvent, RecvError>> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let item = agent_rx.recv().await;
            let closed = matches!(item, Err(RecvError::Closed));
            if tx.send(item).is_err() || closed {
                return;
            }
        }
    });
    rx
}

/// Token usage summed across every response cycle of one turn.
///
/// Providers report usage per cycle, so a turn that calls tools reports
/// several times. The sums feed the live [`BridgeEvent::UsageSnapshot`]
/// meter; nothing is persisted to the ledger.
#[derive(Debug, Default)]
struct TurnUsage {
    /// Fresh (non-cache-read) prompt tokens summed over the turn. Derived
    /// uniformly from `prompt_tokens` minus the cache read, so an
    /// OpenAI-style `input_tokens` that already includes the cached
    /// context does not bill every re-sent round as new input.
    input: u64,
    /// Most recent request's prompt tokens. The context meter uses this
    /// instead of `input`, which sums every tool round.
    latest_input: u64,
    output: u64,
    cache: Option<u64>,
    /// Cache-read tokens on the latest prompt. The context meter shows this
    /// beside the window, separate from [`Self::cache`], which is the sum.
    latest_cache: u64,
    /// Response cycles folded in.
    requests: u32,
}

impl TurnUsage {
    fn fold(&mut self, usage: &mycode_agent::Usage) {
        self.requests = self.requests.saturating_add(1);
        let prompt = if usage.prompt_tokens > 0 {
            usage.prompt_tokens
        } else {
            usage.input_tokens
        };
        self.input = self
            .input
            .saturating_add(prompt.saturating_sub(usage.cache_read_tokens.unwrap_or(0)));
        if prompt > 0 {
            self.latest_input = prompt;
            self.latest_cache = usage.cache_read_tokens.unwrap_or(0);
        }
        self.output = self.output.saturating_add(usage.output_tokens);
        if let Some(cache) = usage.cache_read_tokens {
            self.cache = Some(self.cache.unwrap_or_default().saturating_add(cache));
        }
    }
}

pub(super) fn plain_user_text(user: &UserMessage) -> String {
    let mut text = String::new();
    for block in &user.content {
        if let ContentBlock::Text(block) = block {
            text.push_str(&block.text);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_agent_card_names_its_role_and_brief_while_the_call_streams() {
        let mut preparing = super::PreparingCalls::default();
        assert_eq!(preparing.open("call_1", "read"), None);
        assert_eq!(preparing.open("call_2", "agent").as_deref(), Some(""));
        assert_eq!(
            preparing.extend("call_1", "{}"),
            None,
            "only agent calls are tracked"
        );
        assert_eq!(
            preparing.extend("call_2", r#"{"prompt":"say \"agent\": \"x\""#),
            None
        );
        assert_eq!(preparing.extend("call_2", r#"", "agent": "sco"#), None);
        assert_eq!(
            preparing
                .extend("call_2", r#"ut", "description": "map the "#)
                .as_deref(),
            Some("scout")
        );
        assert_eq!(
            preparing.extend("call_2", r#"crate"}"#).as_deref(),
            Some("scout map the crate")
        );
        assert_eq!(
            preparing.extend("call_2", " "),
            None,
            "an unchanged label is not resent"
        );
        assert!(
            preparing.calls["call_2"].arguments.is_empty(),
            "a settled brief is no longer buffered"
        );
        preparing.close("call_2");
        assert_eq!(preparing.extend("call_2", "x"), None);
    }

    #[test]
    fn turn_usage_sums_fresh_input_but_meters_the_latest_prompt() {
        // OpenAI-style usage counts the cache read inside `input_tokens`,
        // so every tool round re-reports the whole re-sent prompt. The
        // sum keeps only the fresh part: prompt minus the cache read.
        let mut usage = super::TurnUsage::default();
        for prompt in [40_000, 52_000, 61_000] {
            usage.fold(&mycode_agent::Usage {
                input_tokens: prompt,
                output_tokens: 500,
                cache_read_tokens: Some(prompt - 1_000),
                cache_write_tokens: None,
                prompt_tokens: prompt,
            });
        }
        assert_eq!(usage.requests, 3);
        assert_eq!(usage.input, 3_000, "the sum is fresh input, not cache");
        assert_eq!(
            usage.latest_input, 61_000,
            "the context meter is one prompt"
        );
        assert_eq!(usage.latest_cache, 60_000);
        assert_eq!(usage.output, 1_500);
    }

    #[test]
    fn turn_usage_fresh_input_matches_a_cache_excluding_provider() {
        // Anthropic- and DeepSeek-style `input_tokens` already exclude the
        // cache read; `prompt_tokens` is input plus cache. Prompt minus
        // cache lands on the same fresh figure the provider billed.
        let mut usage = super::TurnUsage::default();
        for (input, cache_read) in [(100, 11_800), (250, 12_050)] {
            usage.fold(&mycode_agent::Usage {
                input_tokens: input,
                output_tokens: 10,
                cache_read_tokens: Some(cache_read),
                cache_write_tokens: None,
                prompt_tokens: input + cache_read,
            });
        }
        assert_eq!(usage.requests, 2);
        assert_eq!(usage.input, 350);
        assert_eq!(usage.latest_input, 12_300);
        assert_eq!(usage.latest_cache, 12_050);
        assert_eq!(usage.cache, Some(23_850));
    }
}
