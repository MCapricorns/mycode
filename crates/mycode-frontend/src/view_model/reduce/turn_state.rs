//! Turn bookkeeping the reducer calls: parking steers with their chat,
//! resetting the turn view, and appending entries once.

use super::*;
/// A mid-turn summary arrives while the final reply is still only in the
/// streaming bubble. Inserting it then paints the card above that reply.
/// Hold it until `ChatDone` appends the reply. A manual `/compact`, or an
/// automatic checkpoint taken before this turn has streamed anything, is
/// inserted where it arrives, which is also where the ledger stores it.
pub(super) fn summary_waits_for_the_reply(state: &WorkspaceState) -> bool {
    state.sending
        && state.active.as_ref().is_some_and(|active| {
            active.streaming.as_ref().is_some_and(|reply| {
                !reply.text.trim().is_empty() || !reply.thinking.trim().is_empty()
            })
        })
}

/// Cuts a composer text to [`crate::view_model::MAX_COMPOSER_CHARS`] in place.
pub(super) fn bounded_draft(mut text: String) -> String {
    if let Some((cut, _)) = text
        .char_indices()
        .nth(crate::view_model::MAX_COMPOSER_CHARS)
    {
        text.truncate(cut);
    }
    text
}

/// Files steers a chat off screen did not read. Events for a chat that is
/// not open are dropped, so its composer cannot take them now; they come
/// back when it opens.
pub(super) fn park_steers(state: &mut WorkspaceState, session_id: String, texts: Vec<String>) {
    let index = match state
        .parked_steers
        .iter()
        .position(|(parked, _)| *parked == session_id)
    {
        Some(index) => index,
        None => {
            state.parked_steers.push((session_id, Vec::new()));
            state.parked_steers.len() - 1
        }
    };
    let parked = &mut state.parked_steers[index].1;
    for text in texts {
        let text = bounded_draft(text);
        if !text.trim().is_empty() {
            parked.push(text);
        }
    }
}

/// Drops the turn-scoped view of the conversation leaving the screen: its
/// steers, live jobs, asks, held summary, and the in-flight flag.
/// Park the steers first ([`park_steers`]) when the chat is only leaving.
/// Events for a session that is not open are dropped, so none of this would
/// ever be cleared once another conversation takes the screen.
pub(super) fn reset_turn_view(state: &mut WorkspaceState) {
    state.sending = false;
    state.turn_started_ms = 0;
    state.compacting = false;
    state.auto_compacting = false;
    state.pending_summaries.clear();
    state.pending_steers.clear();
    state.turn_alert = None;
    retire_live_jobs(state);
    drop_preparing_rows(state);
    // `retire_live_jobs` just parked each job's final runtime on the state
    // for the transcript rows that stay behind; clearing here would undo it.
    state.changes_panel_open = false;
    state.pending_ask = None;
    state.ask_answers.clear();
    state.history_loading = false;
}

/// Closes the turn on screen after it committed or failed.
pub(super) fn end_turn(state: &mut WorkspaceState) {
    // Retire, not drop: each agent row keeps its final runtime.
    retire_live_jobs(state);
    // A call still being written when the turn closed never started.
    drop_preparing_rows(state);
    state.pending_ask = None;
    state.ask_answers.clear();
    state.sending = false;
    state.auto_compacting = false;
    state.turn_started_ms = 0;
}

pub(super) fn push_entry_once(
    conversation: &mut ActiveConversation,
    entry: mycode_app::ConversationEntry,
) {
    let already = conversation
        .entries
        .iter()
        .any(|existing| existing.event_id == entry.event_id);
    if !already {
        conversation.entries.push(entry);
    }
}
