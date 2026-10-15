use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::{TestSupportExt as _, TestWindowExt as _};
use gpui_kit::{
    AppContext as _, Context, InteractiveElement, IntoElement, ParentElement, Render, Styled,
    TestAppContext, Window, div, px, size,
};

use super::{MenuAnchor, composer_menu_layer, menu_height_cap};

struct MenuProbe {
    x: f32,
    width: f32,
    anchor: MenuAnchor,
}

impl Render for MenuProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            div()
                .id("probe-chip")
                .absolute()
                .left(px(self.x))
                .bottom(px(12.))
                .w(px(96.))
                .h(px(24.))
                .overflow_hidden()
                .test_support()
                .child(composer_menu_layer(
                    "probe-menu-layer",
                    self.anchor,
                    crate::ui::skin::menu_panel("probe-menu", cx.theme())
                        .w(px(self.width))
                        .max_w_full()
                        .h(menu_height_cap(window, 320.))
                        .test_support(),
                )),
        )
    }
}

#[gpui_kit::test]
fn composer_menus_clamp_inside_short_narrow_windows(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    for (x, width, anchor) in [
        (300., 340., MenuAnchor::ChipStart),
        (8., 220., MenuAnchor::ChipEnd),
    ] {
        let handle = cx.open_window(size(px(420.), px(360.)), move |_, _| MenuProbe {
            x,
            width,
            anchor,
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let menu = window.find("probe-menu");
            let bounds = menu.bounds();
            let chip = window.find("probe-chip").bounds();
            assert!(menu.visible(), "deferred menus must escape the chip's clip: {bounds:?}");
            assert!(bounds.left() >= px(0.) && bounds.right() <= px(420.), "the menu must stay inside the window: {bounds:?}");
            assert!(bounds.top() >= px(0.) && bounds.bottom() < chip.top(), "a short window must keep the menu above the composer: menu {bounds:?} chip {chip:?}");
            assert!((bounds.size.width - px(width)).abs() <= px(1.), "the menu must retain a readable width: {bounds:?}");
        }).unwrap();
    }
}
