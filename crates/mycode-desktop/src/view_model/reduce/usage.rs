//! Token accounting: dropping the previous session's live meter when the
//! open conversation changes.

use crate::view_model::WorkspaceState;

/// Drops the previous session's meter and turn stats.
///
/// Usage accounting is live-only: `last_turn` and `live_turn` carry what the
/// provider reported for turns the user watched in this session, so switching
/// conversations must not keep painting the previous prompt size, cache read,
/// or totals.
pub(super) fn clear_session_usage(state: &mut WorkspaceState) {
    state.last_turn = None;
    state.live_turn = None;
    state.context_used = 0;
    state.context_cache = 0;
}
