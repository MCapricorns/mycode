//! The General settings page: palette, language, request identity, and the
//! platform shell preference.
use gpui_kit::component::button::Button;
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::widgets::{dropdown_field, settings_card, settings_row};
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
    let shell = workspace
        .vm()
        .settings
        .as_ref()
        .and_then(|settings| settings.tools.shell.clone());
    let shell_kind = shell
        .as_ref()
        .map(|item| item.kind.clone())
        .unwrap_or_else(|| "auto".to_owned());
    let shell_program = shell
        .as_ref()
        .map(|item| item.program.clone())
        .unwrap_or_default();
    let shell_source = shell
        .as_ref()
        .map(|item| {
            if item.source.is_empty() {
                "auto".to_owned()
            } else {
                item.source.clone()
            }
        })
        .unwrap_or_else(|| "auto".to_owned());
    let shell_status = if shell_program.is_empty() {
        t(
            "No shell found. Detect one or browse to pwsh or Git bash.",
            "未找到 shell。可自动检测,或浏览选择 pwsh 或 Git bash。",
        )
        .to_owned()
    } else {
        format!("{shell_kind} · {shell_program} ({shell_source})")
    };
    let shell_kind_open = workspace.vm().shell_kind_menu_open;
    let shell_options = ["pwsh", "bash"]
        .iter()
        .map(|kind| (*kind).to_owned())
        .collect::<Vec<_>>();
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
                "The palette applies immediately and is saved to settings right away.",
                "色板立即生效并随设置保存。",
            )),
            theme,
            vec![palette_row, language_row, ua_field],
        ))
        .child(settings_card(
            "shell",
            t("Shell", "Shell"),
            Some(t(
                "First launch detects pwsh, then Git bash. Override it here if \
                 detection misses your install.",
                "首次启动会依次探测 pwsh、Git bash。如果检测不到,可在这里手动指定。",
            )),
            theme,
            vec![
                settings_row(
                    "shell-current",
                    t("Current program", "当前程序"),
                    Some(shell_status.as_str()),
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .child(
                            Button::new("shell-detect")
                                .label(t("Detect", "检测"))
                                .small()
                                .outline()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_detect_shell(cx);
                                })),
                        )
                        .child(
                            Button::new("shell-browse")
                                .label(t("Browse", "浏览"))
                                .small()
                                .outline()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_browse_shell(cx);
                                })),
                        )
                        .into_any_element(),
                ),
                dropdown_field(
                    "shell-kind",
                    t("Kind", "类型"),
                    Some(t("Used to build the launch line", "用于拼接启动命令")),
                    &shell_kind,
                    &shell_options,
                    shell_kind_open,
                    |workspace, open, cx| workspace.on_toggle_shell_kind_menu(open, cx),
                    |workspace, kind, cx| {
                        workspace.on_set_shell_kind(kind, cx);
                        workspace.on_toggle_shell_kind_menu(false, cx);
                    },
                    cx,
                ),
            ],
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
                    "Solid panels over a page gradient. Pick a hue.",
                    "页面渐变之上的实色面板。选一个色调。",
                ))),
        )
        .child(palette_choices(selected, cx))
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
                .border_color(if on { theme.primary } else { theme.border })
                .bg(if on { theme.accent } else { theme.secondary })
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
