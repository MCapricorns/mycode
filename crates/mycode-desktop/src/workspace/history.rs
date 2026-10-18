//! Conversation scroll: tail follow, the theme-change hold, and paging
//! older history in near the top.
use gpui_kit::{Context, Pixels, px};
use mycode_app::BridgeCommand;

use super::{AppearanceScroll, ScrollHold, Workspace};
use crate::view_model::FrontendAction;

impl Workspace {
    /// Previous offset and content height for one prepend, consumed by the
    /// chat column on the frame that paints the new rows.
    pub(crate) fn take_scroll_anchor(&mut self) -> Option<(Pixels, Pixels)> {
        self.scroll_hold
            .take()
            .map(|hold| (hold.offset_y, hold.content_height))
    }

    /// Theme-change scroll hold, consumed by the next conversation paint.
    ///
    /// The bool is whether the column was pinned to the tail.
    pub(crate) fn take_appearance_scroll(&mut self) -> Option<(bool, Pixels)> {
        self.appearance_scroll
            .take()
            .map(|hold| (hold.pinned, hold.offset_y))
    }

    /// True when the conversation column is within a few pixels of the bottom.
    ///
    /// The handle stores a positive max extent and a negative live offset, so
    /// their sum is about zero at the tail and positive after a scroll upward.
    /// Before the first layout both are zero, which still follows.
    pub(crate) fn conversation_follows_tail(&self) -> bool {
        follows_tail(
            self.conversation_scroll.offset().y,
            self.conversation_scroll.max_offset().y,
        )
    }

    /// The conversation column's scroll handle.
    pub(crate) fn conversation_scroll_handle(&self) -> &gpui_kit::ScrollHandle {
        &self.conversation_scroll
    }

    /// Moves the conversation column to the newest line.
    pub(crate) fn scroll_conversation_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.conversation_scroll.scroll_to_bottom();
        cx.notify();
    }

    /// The subagent window body's scroll handle.
    pub(crate) fn subagent_scroll_handle(&self) -> &gpui_kit::ScrollHandle {
        &self.subagent_scroll
    }

    /// The open subagent window, its log length, and whether its body sits
    /// on the newest step, read before an update lands.
    pub(super) fn subagent_window_mark(&self) -> SubagentMark {
        let call_id = self.vm.subagent_window.clone();
        let log_len = call_id.as_deref().map_or(0, |call_id| {
            self.vm
                .live_jobs
                .iter()
                .find(|job| job.call_id == call_id)
                .map_or(0, |job| job.log.len())
        });
        SubagentMark {
            call_id,
            log_len,
            at_tail: follows_tail(
                self.subagent_scroll.offset().y,
                self.subagent_scroll.max_offset().y,
            ),
        }
    }

    /// Opens a different subagent at its newest step, and keeps an open one
    /// there as steps arrive. A reader who scrolled up keeps that position.
    pub(super) fn follow_subagent_window(&mut self, before: SubagentMark) {
        let after = self.subagent_window_mark();
        if subagent_window_follows(&before, &after) {
            self.subagent_scroll.scroll_to_bottom();
        }
    }

    /// Loads the next older page when the viewport is within a short distance
    /// of the top. A transcript that still fits does not chain-load. The
    /// notify keeps the docked jump-to-latest row in step with the scroll
    /// position.
    pub(crate) fn on_conversation_scrolled(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        if self.vm.history_loading || self.scroll_hold.is_some() || !self.near_history_top() {
            return;
        }
        self.request_older(cx);
    }

    /// Chip click. Works even when the tail still fits on screen.
    pub(crate) fn on_load_older(&mut self, cx: &mut Context<Self>) {
        self.request_older(cx);
    }

    fn near_history_top(&self) -> bool {
        near_history_top(
            self.conversation_scroll.offset().y,
            self.conversation_scroll.max_offset().y,
        )
    }

    pub(super) fn capture_appearance_scroll(&mut self) {
        self.appearance_scroll = Some(AppearanceScroll {
            pinned: self.conversation_follows_tail(),
            offset_y: self.conversation_scroll.offset().y,
        });
    }

    pub(super) fn capture_scroll_hold(&mut self) {
        let offset = self.conversation_scroll.offset();
        let height = self
            .conversation_scroll
            .bounds_for_item(0)
            .map(|bounds| bounds.size.height)
            .unwrap_or(px(0.));
        self.scroll_hold = Some(ScrollHold {
            offset_y: offset.y,
            content_height: height,
        });
    }

    fn request_older(&mut self, cx: &mut Context<Self>) {
        // `scroll_hold` is the frame where a previous page is still being
        // anchored. Starting another read here would measure the wrong height.
        if self.vm.sending || self.vm.history_loading || self.scroll_hold.is_some() {
            return;
        }
        let Some(before) = self
            .vm
            .active
            .as_ref()
            .and_then(|active| active.older_before.clone())
        else {
            return;
        };
        let Some((session, branch, expected_head)) = self.open_turn_target() else {
            return;
        };
        self.apply_action(FrontendAction::HistoryLoadStarted, cx);
        self.dispatch(
            BridgeCommand::LoadOlder {
                session,
                branch,
                expected_head,
                before,
            },
            cx,
        );
    }
}

/// Subagent window state captured around one update.
pub(super) struct SubagentMark {
    call_id: Option<String>,
    log_len: usize,
    at_tail: bool,
}

fn subagent_window_follows(before: &SubagentMark, after: &SubagentMark) -> bool {
    match (&before.call_id, &after.call_id) {
        (_, None) => false,
        (Some(old), Some(new)) if old == new => after.log_len != before.log_len && before.at_tail,
        _ => true,
    }
}

fn near_history_top(offset_y: Pixels, max_offset_y: Pixels) -> bool {
    max_offset_y > px(1.) && offset_y > px(-64.)
}

/// Slack past the bottom edge. A reader a few pixels short of the tail still
/// follows the stream; anything further up is history.
const TAIL_SLACK: Pixels = px(48.);

fn follows_tail(offset_y: Pixels, max_offset_y: Pixels) -> bool {
    // The wheel handler adds the delta before the scroller's prepaint clamp,
    // so a scroll down at the tail reads a negative gap until that pass. Past
    // the bottom is still the bottom.
    let gap = offset_y + max_offset_y;
    gap <= TAIL_SLACK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_autoload_uses_the_positive_scroll_extent() {
        assert!(!near_history_top(px(0.), px(0.)));
        assert!(near_history_top(px(0.), px(800.)));
        assert!(near_history_top(px(-32.), px(800.)));
        assert!(!near_history_top(px(-100.), px(800.)));
        assert!(!near_history_top(px(-800.), px(800.)));
    }

    #[test]
    fn the_subagent_window_follows_new_steps_only_from_its_tail() {
        let mark = |call_id: Option<&str>, log_len, at_tail| SubagentMark {
            call_id: call_id.map(str::to_owned),
            log_len,
            at_tail,
        };
        // Opening a window, or switching to another child, starts at the end.
        assert!(subagent_window_follows(
            &mark(None, 0, true),
            &mark(Some("a"), 3, true)
        ));
        assert!(subagent_window_follows(
            &mark(Some("a"), 3, false),
            &mark(Some("b"), 1, false)
        ));
        // A new step follows only a reader already at the bottom.
        assert!(subagent_window_follows(
            &mark(Some("a"), 3, true),
            &mark(Some("a"), 4, true)
        ));
        assert!(!subagent_window_follows(
            &mark(Some("a"), 3, false),
            &mark(Some("a"), 4, false)
        ));
        assert!(!subagent_window_follows(
            &mark(Some("a"), 3, true),
            &mark(Some("a"), 3, true)
        ));
        assert!(!subagent_window_follows(
            &mark(Some("a"), 3, true),
            &mark(None, 0, true)
        ));
    }

    #[test]
    fn tail_follow_stays_at_the_bottom_and_lets_go_when_scrolled_up() {
        assert!(follows_tail(px(0.), px(0.)));
        assert!(follows_tail(px(-800.), px(800.)));
        assert!(follows_tail(px(-780.), px(800.)));
        // Wheel delta past the bottom reads as overscroll before the clamp.
        assert!(follows_tail(px(-860.), px(800.)));
        assert!(!follows_tail(px(-100.), px(800.)));
        assert!(!follows_tail(px(0.), px(800.)));
    }
}
