use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::test::{TestSupportExt as _, TestWindowExt as _};
use gpui_kit::{
    AppContext as _, Context, InteractiveElement, IntoElement, ParentElement, Render, ScrollDelta,
    ScrollHandle, StatefulInteractiveElement, Styled, TestAppContext, Window, div, point, px, size,
};

struct FollowProbe {
    handle: ScrollHandle,
    viewport: Rc<Cell<f32>>,
    content: Rc<Cell<f32>>,
    following: Rc<Cell<bool>>,
}

impl Render for FollowProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let anchor =
            super::ScrollAnchor::new(self.handle.clone(), None, None, self.following.get());
        let probe = anchor.probe();
        div().size_full().child(
            anchor.child(
                div()
                    .id("follow-scroller")
                    .w(px(390.))
                    .h(px(self.viewport.get()))
                    .overflow_y_scroll()
                    .track_scroll(&self.handle)
                    .on_scroll_wheel(cx.listener(|this, _, _, cx| {
                        this.following.set(
                            crate::workspace::Workspace::conversation_offset_is_at_tail(
                                this.handle.offset().y,
                                this.handle.max_offset().y,
                            ),
                        );
                        cx.notify();
                    }))
                    .flex()
                    .flex_col()
                    .child(probe.child(div().w_full().h(px(self.content.get())).flex_none()))
                    .test_support(),
            ),
        )
    }
}

#[gpui_kit::test]
fn steer_dock_resize_keeps_following_until_the_reader_scrolls_away(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let scroll = ScrollHandle::new();
    let viewport = Rc::new(Cell::new(400.));
    let content = Rc::new(Cell::new(1000.));
    let following = Rc::new(Cell::new(true));
    let probe = FollowProbe {
        handle: scroll.clone(),
        viewport: viewport.clone(),
        content: content.clone(),
        following: following.clone(),
    };
    let window = cx.open_window(size(px(420.), px(600.)), move |_, _| probe);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(scroll.offset().y, -scroll.max_offset().y);
        assert!(scroll.max_offset().y > px(0.));

        // Opening a steer choice shrinks the viewport without a chat event.
        viewport.set(280.);
        window.render_frame(cx);
        assert_eq!(scroll.offset().y, px(-720.));

        // A queued steer appears, then new output arrives. Neither is user scroll.
        viewport.set(240.);
        content.set(1180.);
        window.render_frame(cx);
        assert_eq!(scroll.offset().y, px(-940.));

        // Deliberately reading history must keep the chosen line in view.
        following.set(false);
        scroll.set_offset(point(px(0.), px(-200.)));
        viewport.set(400.);
        content.set(1300.);
        window.render_frame(cx);
        assert_eq!(scroll.offset().y, px(-200.));

        // Jump to latest restores follow for subsequent layout-only changes.
        following.set(true);
        window.render_frame(cx);
        assert_eq!(scroll.offset().y, px(-900.));
        viewport.set(300.);
        window.render_frame(cx);
        assert_eq!(scroll.offset().y, px(-1000.));
    })
    .unwrap();
}

#[gpui_kit::test]
fn small_upward_wheel_steps_pause_follow_before_the_next_frame(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let scroll = ScrollHandle::new();
    let following = Rc::new(Cell::new(true));
    let viewport = Rc::new(Cell::new(400.));
    let probe = FollowProbe {
        handle: scroll.clone(),
        viewport: viewport.clone(),
        content: Rc::new(Cell::new(1000.)),
        following: following.clone(),
    };
    let window = cx.open_window(size(px(420.), px(600.)), move |_, _| probe);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(scroll.offset().y, px(-600.));
        for expected in [-580., -560., -540.] {
            window.scroll(
                "follow-scroller",
                ScrollDelta::Pixels(point(px(0.), px(20.))),
                cx,
            );
            assert_eq!(scroll.offset().y, px(expected));
            assert!(!following.get(), "a small wheel step must not snap back");
        }
        viewport.set(280.);
        window.render_frame(cx);
        assert_eq!(scroll.offset().y, px(-540.));

        window.scroll(
            "follow-scroller",
            ScrollDelta::Pixels(point(px(0.), px(-2000.))),
            cx,
        );
        assert!(following.get(), "scrolling back to the tail resumes follow");
        assert_eq!(scroll.offset().y, px(-720.));
        window.scroll(
            "follow-scroller",
            ScrollDelta::Pixels(point(px(0.), px(0.25))),
            cx,
        );
        assert!(
            !following.get(),
            "fractional touchpad input also pauses follow"
        );
        assert_eq!(scroll.offset().y, px(-719.75));
    })
    .unwrap();
}
