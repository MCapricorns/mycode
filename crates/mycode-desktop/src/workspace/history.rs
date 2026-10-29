//! Conversation scroll: tail follow, the theme-change hold, and paging
//! older history in near the top. The load decision is the driver's; the
//! scroll handle and its anchors stay here.
use gpui_kit::{Context, Pixels, px};

use super::{AppearanceScroll, ScrollHold, Workspace};

impl Workspace {
    /// Previous offset and content height for one prepend, consumed by the
    /// chat column on the frame that paints the new rows.
    pub(crate) fn take_scroll_anchor(&mut self) -> Option<(Pixels, Pixels)> {
        let hold = self
            .scroll_hold
            .take()
            .map(|hold| (hold.offset_y, hold.content_height));
        if hold.is_some() {
            self.driver.note_scroll_hold(false);
        }
        hold
    }

    /// Theme-change scroll hold, consumed by the next conversation paint.
    ///
    /// The bool is whether the column was pinned to the tail.
    pub(crate) fn take_appearance_scroll(&mut self) -> Option<(bool, Pixels)> {
        self.appearance_scroll
            .take()
            .map(|hold| (hold.pinned, hold.offset_y))
    }

    /// The reader's follow intent, changed by scrolling or jumping to latest.
    /// A steer card or a resized composer may change the measured bottom
    /// without any user scroll, so geometry alone is not the source of truth.
    pub(crate) fn conversation_follows_tail(&self) -> bool {
        self.driver.conversation_at_tail()
    }

    /// True only at or beyond the tail. Any upward movement must be able
    /// to pause follow, including small touchpad deltas between frames.
    pub(crate) fn conversation_offset_is_at_tail(offset_y: Pixels, max_offset_y: Pixels) -> bool {
        // Wheel input can briefly overscroll before prepaint clamps it.
        offset_y + max_offset_y <= px(0.)
    }

    /// The conversation column's scroll handle.
    pub(crate) fn conversation_scroll_handle(&self) -> &gpui_kit::ScrollHandle {
        &self.conversation_scroll
    }

    /// Moves the conversation column to the newest line.
    pub(crate) fn scroll_conversation_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.driver.set_conversation_at_tail(true);
        self.conversation_scroll.scroll_to_bottom();
        cx.notify();
    }

    /// The subagent window body's scroll handle.
    pub(crate) fn subagent_scroll_handle(&self) -> &gpui_kit::ScrollHandle {
        &self.subagent_scroll
    }

    /// The selected record before an update lands.
    pub(super) fn subagent_window_mark(&self) -> SubagentMark {
        SubagentMark {
            call_id: self.vm().subagent_window.clone(),
        }
    }

    /// A newly selected record opens at its current state and result.
    /// Progress updates preserve the reader's position in optional details.
    pub(super) fn follow_subagent_window(&mut self, before: SubagentMark) {
        let after = self.subagent_window_mark();
        if subagent_window_follows(&before, &after) {
            self.subagent_scroll
                .set_offset(gpui_kit::point(px(0.), px(0.)));
        }
    }

    /// Loads the next older page when the viewport is within a short distance
    /// of the top. A transcript that still fits does not chain-load. The
    /// notify keeps the docked jump-to-latest row in step with the scroll
    /// position.
    pub(crate) fn on_conversation_scrolled(&mut self, cx: &mut Context<Self>) {
        self.driver
            .set_conversation_at_tail(Self::conversation_offset_is_at_tail(
                self.conversation_scroll.offset().y,
                self.conversation_scroll.max_offset().y,
            ));
        cx.notify();
        if self.vm().history_loading || self.scroll_hold.is_some() || !self.near_history_top() {
            return;
        }
        self.driver_do(|d, fx| d.request_older(fx), cx);
    }

    /// Chip click. Works even when the tail still fits on screen.
    pub(crate) fn on_load_older(&mut self, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.request_older(fx), cx);
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

    /// Anchors the viewport while the prepended page lays out. Mirrored into
    /// the driver so a second `LoadOlder` does not measure the wrong height.
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
        self.driver.note_scroll_hold(true);
    }
}

/// Subagent window state captured around one update.
pub(super) struct SubagentMark {
    call_id: Option<String>,
}

fn subagent_window_follows(before: &SubagentMark, after: &SubagentMark) -> bool {
    after.call_id.is_some() && before.call_id != after.call_id
}

fn near_history_top(offset_y: Pixels, max_offset_y: Pixels) -> bool {
    max_offset_y > px(1.) && offset_y > px(-64.)
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
    fn agent_details_open_at_the_summary_and_updates_preserve_scroll() {
        let mark = |id: Option<&str>| SubagentMark {
            call_id: id.map(str::to_owned),
        };
        assert!(subagent_window_follows(&mark(None), &mark(Some("a"))));
        assert!(subagent_window_follows(&mark(Some("a")), &mark(Some("b"))));
        assert!(!subagent_window_follows(&mark(Some("a")), &mark(Some("a"))));
        assert!(!subagent_window_follows(&mark(Some("a")), &mark(None)));
    }

    #[test]
    fn tail_follow_stays_at_the_bottom_and_lets_go_when_scrolled_up() {
        let follows = Workspace::conversation_offset_is_at_tail;
        assert!(follows(px(0.), px(0.)));
        assert!(follows(px(-800.), px(800.)));
        assert!(!follows(px(-799.75), px(800.)));
        assert!(!follows(px(-780.), px(800.)));
        // Wheel delta past the bottom reads as overscroll before the clamp.
        assert!(follows(px(-860.), px(800.)));
        assert!(!follows(px(-100.), px(800.)));
        assert!(!follows(px(0.), px(800.)));
    }
}
