//! The General settings page: palette, typeface, language, and request identity.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::Input;
use gpui_kit::component::theme::Theme;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, TestSupportExt as _, Window, div, px,
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
        .w_full()
        .h_auto()
        .flex_none()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_sm()
                .child(t("HTTP User-Agent", "HTTP User-Agent")),
        )
        .child(
            // Fixed height. `Input` is `size_full`; without a definite box
            // its percentage height resolves against the scrollport.
            div()
                .w_full()
                .h(px(30.))
                .min_h(px(30.))
                .max_h(px(30.))
                .flex_none()
                .text_sm()
                .child(Input::new(&ua_input)),
        )
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
        .w_full()
        .h_auto()
        .flex_none()
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

/// Label above a wrapping row of fixed-width swatches.
///
/// Thirteen palettes fill two rows of five and leave three on the last.
/// Cells share one width, so the last row stays on the left. The row is a
/// flex wrap, not a grid: grid row tracks still grew into the settings
/// scrollport and hid the font controls in that gap.
fn palette_field(selected: &str, cx: &mut Context<Workspace>) -> AnyElement {
    div()
        .id("row-palette")
        .w_full()
        .h_auto()
        .flex_none()
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
        .test_support()
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
        .h_auto()
        .flex_none()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .w_full()
                .min_w_0()
                .h_auto()
                .flex_none()
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
        .test_support()
        .into_any_element()
}

fn palette_choices(selected: &str, cx: &mut Context<Workspace>) -> AnyElement {
    let theme = cx.theme().clone();
    let cells = crate::ui::desk::PALETTES
        .into_iter()
        .map(|id| {
            palette_cell(id, selected, &theme)
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    workspace.on_select_palette(id, cx);
                }))
                .test_support()
                .into_any_element()
        })
        .collect::<Vec<_>>();
    palette_swatches(cells)
}

/// Fixed cells on a wrapping row. Five fit the row; the rest wrap left.
///
/// No grid. GPUI row tracks, including max-content, still absorbed the
/// settings scrollport and pushed the font controls off the first screen.
fn palette_swatches(children: Vec<AnyElement>) -> AnyElement {
    div()
        .id("palette-swatches")
        .w_full()
        .max_w(px(crate::ui::desk::palette_row_max_px()))
        .h_auto()
        .flex_none()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_start()
        .content_start()
        .justify_start()
        .gap(px(crate::ui::desk::PALETTE_GAP_PX))
        .children(children)
        .test_support()
        .into_any_element()
}

/// One palette cell: a fixed 72px column with a 40px swatch and a one-line label.
///
/// Width and the color fill are absolute pixels. A percentage size is
/// resolved against the settings scrollport and stretches the row.
fn palette_cell(
    id: &'static str,
    selected: &str,
    theme: &Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let cell_px = px(crate::ui::desk::PALETTE_CELL_PX);
    let swatch_px = px(crate::ui::desk::PALETTE_SWATCH_PX);
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
        .w(cell_px)
        .h_auto()
        .self_start()
        .flex_none()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .child(
            div()
                .id(format!("swatch-{id}"))
                .size(swatch_px)
                .flex_none()
                .flex_shrink_0()
                .rounded(px(8.))
                .border_1()
                .border_color(ring)
                .hover(move |this| this.border_color(hover_ring))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(px(crate::ui::desk::PALETTE_SWATCH_PX - 8.))
                        .flex_none()
                        .rounded(px(6.))
                        .bg(swatch),
                )
                .test_support(),
        )
        .child(
            div()
                .w(cell_px)
                .text_center()
                .text_xs()
                .truncate()
                .text_color(theme.foreground)
                .child(crate::ui::desk::palette_label(id)),
        )
}

#[cfg(all(test, feature = "test-support"))]
mod appearance_layout {
    use gpui_kit::component::ActiveTheme as _;
    use gpui_kit::test::{TestSupportExt as _, TestWindowExt};
    use gpui_kit::{
        AppContext as _, Context, InteractiveElement, IntoElement, ParentElement, Render,
        ScrollDelta, StatefulInteractiveElement, Styled, TestAppContext, Window, div, point, px,
        rems, size,
    };

    use super::super::widgets::settings_card;
    use super::{palette_cell, palette_swatches};

    /// Settings scrollport at a normal desktop size. The shell matches
    /// `#settings-content`: a fixed-height flex column whose page does not grow.
    struct AppearanceProbe;

    impl Render for AppearanceProbe {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = cx.theme().clone();
            let swatches = palette_swatches(
                crate::ui::desk::PALETTES
                    .into_iter()
                    .map(|id| {
                        palette_cell(id, "slate", &theme)
                            .test_support()
                            .into_any_element()
                    })
                    .collect(),
            );
            let font = probe_dropdown(
                "font-family",
                "Interface font",
                "System uses the operating-system UI font. Other choices are fonts this computer can load.",
                "System",
            );
            let size_row = probe_font_size();
            let language = probe_dropdown(
                "language",
                "Language",
                "Interface language. English and Simplified Chinese are built in.",
                "English",
            );
            let agent = probe_user_agent();
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().h(px(56.)).flex_none().child("Settings"))
                .child(
                    div()
                        .id("settings-body")
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_row()
                        .child(div().id("settings-nav").w(px(236.)).h_full().flex_none())
                        .child(
                            div()
                                .id("settings-content")
                                .flex_1()
                                .min_w_0()
                                .min_h_0()
                                .h_full()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .justify_start()
                                .test_support()
                                .child(
                                    div()
                                        .id("settings-content-inner")
                                        .mx_auto()
                                        .max_w(rems(52.))
                                        .w_full()
                                        .h_auto()
                                        .flex_none()
                                        .flex()
                                        .flex_col()
                                        .justify_start()
                                        .px_6()
                                        .py_4()
                                        .child(settings_card(
                                            "appearance",
                                            "Appearance",
                                            Some("Palette, typeface, size, and language."),
                                            &theme,
                                            vec![
                                                div()
                                                    .id("row-palette")
                                                    .w_full()
                                                    .h_auto()
                                                    .flex_none()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_2()
                                                    .child("Palette")
                                                    .child(swatches)
                                                    .test_support()
                                                    .into_any_element(),
                                                font,
                                                size_row,
                                                language,
                                                agent,
                                            ],
                                        )),
                                )
                                .test_support(),
                        ),
                )
        }
    }

    /// Mirrors `dropdown_field` so the layout test measures the real row, not a
    /// short stand-in. Clicks are no-ops; only the box tree matters here.
    fn probe_dropdown(
        id: &str,
        label: &str,
        description: &str,
        current: &str,
    ) -> gpui_kit::AnyElement {
        div()
            .id(format!("dropdown-{id}"))
            .w_full()
            .h_auto()
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .id(format!("dropdown-row-{id}"))
                    .w_full()
                    .min_w_0()
                    .h_auto()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .test_support()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(120.))
                            .h_auto()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(div().text_sm().whitespace_normal().child(label.to_owned()))
                            .child(
                                div()
                                    .text_xs()
                                    .opacity(0.5)
                                    .whitespace_normal()
                                    .child(description.to_owned()),
                            ),
                    )
                    .child(
                        div()
                            .id(format!("dropdown-button-{id}"))
                            .h(px(32.))
                            .w(px(200.))
                            .flex_none()
                            .px_2()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .test_support()
                            .child(
                                div()
                                    .flex_none()
                                    .whitespace_nowrap()
                                    .text_sm()
                                    .child(current.to_owned()),
                            )
                            .child(div().size(px(14.)).flex_none()),
                    ),
            )
            .test_support()
            .into_any_element()
    }

    /// Mirrors `font_size_field` plus `choice_chips`.
    fn probe_font_size() -> gpui_kit::AnyElement {
        div()
            .id("row-font-size")
            .w_full()
            .h_auto()
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .h_auto()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(div().text_sm().whitespace_normal().child("Interface font size"))
                    .child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .whitespace_normal()
                            .child("S / M / L / XL, about 12 / 13 / 14 / 16 px. Chat, the sidebar, and settings follow it immediately."),
                    ),
            )
            .child(
                div()
                    .id("chips-font-size")
                    .w_full()
                    .h_auto()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_start()
                    .content_start()
                    .justify_start()
                    .gap_1()
                    .children(["s", "m", "l", "xl"].into_iter().map(|value| {
                        div()
                            .id(format!("font-size-{value}"))
                            .h(px(28.))
                            .px_2()
                            .flex_none()
                            .self_start()
                            .flex()
                            .items_center()
                            .child(value.to_uppercase())
                            .test_support()
                    })),
            )
            .test_support()
            .into_any_element()
    }

    fn probe_user_agent() -> gpui_kit::AnyElement {
        div()
            .id("row-user-agent")
            .w_full()
            .h_auto()
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().child("HTTP User-Agent"))
            .child(
                div()
                    .w_full()
                    .h(px(30.))
                    .min_h(px(30.))
                    .max_h(px(30.))
                    .flex_none()
                    .text_sm()
                    .child("pi"),
            )
            .test_support()
            .into_any_element()
    }

    #[gpui_kit::test]
    fn appearance_card_keeps_font_controls_under_the_palette(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| cx.set_reduce_motion(true));
        let handle = cx.open_window(size(px(1280.), px(480.)), |_, _| AppearanceProbe);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let swatches = window.find("palette-swatches").bounds();
            let swatch = window.find("swatch-slate").bounds();
            let slate = window.find("palette-slate").bounds();
            let sand = window.find("palette-sand").bounds();
            let rose = window.find("palette-rose").bounds();
            let plum = window.find("palette-plum").bounds();
            let aurora = window.find("palette-aurora").bounds();
            let font = window.find("dropdown-font-family").bounds();
            let font_row = window.find("dropdown-row-font-family").bounds();
            let font_button = window.find("dropdown-button-font-family").bounds();
            let size_row = window.find("row-font-size").bounds();
            let language = window.find("dropdown-language").bounds();
            let agent = window.find("row-user-agent").bounds();
            let chip = window.find("font-size-s").bounds();
            let card = window.find("card-appearance").bounds();
            let content = window.find("settings-content").bounds();

            assert!(
                (swatch.size.width - px(40.)).abs() <= px(2.)
                    && (swatch.size.height - px(40.)).abs() <= px(2.),
                "swatch should stay a 40px square, got {swatch:?}"
            );
            assert!(
                (slate.size.width - px(crate::ui::desk::PALETTE_CELL_PX)).abs() <= px(2.),
                "palette cell should stay {0}px wide, got {slate:?}",
                crate::ui::desk::PALETTE_CELL_PX
            );
            assert!(
                swatches.size.height < px(280.),
                "palette row should be content height, got {swatches:?}"
            );
            assert!(
                (sand.origin.y - slate.origin.y).abs() <= px(2.)
                    && rose.origin.y > slate.bottom()
                    && rose.origin.y - slate.bottom() < px(24.),
                "five cells on the first row, sixth wraps under it: slate {slate:?} sand {sand:?} rose {rose:?}"
            );
            assert!(
                (plum.origin.x - slate.origin.x).abs() <= px(2.)
                    && aurora.origin.x > plum.origin.x
                    && aurora.right() < slate.origin.x + px(crate::ui::desk::palette_row_max_px()),
                "last row stays left-aligned: plum {plum:?} aurora {aurora:?}"
            );
            assert!(
                font.size.height < px(120.)
                    && size_row.size.height < px(120.)
                    && (font.size.height - font_row.size.height).abs() <= px(2.),
                "font controls should be the row, not a stretched section: font {font:?} row {font_row:?} size {size_row:?}"
            );
            assert!(
                (font_button.size.width - px(200.)).abs() <= px(2.)
                    && (font_button.size.height - px(32.)).abs() <= px(2.),
                "interface font button should paint at its fixed size, got {font_button:?}"
            );
            assert!(
                card.size.height > content.size.height && card.size.height < px(920.),
                "appearance card should scroll inside the pane instead of growing a blank band, card {card:?} content {content:?}"
            );

            let font_gap = font.origin.y - swatches.bottom();
            let size_gap = size_row.origin.y - font.bottom();
            let language_gap = language.origin.y - size_row.bottom();
            let agent_gap = agent.origin.y - language.bottom();
            assert!(
                font_gap >= px(0.) && font_gap < px(80.),
                "gap between palette and interface font is {font_gap:?}, swatches {swatches:?} font {font:?}"
            );
            assert!(
                size_gap >= px(0.) && size_gap < px(80.),
                "gap between interface font and S–XL is {size_gap:?}"
            );
            assert!(
                language_gap >= px(0.) && language_gap < px(80.),
                "gap between S–XL and language is {language_gap:?}"
            );
            assert!(
                agent_gap >= px(0.) && agent_gap < px(80.),
                "gap between language and user-agent is {agent_gap:?}"
            );
            assert!(
                chip.size.height > px(16.) && chip.size.height < px(48.),
                "S chip should paint at content height, got {chip:?}"
            );
            assert!(
                agent.bottom() < px(920.),
                "palette through user-agent should stay one short page, ua {agent:?}"
            );

            // Scrolling the settings pane must not panic, and the same
            // sections have to stay content-sized after the offset changes.
            window.scroll(
                "settings-content",
                ScrollDelta::Pixels(point(px(0.), px(-240.))),
                cx,
            );
            let font_after = window.find("dropdown-font-family").bounds();
            let size_after = window.find("row-font-size").bounds();
            let language_after = window.find("dropdown-language").bounds();
            let agent_after = window.find("row-user-agent").bounds();
            let scrolled_gap = size_after.origin.y - font_after.bottom();
            let scrolled_language = language_after.origin.y - size_after.bottom();
            let scrolled_agent = agent_after.origin.y - language_after.bottom();
            assert!(
                font_after.size.height < px(120.)
                    && size_after.size.height < px(120.)
                    && scrolled_gap >= px(0.)
                    && scrolled_gap < px(80.)
                    && scrolled_language >= px(0.)
                    && scrolled_language < px(80.)
                    && scrolled_agent >= px(0.)
                    && scrolled_agent < px(80.),
                "scroll should keep font, size, language, and user-agent packed: font {font_after:?} size {size_after:?} language {language_after:?} ua {agent_after:?}"
            );
        })
        .unwrap();
    }
}
