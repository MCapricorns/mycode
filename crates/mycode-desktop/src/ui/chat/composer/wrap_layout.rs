use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::test::{TestSupportExt as _, TestWindowExt as _};
use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    Styled, TestAppContext, Window, div, px, size,
};

struct WrapProbe {
    state: Entity<TextareaState>,
    width: f32,
}

impl Render for WrapProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            div()
                .id("wrap-box")
                .w(px(self.width))
                .min_w_0()
                .child(
                    div()
                        .id("composer-input")
                        .w_full()
                        .min_w_0()
                        .h_auto()
                        .flex()
                        .flex_col()
                        .child(
                            Textarea::new(&self.state)
                                .appearance(false)
                                .bordered(false)
                                .w_full(),
                        )
                        .test_support(),
                )
                .test_support(),
        )
    }
}

#[gpui_kit::test]
fn a_long_draft_wraps_and_grows_inside_the_composer_width(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let width = 360.;
    let held: Rc<RefCell<Option<Entity<TextareaState>>>> = Rc::new(RefCell::new(None));
    let slot = held.clone();
    let handle = cx.open_window(size(px(800.), px(400.)), move |window, cx| {
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .soft_wrap(true)
                .auto_grow(1, 8)
        });
        *slot.borrow_mut() = Some(state.clone());
        WrapProbe { state, width }
    });
    let state = held.borrow().clone().expect("textarea");
    cx.update_window(handle.into(), |_, window, cx| {
        let draft = "wrap this sentence ".repeat(12);
        state.update(cx, |state, cx| state.set_value(draft, window, cx));
        window.render_frame(cx);
        window.render_frame(cx);
        let box_bounds = window.find("wrap-box").bounds();
        let input = window.find("composer-input").bounds();
        assert!(
            input.right() <= box_bounds.right() + px(1.)
                && input.size.width <= box_bounds.size.width + px(1.),
            "the field must stay inside the composer width: input {input:?} box {box_bounds:?}"
        );
        assert!(
            input.size.height > px(28.),
            "a long draft must wrap onto another line instead of scrolling sideways: {input:?}"
        );
    })
    .unwrap();
}
