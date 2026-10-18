//! Token accounting: the live meter, per-turn cache hit, and the warning
//! when a compaction or steer knocks a warm prompt cache cold.

use super::super::WorkspaceState;
use super::super::usage::{
    CacheBreak, CacheWatch, cache_hit_collapsed, cache_hit_recovered, cache_percent,
};

/// Drops the previous session's meter, turn stats, and cache warning.
///
/// `last_turn` and `live_turn` belong to turns watched in this process.
/// Opening another session fills `context_used` / `context_cache` again from
/// that session's ledger snapshot. The collapse watch is process-local too:
/// the next session did not just compact or steer.
pub(super) fn clear_session_usage(state: &mut WorkspaceState) {
    state.last_turn = None;
    state.live_turn = None;
    state.context_used = 0;
    state.context_cache = 0;
    state.cache_watch = None;
    state.cache_alert = None;
}

/// Remember the current hit percent until the next prompt reports usage.
///
/// A second rewrite before that prompt keeps the first baseline and records
/// both causes. Recapturing the baseline would compare the collapsed prompt
/// with itself.
pub(super) fn arm_cache_watch(state: &mut WorkspaceState, cause: CacheBreak) {
    match &mut state.cache_watch {
        Some(watch) => watch.cause = watch.cause.merge(cause),
        None => {
            state.cache_watch = Some(CacheWatch {
                cause,
                before: cache_percent(state.context_cache, state.context_used),
            });
        }
    }
}

/// Compare one prompt's cache hit with an armed rewrite, or clear a warning
/// once a later prompt is warm again.
///
/// A prompt size of zero is not a reading: the watch stays armed. The hit
/// is this prompt's (`cached` / `context`), not the turn's summed tokens.
pub(super) fn observe_cache_hit(state: &mut WorkspaceState, context: u64, cached: u64) {
    if context == 0 {
        return;
    }
    let after = cache_percent(cached, context);
    let Some(watch) = state.cache_watch.take() else {
        if state
            .cache_alert
            .is_some_and(|_| cache_hit_recovered(after))
        {
            state.cache_alert = None;
        }
        return;
    };
    if cache_hit_collapsed(watch.before, after) {
        state.cache_alert = Some(crate::view_model::CacheAlert {
            cause: watch.cause,
            before: watch.before.unwrap_or(0),
            after: after.unwrap_or(0),
        });
        return;
    }
    if state
        .cache_alert
        .is_some_and(|_| cache_hit_recovered(after))
    {
        state.cache_alert = None;
    }
}

#[cfg(test)]
mod tests {
    use mycode_app::{ActiveConversation, ConversationEntry, EntryKind};

    use super::super::reduce;
    use crate::view_model::{CacheBreak, FrontendAction, WorkspaceState, turn_cache_hit};

    fn warm() -> WorkspaceState {
        WorkspaceState {
            sending: true,
            active: Some(ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "br".to_owned(),
                head: "h".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                running: false,
                context_used: 10_000,
                context_cache: 9_200,
            }),
            context_used: 10_000,
            context_cache: 9_200,
            ..WorkspaceState::default()
        }
    }

    fn snapshot(context: u64, context_cache: u64, input: u64, cache: Option<u64>) -> FrontendAction {
        FrontendAction::UsageSnapshot {
            model: "m".to_owned(),
            input,
            context,
            context_cache,
            output: 10,
            cache,
            requests: 1,
            elapsed_ms: 5,
        }
    }

    fn entry(id: &str) -> ConversationEntry {
        ConversationEntry {
            event_id: id.to_owned(),
            kind: EntryKind::AssistantMessage,
            text: "done".into(),
            call_id: None,
            thinking: String::new(),
        }
    }

    #[test]
    fn the_turn_row_hit_is_the_sum_not_the_latest_prompt() {
        let mut state = warm();
        reduce(&mut state, snapshot(10_000, 9_200, 3_000, Some(900)));
        let turn = state.live_turn.as_ref().expect("live turn");
        assert_eq!(turn_cache_hit(turn), Some(30));
        assert_eq!(state.context_cache, 9_200);
        assert!(state.cache_alert.is_none());
    }

    #[test]
    fn a_model_switch_arms_the_watch_and_a_cold_prompt_alerts() {
        let mut state = warm();
        state.selected_model = Some("current".to_owned());
        // Re-picking the same model does not disturb the watch.
        reduce(
            &mut state,
            FrontendAction::ModelSelected("current".to_owned()),
        );
        assert!(state.cache_watch.is_none());
        reduce(
            &mut state,
            FrontendAction::ModelSelected("other".to_owned()),
        );
        assert_eq!(
            state.cache_watch.expect("armed").cause,
            CacheBreak::Switch
        );
        reduce(&mut state, snapshot(10_000, 0, 10_000, None));
        assert_eq!(
            state.cache_alert.expect("collapse").cause,
            CacheBreak::Switch
        );
    }

    #[test]
    fn compaction_then_a_cold_prompt_raises_a_cache_alert() {
        let mut state = warm();
        reduce(
            &mut state,
            FrontendAction::CacheBreakArmed(CacheBreak::Compaction),
        );
        assert_eq!(
            state.cache_watch.expect("armed").before,
            Some(92),
            "baseline is the prompt already on the meter"
        );
        reduce(&mut state, snapshot(0, 0, 0, None));
        assert!(
            state.cache_watch.is_some(),
            "a sizeless prompt is not a reading"
        );
        assert!(state.cache_alert.is_none());
        reduce(&mut state, snapshot(10_000, 0, 10_000, None));
        let alert = state.cache_alert.expect("collapse");
        assert_eq!(alert.cause, CacheBreak::Compaction);
        assert_eq!(alert.before, 92);
        assert_eq!(alert.after, 0);
        assert!(state.cache_watch.is_none());
        reduce(
            &mut state,
            FrontendAction::ChatDone {
                head: "h2".to_owned(),
                entry: entry("done"),
            },
        );
        assert!(
            state.cache_alert.is_some(),
            "finishing the turn keeps the warning"
        );
        assert!(state.live_turn.is_none());
        assert!(state.last_turn.is_some());
    }

    #[test]
    fn a_steer_then_a_modest_dip_does_not_alert() {
        let mut state = warm();
        reduce(
            &mut state,
            FrontendAction::CacheBreakArmed(CacheBreak::Steer),
        );
        reduce(&mut state, snapshot(10_000, 7_000, 10_000, Some(7_000)));
        assert!(state.cache_alert.is_none());
        assert!(state.cache_watch.is_none());
        reduce(&mut state, snapshot(10_000, 0, 20_000, Some(7_000)));
        assert!(
            state.cache_alert.is_none(),
            "a later dip without a new rewrite stays quiet"
        );
    }

    #[test]
    fn compaction_and_a_steer_share_the_first_baseline() {
        let mut state = warm();
        reduce(
            &mut state,
            FrontendAction::CacheBreakArmed(CacheBreak::Compaction),
        );
        state.context_used = 100;
        state.context_cache = 0;
        reduce(
            &mut state,
            FrontendAction::CacheBreakArmed(CacheBreak::Steer),
        );
        let watch = state.cache_watch.expect("still the first arm");
        assert_eq!(watch.before, Some(92));
        assert_eq!(watch.cause, CacheBreak::Both);
        reduce(&mut state, snapshot(10_000, 800, 10_000, Some(800)));
        let alert = state.cache_alert.expect("collapse");
        assert_eq!(alert.cause, CacheBreak::Both);
        assert_eq!(alert.before, 92);
        assert_eq!(alert.after, 8);
    }

    #[test]
    fn a_dip_without_compaction_or_a_steer_stays_quiet() {
        let mut state = warm();
        reduce(&mut state, snapshot(10_000, 9_200, 10_000, Some(9_200)));
        reduce(&mut state, snapshot(10_000, 0, 20_000, Some(9_200)));
        assert!(state.cache_alert.is_none());
        assert!(state.cache_watch.is_none());
    }

    #[test]
    fn a_warm_prompt_clears_the_alert() {
        let mut state = warm();
        reduce(
            &mut state,
            FrontendAction::CacheBreakArmed(CacheBreak::Compaction),
        );
        reduce(&mut state, snapshot(10_000, 0, 10_000, None));
        assert!(state.cache_alert.is_some());
        reduce(&mut state, snapshot(10_000, 8_000, 10_000, Some(8_000)));
        assert!(state.cache_alert.is_none());
    }

    #[test]
    fn switching_sessions_drops_the_cache_alert() {
        let mut state = warm();
        reduce(
            &mut state,
            FrontendAction::CacheBreakArmed(CacheBreak::Steer),
        );
        reduce(&mut state, snapshot(10_000, 0, 10_000, None));
        assert!(state.cache_alert.is_some());
        reduce(
            &mut state,
            FrontendAction::ConversationOpened(ActiveConversation {
                session_id: "other".to_owned(),
                branch_id: "br".to_owned(),
                head: "o".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                running: false,
                context_used: 100,
                context_cache: 0,
            }),
        );
        assert!(state.cache_alert.is_none());
        assert!(state.cache_watch.is_none());
        assert_eq!(state.context_used, 100);
        assert!(state.live_turn.is_none());
    }
}
