//! The ledger pump: folds the agent's event bus into ledger writes and
//! bridge events. It owns the branch head for the life of the turn: tool
//! calls and results commit as they happen, tool-requesting steps commit
//! when they arrive, and the closing text reply commits at turn end.

use std::sync::Arc;

use mycode_agent::session::EventKind;
use mycode_core::events::{AgentEvent, MessageDelta, TurnOutcome};
use mycode_core::{AssistantMessage, ContentBlock, Message, UserMessage};
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
    pub(super) session_id: String,
    /// Summaries the request hook produced mid-turn; published after the
    /// closing reply so a card cannot split a tool call from its result.
    pub(super) deferred: DeferredSummaries,
    pub(super) cancel: CancellationToken,
    /// Model id the usage meter names.
    pub(super) usage_model: String,
    pub(super) started: std::time::Instant,
}

impl LedgerPump {
    /// Runs until the turn ends. Every exit sends exactly one terminal event
    /// (`ChatDone` or `ChatFailed`) so the desktop can leave the sending state.
    pub(super) async fn run(self, agent_rx: broadcast::Receiver<AgentEvent>) {
        let mut agent_rx = relay(agent_rx);
        // A text-only reply is held until the turn ends: a later step or a
        // steer may still follow it.
        let mut pending_assistant: Option<Arc<Message>> = None;
        let mut last_step: Option<ConversationEntry> = None;
        let mut turn_usage = TurnUsage::default();
        // The opening user message is already in the ledger. Later user
        // messages are steers injected into this same turn.
        let mut saw_opening_user = false;
        loop {
            let event = match agent_rx.recv().await {
                Some(Ok(event)) => event,
                Some(Err(RecvError::Lagged(_))) => {
                    self.fail("agent event stream lagged").await;
                    return;
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
                    self.fail(message).await;
                    return;
                }
            };
            match event {
                AgentEvent::MessageDelta(MessageDelta::TextDelta(delta)) => {
                    self.send(BridgeEvent::ChatText {
                        session_id: self.session_id.clone(),
                        delta,
                    });
                }
                AgentEvent::MessageDelta(MessageDelta::ThinkingDelta(delta)) => {
                    self.send(BridgeEvent::ChatThinking {
                        session_id: self.session_id.clone(),
                        delta,
                    });
                }
                AgentEvent::MessageDelta(MessageDelta::ToolCallDelta { .. })
                | AgentEvent::TurnStarted => {}
                AgentEvent::ToolStarted {
                    call_id,
                    name,
                    target,
                } => {
                    let spelling = call_id.to_string();
                    // The ToolCall event must commit before its result; the
                    // ledger's ordering check rejects results for calls that
                    // were never opened.
                    if let Err(error) = self.writer.open_call(&spelling, &name, &target).await {
                        self.fail(render_error(error)).await;
                        return;
                    }
                    self.send(BridgeEvent::ToolStarted {
                        session_id: self.session_id.clone(),
                        call_id: spelling,
                        name,
                        target,
                    });
                }
                AgentEvent::ToolProgress { call_id, message } => {
                    self.send(BridgeEvent::ToolProgress {
                        session_id: self.session_id.clone(),
                        call_id: call_id.to_string(),
                        name: String::new(),
                        message,
                    });
                }
                AgentEvent::ToolCompleted {
                    call_id,
                    result: tool_result,
                } => {
                    let Ok(payload) = serde_json::to_vec(&tool_result) else {
                        self.fail("tool result could not be encoded").await;
                        return;
                    };
                    match self.writer.close_call(call_id.as_str(), &payload).await {
                        Ok(event_id) => self.send(BridgeEvent::ToolCompleted {
                            session_id: self.session_id.clone(),
                            entry: project_tool_result_message(&event_id, &tool_result),
                        }),
                        Err(error) => {
                            self.fail(render_error(error)).await;
                            return;
                        }
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
                            self.fail(failure).await;
                            return;
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
                                    session_id: self.session_id.clone(),
                                    entry,
                                });
                            }
                            Err(failure) => {
                                self.fail(failure).await;
                                return;
                            }
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
                    self.fail(message).await;
                    return;
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
                        // assistant message" used to clear the live bubble
                        // after the step was the whole turn.
                        None => Ok(last_step.take()),
                    };
                    match closing {
                        Ok(Some(entry)) => self.finish(entry).await,
                        Ok(None) => {
                            self.publish_deferred().await;
                            self.fail("the turn ended without an assistant message")
                                .await;
                        }
                        Err(failure) => {
                            self.publish_deferred().await;
                            self.fail(failure).await;
                        }
                    }
                    return;
                }
                AgentEvent::Error(error) => {
                    // Best effort: the error is the report either way.
                    let _ = self.commit_held_step(pending_assistant.take()).await;
                    self.publish_deferred().await;
                    self.fail(format!("agent error: {error}")).await;
                    return;
                }
            }
        }
    }

    fn send(&self, event: BridgeEvent) {
        let _ = self.events.send(event);
    }

    /// Ends the turn as failed. The agent is cancelled too: a pump that
    /// stops recording (a lagged bus, a failed ledger write) must not leave
    /// the model running tools that nothing records and the UI no longer
    /// shows. After a terminal agent event the cancel is a no-op.
    async fn fail(&self, message: impl Into<String>) {
        self.cancel.cancel();
        emit_chat_failed(&self.events, &self.writer, &self.session_id, message).await;
    }

    async fn publish_deferred(&self) {
        self.deferred
            .publish(&self.writer, &self.events, &self.session_id)
            .await;
    }

    fn send_usage(&self, usage: &TurnUsage) {
        self.send(BridgeEvent::UsageSnapshot {
            session_id: self.session_id.clone(),
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
            session_id: self.session_id.clone(),
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
            session_id: self.session_id.clone(),
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
    async fn finish(&self, entry: ConversationEntry) {
        self.publish_deferred().await;
        self.send(BridgeEvent::ChatDone {
            session_id: self.session_id.clone(),
            head: self.writer.head().await,
            entry,
        });
    }
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

async fn emit_chat_failed(
    events: &crate::BridgeEventTx,
    writer: &HeadWriter,
    session_id: &str,
    message: impl Into<String>,
) {
    let _ = events.send(BridgeEvent::ChatFailed {
        session_id: session_id.to_owned(),
        message: message.into(),
        head: writer.head().await,
    });
}

/// Token usage summed across every response cycle of one turn.
///
/// Providers report usage per cycle, so a turn that calls tools reports
/// several times. The sums feed the live [`BridgeEvent::UsageSnapshot`]
/// meter; nothing is persisted to the ledger.
#[derive(Debug, Default)]
struct TurnUsage {
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
    fn fold(&mut self, usage: &mycode_core::Usage) {
        self.requests = self.requests.saturating_add(1);
        self.input = self.input.saturating_add(usage.input_tokens);
        let prompt = if usage.prompt_tokens > 0 {
            usage.prompt_tokens
        } else {
            usage.input_tokens
        };
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
    fn turn_usage_sums_every_request_but_meters_the_latest_prompt() {
        let mut usage = super::TurnUsage::default();
        for prompt in [40_000, 52_000, 61_000] {
            usage.fold(&mycode_core::Usage {
                input_tokens: prompt,
                output_tokens: 500,
                cache_read_tokens: Some(prompt - 1_000),
                cache_write_tokens: None,
                prompt_tokens: prompt,
            });
        }
        assert_eq!(usage.requests, 3);
        assert_eq!(usage.input, 153_000, "billing sums every tool round");
        assert_eq!(
            usage.latest_input, 61_000,
            "the context meter is one prompt"
        );
        assert_eq!(usage.latest_cache, 60_000);
        assert_eq!(usage.output, 1_500);
    }
}
