//! The General settings page: palette, typeface, language, and request identity.
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
    let font_family = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| settings.font_family.clone())
        .unwrap_or_else(|| mycode_config::SYSTEM_FONT_FAMILY.to_owned());
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
    let font_family_row = font_family_field(&font_family, workspace.vm().font_family_menu_open, cx);
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
                "Palette, typeface, size, and language apply immediately and are saved to settings.",
                "色板、字体、字号和语言立即生效并写入设置。",
            )),
            theme,
            vec![
                palette_row,
                font_family_row,
                font_row,
                language_row,
                ua_field,
            ],
        ))
        .into_any_element()
}

/// Label above a fixed five-column swatch grid.
///
/// Thirteen palettes fill two rows and leave three on the last. Columns are
/// equal, so the last row stays left-aligned instead of stretching.
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

fn font_family_field(selected: &str, open: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let installed = crate::ui::desk::installed_font_names(cx);
    let options = crate::ui::desk::available_font_family_ids(installed)
        .into_iter()
        .map(|id| crate::ui::desk::font_family_label(id).to_owned())
        .collect::<Vec<_>>();
    let current = crate::ui::desk::font_family_label(selected).to_owned();
    dropdown_field(
        "font-family",
        t("Interface font", "界面字体"),
        Some(t(
            "System uses the operating-system UI font. Other choices are fonts this computer can load.",
            "系统使用操作系统界面字体。其余选项是本机能够加载的字体。",
        )),
        &current,
        &options,
        open,
        |workspace, open, cx| workspace.on_toggle_font_family_menu(open, cx),
        |workspace, label, cx| {
            if let Some(id) = font_family_id_for_label(label) {
                workspace.on_select_font_family(id, cx);
            }
            workspace.on_toggle_font_family_menu(false, cx);
        },
        cx,
    )
}

/// The id a displayed font label maps back to, in either language pack.
fn font_family_id_for_label(label: &str) -> Option<&'static str> {
    match label {
        "System" | "系统" => Some(mycode_config::SYSTEM_FONT_FAMILY),
        "Inter" => Some("Inter"),
        "Segoe UI" => Some("Segoe UI"),
        "PingFang" => Some("PingFang"),
        "Noto Sans" => Some("Noto Sans"),
        _ => None,
    }
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
        .id("palette-grid")
        .w_full()
        .grid()
        .grid_cols(crate::ui::desk::PALETTE_GRID_COLUMNS)
        .gap_2()
        .children(crate::ui::desk::PALETTES.into_iter().map(|id| {
            let on = selected == id;
            let swatch = crate::ui::desk::palette_swatch(id);
            let ring = if on {
                theme.foreground
            } else {
                theme.transparent
            };
            let hover_ring = if on {
                theme.foreground
            } else {
                theme.muted_foreground
            };
            div()
                .id(format!("palette-{id}"))
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .cursor_pointer()
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    workspace.on_select_palette(id, cx);
                }))
                .child(
                    div()
                        .w_full()
                        .aspect_square()
                        .p(px(2.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(ring)
                        .hover(move |this| this.border_color(hover_ring))
                        .child(div().size_full().rounded(px(6.)).bg(swatch)),
                )
                .child(
                    div()
                        .text_xs()
                        .truncate()
                        .text_color(theme.foreground)
                        .child(crate::ui::desk::palette_label(id)),
                )
        }))
}
