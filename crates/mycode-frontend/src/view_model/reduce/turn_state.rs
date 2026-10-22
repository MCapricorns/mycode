//! Turn and queue bookkeeping the reducer calls: parking follow-ups with
//! their chat, resetting the turn view, and appending entries once.

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
    if let Some((cut, _)) = text.char_indices().nth(crate::view_model::MAX_COMPOSER_CHARS) {
        text.truncate(cut);
    }
    text
}

/// Appends one follow-up to the queue unless it is blank or the queue is full.
pub(super) fn queue_message(state: &mut WorkspaceState, text: String) {
    let text = bounded_draft(text);
    if !text.trim().is_empty() && state.queued.len() < crate::view_model::MAX_QUEUED_MESSAGES {
        state.queued.push(text);
    }
}

/// Keeps the open chat's follow-up queue while another chat takes the
/// screen. Events for a chat that is not open are dropped, so its queue
/// could never send from here; clearing it lost what the user had typed.
pub(super) fn park_queue(state: &mut WorkspaceState) {
    let Some(session_id) = state
        .active
        .as_ref()
        .map(|active| active.session_id.clone())
    else {
        return;
    };
    state
        .parked_queues
        .retain(|(parked, _)| parked != &session_id);
    if !state.queued.is_empty() {
        let queue = std::mem::take(&mut state.queued);
        state.parked_queues.push((session_id, queue));
    }
}

/// Files steers a chat off screen did not take at the end of its parked
/// queue, under the same cap as a queue on screen.
pub(super) fn park_steers(state: &mut WorkspaceState, session_id: String, texts: Vec<String>) {
    let index = match state
        .parked_queues
        .iter()
        .position(|(parked, _)| *parked == session_id)
    {
        Some(index) => index,
        None => {
            state.parked_queues.push((session_id, Vec::new()));
            state.parked_queues.len() - 1
        }
    };
    let queue = &mut state.parked_queues[index].1;
    for text in texts {
        let text = bounded_draft(text);
        if !text.trim().is_empty() && queue.len() < crate::view_model::MAX_QUEUED_MESSAGES {
            queue.push(text);
        }
    }
}

/// Brings a chat's parked follow-ups back when it opens again. They wait
/// for an explicit send unless its turn is still running; then they start
/// when it ends, as they would have without the switch.
pub(super) fn unpark_queue(state: &mut WorkspaceState, session_id: &str, running: bool) {
    if let Some(index) = state
        .parked_queues
        .iter()
        .position(|(parked, _)| parked == session_id)
    {
        let (_, queue) = state.parked_queues.remove(index);
        state.queued = queue;
        state.queue_paused = !running;
    }
}

/// Drops the turn-scoped view of the conversation leaving the screen: its
/// queue, steers, live jobs, asks, held summary, and the in-flight flag.
/// Park the queue first ([`park_queue`]) when the chat is only leaving.
/// Events for a session that is not open are dropped, so none of this would
/// ever be cleared once another conversation takes the screen.
pub(super) fn reset_turn_view(state: &mut WorkspaceState) {
    state.sending = false;
    state.turn_started_ms = 0;
    state.compacting = false;
    state.auto_compacting = false;
    state.pending_summaries.clear();
    state.queued.clear();
    state.queue_paused = false;
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

pub(super) fn push_entry_once(conversation: &mut ActiveConversation, entry: mycode_app::ConversationEntry) {
    let already = conversation
        .entries
        .iter()
        .any(|existing| existing.event_id == entry.event_id);
    if !already {
        conversation.entries.push(entry);
    }
}
