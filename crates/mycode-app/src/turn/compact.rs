//! Compaction around a turn: `/compact` on demand, and the transcript cards
//! for summaries written before or during a turn.

use std::sync::Arc;

use mycode_agent::session::{BranchId, EventKind, HeadStamp, SessionId};
use mycode_providers::{ReqwestTransport, ResolvedProvider, SseTransport, WireProvider};
use tokio_util::sync::CancellationToken;

use crate::BridgeEvent;
use crate::ledger::{
    HEAD_MOVED_ON, HeadWriter, branch_head, head_spelling, ledger_history, render_error,
};
use crate::oauth::resolve_request_auth;
use crate::state::{CoreState, model_context_window};

use super::TurnControl;
use super::turn_credentials;

/// Summarizes the open session on `expected_head` and stores a checkpoint
/// the next turn will send instead of the full history.
///
/// The cancel token is published before the returned future is first polled,
/// so a subsequent `ChatTurn`, `CancelChat`, or `DeleteSession` can cancel an
/// in-flight manual compaction immediately.
pub(crate) fn manual_compact(
    state: Arc<CoreState>,
    events: crate::BridgeEventTx,
    session: SessionId,
    branch: BranchId,
    expected_head: HeadStamp,
    provider_id: String,
    model: String,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let session_id = session.as_str().to_owned();
    let control = TurnControl::register_cancel_only(&state, &session_id);
    async move {
        let outcome = compact_session_now(
            &state,
            &session,
            &branch,
            &expected_head,
            &provider_id,
            &model,
            &control.cancel,
        )
        .await;
        // The card write advances the durable head; report it so the UI's
        // next `expected_head` does not collide with its own summary row.
        let (ok, message, head) = match outcome {
            Ok((message, Some(summary))) => {
                match show_compaction_summary(
                    &state,
                    &events,
                    &session,
                    &branch,
                    &expected_head,
                    &session_id,
                    &summary,
                )
                .await
                {
                    Ok(head) => (true, message, head),
                    Err(error) => (false, error, head_spelling(&expected_head)),
                }
            }
            Ok((message, None)) if message == "empty" || message == "covered" => {
                (true, message, head_spelling(&expected_head))
            }
            Ok((_, None)) => (
                false,
                "compaction finished without a summary".to_owned(),
                head_spelling(&expected_head),
            ),
            Err(message) => (false, message, head_spelling(&expected_head)),
        };
        let _ = events.send(BridgeEvent::CompactFinished {
            session_id,
            message,
            ok,
            head,
        });
    }
}

/// Writes the summary into the ledger as a transcript row and tells the UI.
/// The model history skips this row; the checkpoint still builds the request.
/// Returns the head spelling after the card commit.
async fn show_compaction_summary(
    state: &CoreState,
    events: &crate::BridgeEventTx,
    session: &SessionId,
    branch: &BranchId,
    expected_head: &HeadStamp,
    session_id: &str,
    summary: &str,
) -> Result<String, String> {
    let head = open_branch_head(state, session, branch, expected_head).await?;
    let writer = HeadWriter::new(state.service.clone(), session.clone(), branch.clone(), head);
    publish_visible_summary(&writer, events, session_id, summary, true).await?;
    Ok(writer.head().await)
}

/// Summaries produced inside `before_request`. The hook must not append a
/// ledger row: that write lands between the turn's tool calls and their
/// results and breaks the head the turn writer is committing. The pump
/// publishes the card after the assistant message (and usage) commit.
#[derive(Clone, Default)]
pub(super) struct DeferredSummaries {
    pending: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl DeferredSummaries {
    /// Stores a summary for the deferred transcript card.
    ///
    /// Returns whether a new summary was stored. The caller signals
    /// [`mycode_core::events::AgentEvent::CacheDisrupted`] on the agent bus
    /// in that case, so the pump forwards the cache watch in bus order.
    pub(super) async fn stash(&self, summary: Option<String>) -> bool {
        if let Some(summary) = summary {
            self.pending.lock().await.push(summary);
            true
        } else {
            false
        }
    }

    pub(super) async fn publish(
        &self,
        writer: &HeadWriter,
        events: &crate::BridgeEventTx,
        session_id: &str,
    ) {
        let summaries = {
            let mut pending = self.pending.lock().await;
            std::mem::take(&mut *pending)
        };
        for summary in summaries {
            // `false`: stash already armed the watch, before this request.
            // Arming again from the late card would compare the next turn
            // with the post-compaction prompt.
            let _ = publish_visible_summary(writer, events, session_id, &summary, false).await;
        }
    }
}

pub(super) fn watch_prompt_cache(
    events: &crate::BridgeEventTx,
    session_id: &str,
    cause: crate::CacheDisruption,
) {
    let _ = events.send(BridgeEvent::PromptCacheWatch {
        session_id: session_id.to_owned(),
        cause,
    });
}

pub(super) async fn publish_visible_summary(
    writer: &HeadWriter,
    events: &crate::BridgeEventTx,
    session_id: &str,
    summary: &str,
    watch_cache: bool,
) -> Result<(), String> {
    let payload = crate::compaction::display_summary_text(summary);
    let event_id = writer
        .write(EventKind::Message, payload.as_bytes())
        .await
        .map_err(|error| {
            format!(
                "could not write the compaction summary: {}",
                render_error(error)
            )
        })?;
    let entry = crate::protocol::ConversationEntry {
        event_id,
        kind: crate::protocol::EntryKind::UserMessage,
        text: payload.into(),
        call_id: None,
        thinking: String::new(),
    };
    if watch_cache {
        watch_prompt_cache(events, session_id, crate::CacheDisruption::Compaction);
    }
    let _ = events.send(BridgeEvent::SummaryShown {
        session_id: session_id.to_owned(),
        entry,
    });
    Ok(())
}

/// The branch head, which must still be `expected_head`: a summary of a
/// history the user no longer sees would be stale.
async fn open_branch_head(
    state: &CoreState,
    session: &SessionId,
    branch: &BranchId,
    expected_head: &HeadStamp,
) -> Result<HeadStamp, String> {
    let head = branch_head(&state.service, session, branch)
        .await
        .map_err(|error| format!("could not open the session: {}", render_error(error)))?;
    if &head == expected_head {
        Ok(head)
    } else {
        Err(HEAD_MOVED_ON.to_owned())
    }
}

async fn compact_session_now(
    state: &CoreState,
    session: &SessionId,
    branch: &BranchId,
    expected_head: &HeadStamp,
    provider_id: &str,
    model: &str,
    cancel: &CancellationToken,
) -> Result<(String, Option<String>), String> {
    open_branch_head(state, session, branch, expected_head).await?;
    let home = &state.home;
    let (_settings, provider, stored_key) = turn_credentials(home, provider_id).await?;
    let (bearer, extra_headers) =
        resolve_request_auth(&state.home, &state.copilot, &provider, &stored_key).await?;
    if cancel.is_cancelled() {
        return Err("compaction cancelled".to_owned());
    }
    let mut resolved = ResolvedProvider::resolve(
        &provider,
        model,
        &bearer,
        &crate::user_agent::default_user_agent(),
    )
    .map_err(|error| format!("provider setup failed: {error}"))?;
    resolved.headers.extend(extra_headers);
    let transport: Arc<dyn SseTransport> = Arc::new(
        ReqwestTransport::new().map_err(|error| format!("HTTP transport unavailable: {error}"))?,
    );
    let wire = WireProvider::new(resolved, transport);
    let history = ledger_history(&state.service, session, branch, expected_head)
        .await
        .map_err(render_error)?;
    if history.len() < 2 {
        return Ok(("empty".to_owned(), None));
    }
    let head_stamp_text = head_spelling(expected_head);
    let context_window = model_context_window(state, &provider, model);
    let scope = crate::compaction::CompactScope {
        wire: &wire,
        model,
        checkpoint: Some(crate::compaction::CheckpointSlot {
            home,
            session_id: session.as_str(),
            branch_id: branch.as_str(),
            head: &head_stamp_text,
        }),
        context_window,
        overhead_tokens: 0,
        cancel,
    };
    let compacted = crate::compaction::compact_history(&scope, history, true).await;
    match compacted.status {
        crate::compaction::CompactStatus::Wrote | crate::compaction::CompactStatus::Replaced => {
            Ok(("compacted".to_owned(), compacted.summary))
        }
        crate::compaction::CompactStatus::Covered => Ok(("covered".to_owned(), None)),
        crate::compaction::CompactStatus::Unchanged => Ok(("empty".to_owned(), None)),
        crate::compaction::CompactStatus::Failed(message) => Err(message),
    }
}
