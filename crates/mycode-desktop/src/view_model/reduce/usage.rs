//! Token accounting: dropping the previous session's live meter when the
//! open conversation changes.

use crate::view_model::WorkspaceState;

/// Drops the previous session's meter and turn stats.
///
/// `last_turn` and `live_turn` belong to turns watched in this process.
/// Opening another session fills `context_used` / `context_cache` again from
/// that session's ledger snapshot.
pub(super) fn clear_session_usage(state: &mut WorkspaceState) {
    state.last_turn = None;
    state.live_turn = None;
    state.context_used = 0;
    state.context_cache = 0;
}
