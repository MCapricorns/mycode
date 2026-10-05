//! The General settings page: palette, language, and request identity.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::Input;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::widgets::{choice_chips, dropdown_field, settings_card};
use crate::i18n::{effective_language_id, t};
use crate::workspace::Workspace;

/// Display label for one configured language id.
fn language_label(id: &str) -> &'static str {
    match id {
        "zh" => "中文",
        "en" => "English",
        _ => t("Follow system", "跟随系统"),
    }
}

/// The id a displayed label maps back to, across both language packs.
fn language_id_for_label(label: &str) -> Option<&'static str> {
    match label {
        "中文" => Some("zh"),
        "English" => Some("en"),
        "Follow system" | "跟随系统" => Some("auto"),
        _ => None,
    }
}

pub(super) fn render_general_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let ua_input = workspace.settings_ua_input(window, cx);
    let palette = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| settings.palette.clone())
        .unwrap_or_else(|| "slate".to_owned());
    let configured_language = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| settings.language.clone())
        .unwrap_or_else(|| "auto".to_owned());
    let font_size = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| settings.font_size.clone())
        .unwrap_or_else(|| "m".to_owned());
    let language = effective_language_id(&configured_language).to_owned();
    let language_label_now = language_label(&language).to_owned();
    let language_options = ["auto", "en", "zh"]
        .iter()
        .map(|id| language_label(id).to_owned())
        .collect::<Vec<_>>();
    let effective_ua = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| settings.effective_user_agent.clone())
        .unwrap_or_default();
    let palette_row = palette_field(&palette, cx);
    let font_row = font_size_field(&font_size, cx);
    let language_row = dropdown_field(
        "language",
        t("Language", "语言"),
        Some(t(
            "Interface language. English and Simplified Chinese are built in.",
            "界面语言。内置英文与简体中文。",
        )),
        &language_label_now,
        &language_options,
        workspace.vm().language_menu_open,
        |workspace, open, cx| workspace.on_toggle_language_menu(open, cx),
        |workspace, label, cx| {
            if let Some(id) = language_id_for_label(label) {
                workspace.on_select_language(id, cx);
            }
            workspace.on_toggle_language_menu(false, cx);
        },
        cx,
    );
    let ua_field = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_sm()
                .child(t("HTTP User-Agent", "HTTP User-Agent")),
        )
        .child(div().h(px(30.)).text_sm().child(Input::new(&ua_input)))
        .child(
            div()
                .text_xs()
                .opacity(0.5)
                .child(format!("{}: {effective_ua}", t("Effective", "生效值"))),
        )
        .into_any_element();
    let theme = cx.theme();
    div()
        .id("general-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_card(
            "appearance",
            t("Appearance", "外观"),
            Some(t(
                "Palette, language, and type size apply immediately and are saved to settings.",
                "色板、语言和字号立即生效并随设置保存。",
            )),
            theme,
            vec![palette_row, font_row, language_row, ua_field],
        ))
        .into_any_element()
}

/// Label above a wrapping chip row.
///
/// A side-by-side settings row gives the label `flex-basis: 0` and
/// `min-width: 0`. The chip group's max-content width is the full unwrapped
/// line, and thirteen names are wider than the settings column, so that label
/// collapses and its text paints across the chips. Extra palettes only add
/// wrap rows under the description.
fn palette_field(selected: &str, cx: &mut Context<Workspace>) -> AnyElement {
    div()
        .id("row-palette")
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .whitespace_normal()
                        .child(t("Palette", "色板")),
                )
                .child(div().text_xs().opacity(0.5).whitespace_normal().child(t(
                    "A quiet dark page. Pick a hue.",
                    "安静的深色页面。选一个色调。",
                ))),
        )
        .child(palette_choices(selected, cx))
        .into_any_element()
}

fn font_size_field(selected: &str, cx: &mut Context<Workspace>) -> AnyElement {
    let options = ["s", "m", "l", "xl"]
        .into_iter()
        .map(|id| {
            (
                id.to_owned(),
                crate::ui::desk::font_size_label(id).to_owned(),
            )
        })
        .collect::<Vec<_>>();
    div()
        .id("row-font-size")
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .whitespace_normal()
                        .child(t("Interface font size", "界面字号")),
                )
                .child(div().text_xs().opacity(0.5).whitespace_normal().child(t(
                    "S / M / L / XL, about 12 / 13 / 14 / 16 px. Chat, the sidebar, and settings follow it immediately.",
                    "S / M / L / XL，大约 12 / 13 / 14 / 16 像素。对话、侧栏和设置会立即跟随。",
                ))),
        )
        .child(choice_chips(
            "font-size",
            &options,
            selected,
            |workspace, value, cx| workspace.on_select_font_size(value, cx),
            cx,
        ))
        .into_any_element()
}

fn palette_choices(selected: &str, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .w_full()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap_1()
        .children(crate::ui::desk::PALETTES.into_iter().map(|id| {
            let on = selected == id;
            let swatch = crate::ui::desk::palette_swatch(id);
            let hover_bg = if on {
                theme.accent
            } else {
                theme.secondary_hover
            };
            div()
                .id(format!("palette-{id}"))
                .flex()
                .flex_row()
                .flex_shrink_0()
                .items_center()
                .gap_1()
                .h(px(28.))
                .px_2()
                .rounded(px(8.))
                .border_1()
                .border_color(if on {
                    theme.primary.opacity(0.55)
                } else {
                    theme.border
                })
                .bg(if on { theme.accent } else { theme.transparent })
                .cursor_pointer()
                .hover(move |this| this.bg(hover_bg))
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    workspace.on_select_palette(id, cx);
                }))
                .child(div().size(px(10.)).rounded_full().bg(swatch))
                .child(
                    div()
                        .text_xs()
                        .whitespace_nowrap()
                        .text_color(theme.foreground)
                        .child(crate::ui::desk::palette_label(id)),
                )
        }))
}
