use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::InputState;
use gpui_kit::component::theme::Theme;
use gpui_kit::test::{TestSupportExt as _, TestWindowExt as _};
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement, Render,
    Styled, TestAppContext, Window, div, px, size,
};

use super::widgets::{choice_chip, choice_track, dropdown_button, settings_card, settings_row};

struct SettingsProbe {
    search: Entity<InputState>,
    font_size: &'static str,
}

impl Render for SettingsProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::ui::desk::apply_font_size(Theme::global_mut(cx), self.font_size);
        window.set_rem_size(px(crate::ui::desk::interface_rem_px(self.font_size)));
        let theme = cx.theme();
        let chips = choice_track("font-size", theme)
            .children(["s", "m", "l", "xl"].map(|value| {
                choice_chip(
                    "font-size",
                    value,
                    &value.to_uppercase(),
                    value == self.font_size,
                    theme,
                )
                .test_support()
            }))
            .test_support();
        div().size_full().flex().flex_col()
            .child(super::settings_header(&self.search, |_, _, _| {}, theme).test_support())
            .child(div().flex_1().min_h_0().flex().flex_row()
                .child(div().w(px(236.)).flex_none())
                .child(div().id("settings-content").flex_1().min_w_0().p_6().test_support()
                    .child(settings_card("appearance", "Appearance", None, theme, vec![
                        settings_row("font-size", "Text size", Some("Body text at 12 / 14 / 16 / 18 px. The typeface follows the operating system."), chips.into_any_element(), theme),
                        settings_row("dropdown-language", "Language", Some("Interface language. English and Simplified Chinese are built in."), dropdown_button("language", "English", theme).test_support().into_any_element(), theme),
                    ]))))
    }
}

#[gpui_kit::test]
fn settings_search_and_controls_fit_every_font_scale(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    for width in [960., 1440.] {
        for font_size in ["s", "m", "l", "xl"] {
            let handle =
                cx.open_window(size(px(width), px(600.)), move |window, cx| SettingsProbe {
                    search: cx.new(|cx| InputState::new(window, cx).placeholder("Search settings")),
                    font_size,
                });
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.render_frame(cx);
                let header = window.find("settings-header").bounds();
                let search = window.find("settings-search").bounds();
                assert!(
                    search.top() >= header.top() && search.bottom() <= header.bottom(),
                    "{width}/{font_size}: search {search:?} clipped by header {header:?}"
                );
                let row = window.find("row-font-size").bounds();
                let hint = window.find("row-font-size-hint").bounds();
                let track = window.find("chips-font-size").bounds();
                let chip = window.find("font-size-xl").bounds();
                let language = window.find("row-dropdown-language").bounds();
                let button = window.find("dropdown-button-language").bounds();
                let language_hint = window.find("row-dropdown-language-hint").bounds();
                assert!(
                    hint.right() <= track.left() && chip.right() <= row.right(),
                    "{width}/{font_size}: size controls overlap their label"
                );
                assert!(
                    language.top() >= row.bottom() && button.right() <= language.right(),
                    "{width}/{font_size}: dropdown escapes its row"
                );
                assert!(language_hint.right() <= button.left());
                assert!(
                    chip.size.height >= px(crate::ui::desk::interface_font_px(font_size) * 1.4),
                    "{font_size}: segment clips its text: {chip:?}"
                );
            })
            .unwrap();
        }
    }
}
