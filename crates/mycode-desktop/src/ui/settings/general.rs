//! The General settings page: appearance, text size, language, and the
//! network proxy.
//!
//! The interface always paints the operating-system UI font; there is no
//! typeface picker. The request identity (User-Agent) is fixed by the
//! application and is not shown here.
use gpui_kit::component::button::Button;
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled,
    TestSupportExt as _, Window, div, px,
};
use mycode_frontend::view_model::ProxyTestState;

use super::widgets::{DropdownSpec, choice_chips, dropdown_field, settings_card, settings_row};
use crate::i18n::t;
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
    let proxy_row = proxy_field(workspace, window, cx);
    let theme_id = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| settings.theme.clone())
        .unwrap_or_else(|| "dark".to_owned());
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
    let language_label_now = language_label(&configured_language).to_owned();
    let language_options = ["auto", "en", "zh"]
        .iter()
        .map(|id| language_label(id).to_owned())
        .collect::<Vec<_>>();
    let theme_row = appearance_theme_field(&theme_id, cx);
    let font_row = font_size_field(&font_size, cx);
    let language_row = dropdown_field(
        DropdownSpec {
            id: "language",
            label: t("Language", "语言"),
            description: Some(t(
                "Interface language. English and Simplified Chinese are built in.",
                "界面语言。内置英文与简体中文。",
            )),
            current: &language_label_now,
            options: &language_options,
            open: workspace.vm().language_menu_open,
        },
        |workspace, open, cx| workspace.on_toggle_language_menu(open, cx),
        |workspace, label, cx| {
            if let Some(id) = language_id_for_label(label) {
                workspace.on_select_language(id, cx);
            }
            workspace.on_toggle_language_menu(false, cx);
        },
        cx,
    );
    let theme = cx.theme();
    div()
        .id("general-section")
        .w_full()
        .h_auto()
        .flex_none()
        .flex()
        .flex_col()
        .gap(crate::ui::skin::space(4))
        .child(settings_card(
            "appearance",
            t("Appearance", "外观"),
            Some(t(
                "Applies immediately and is saved to settings.",
                "立即生效并写入设置。",
            )),
            theme,
            vec![theme_row, font_row, language_row],
        ))
        .child(settings_card(
            "network",
            t("Network", "网络"),
            Some(t(
                "Proxy for model, search, and update requests. Local models always connect directly.",
                "模型、搜索与更新请求使用的代理。本地模型始终直连。",
            )),
            theme,
            vec![proxy_row],
        ))
        .into_any_element()
}

/// Mode chip id for one stored proxy value.
fn proxy_mode(stored: &str) -> &'static str {
    match mycode_config::ProxyPreference::parse(stored) {
        Some(mycode_config::ProxyPreference::Url(_)) => "manual",
        _ => "system",
    }
}

/// One line for the proxy test result and whether it reads as success
/// (`None` while idle or running).
fn proxy_test_line(test: &ProxyTestState) -> (String, Option<bool>) {
    use mycode_app::ProxyTestRoute;
    match test {
        ProxyTestState::Idle => (
            t(
                "Test fetches google.com through the setting above.",
                "测试会按上面的设置请求 google.com。",
            )
            .to_owned(),
            None,
        ),
        ProxyTestState::Testing => (t("Testing…", "测试中…").to_owned(), None),
        ProxyTestState::Invalid(message) => (message.clone(), Some(false)),
        ProxyTestState::Done(report) => {
            let route = match &report.route {
                ProxyTestRoute::Direct => t(
                    "direct (no system proxy is set)",
                    "直连（系统没有设置代理）",
                )
                .to_owned(),
                ProxyTestRoute::System(uri) => format!("{} {uri}", t("system proxy", "系统代理")),
                ProxyTestRoute::Manual(url) => format!("{} {url}", t("proxy", "代理")),
            };
            match &report.outcome {
                Ok((status, elapsed)) => (
                    format!(
                        "{} · {route} · HTTP {status} · {} ms",
                        t("Reached google.com", "已连通 google.com"),
                        elapsed.as_millis()
                    ),
                    Some((200..400).contains(status)),
                ),
                Err(message) => (
                    format!(
                        "{} · {route} · {}",
                        t("google.com unreachable", "google.com 不通"),
                        mycode_app::brief_error(message)
                    ),
                    Some(false),
                ),
            }
        }
    }
}

fn proxy_field(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let stored = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| settings.proxy.clone())
        .unwrap_or_default();
    let mode = proxy_mode(&stored);
    let options = [
        ("system", t("Follow system", "跟随系统")),
        ("manual", t("Manual", "手动填写")),
    ]
    .into_iter()
    .map(|(id, label)| (id.to_owned(), label.to_owned()))
    .collect::<Vec<_>>();
    let hint = match mode {
        "manual" => t(
            "http, https, socks5, or socks5h. A bare host:port such as 127.0.0.1:7890 is read as http.",
            "支持 http、https、socks5、socks5h。只填 host:port（如 127.0.0.1:7890）按 http 处理。",
        ),
        _ => t(
            "Uses the system proxy (environment variables or the OS setting, e.g. Clash \"System Proxy\"). With none set, requests connect directly.",
            "使用系统代理（环境变量或系统设置，例如 Clash「系统代理」）。系统没有设置代理时直连。",
        ),
    };
    let (test_text, test_ok) = proxy_test_line(&workspace.vm().proxy_test);
    let testing = workspace.vm().proxy_test == ProxyTestState::Testing;
    let input = (mode == "manual").then(|| workspace.settings_proxy_input(window, cx));
    let invalid = input.as_ref().is_some_and(|input| {
        !matches!(
            mycode_config::ProxyPreference::parse(input.read(cx).value().trim()),
            Some(mycode_config::ProxyPreference::Url(_))
        )
    });
    let chips = choice_chips(
        "proxy-mode",
        &options,
        mode,
        |workspace, value, cx| workspace.on_select_proxy_mode(value, cx),
        cx,
    );
    let test_button = Button::new("proxy-test")
        .label(t("Test", "测试"))
        .small()
        .outline()
        .disabled(testing)
        .on_click(cx.listener(|workspace, _, _, cx| workspace.on_test_proxy(cx)));
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let danger = theme.danger;
    let test_color = match test_ok {
        Some(true) => theme.success,
        Some(false) => danger,
        None => muted,
    };
    div()
        .id("row-proxy")
        .w_full()
        .h_auto()
        .flex_none()
        .flex()
        .flex_col()
        .gap(crate::ui::skin::space(2))
        .child(settings_row(
            "proxy-mode",
            t("Proxy", "代理"),
            None,
            chips,
            theme,
        ))
        .children(input.map(|input| {
            // Fixed height: `Input` is `size_full`; without a definite box
            // its percentage height resolves against the scrollport.
            div()
                .w_full()
                .h(px(32.))
                .min_h(px(32.))
                .max_h(px(32.))
                .flex_none()
                .text_sm()
                .child(Input::new(&input))
        }))
        .child(
            div()
                .w_full()
                .min_w_0()
                .text_xs()
                .whitespace_normal()
                .text_color(if invalid { danger } else { muted })
                .child(if invalid {
                    t(
                        "Not a proxy URL yet; the last valid one stays in use.",
                        "还不是有效的代理地址；继续使用上一个有效值。",
                    )
                } else {
                    hint
                }),
        )
        .child(
            div()
                .id("proxy-test-row")
                .w_full()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap(crate::ui::skin::space(2))
                .child(test_button)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .whitespace_normal()
                        .text_color(test_color)
                        .child(test_text),
                ),
        )
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
    let chips = choice_chips(
        "font-size",
        &options,
        selected,
        |workspace, value, cx| workspace.on_select_font_size(value, cx),
        cx,
    );
    div()
        .id("row-font-size")
        .w_full()
        .h_auto()
        .flex_none()
        .child(settings_row(
            "font-size-row",
            t("Text size", "界面字号"),
            Some(t(
                "Body text at about 12 / 14 / 16 / 18 px. The typeface is always the system UI font.",
                "正文约 12 / 14 / 16 / 18 像素。字体固定使用系统界面字体。",
            )),
            chips,
            cx.theme(),
        ))
        .test_support()
        .into_any_element()
}

fn appearance_theme_field(selected: &str, cx: &mut Context<Workspace>) -> AnyElement {
    let options = [("dark", t("Dark", "深色")), ("light", t("Light", "浅色"))]
        .into_iter()
        .map(|(id, label)| (id.to_owned(), label.to_owned()))
        .collect::<Vec<_>>();
    let chips = choice_chips(
        "appearance-theme",
        &options,
        selected,
        |workspace, value, cx| workspace.on_select_theme(value, cx),
        cx,
    );
    settings_row(
        "appearance-theme",
        t("Theme", "主题"),
        Some(t("Dark or light.", "深色或浅色。")),
        chips,
        cx.theme(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn language_dropdown_keeps_follow_system_visible_when_configured_auto() {
        assert_eq!(
            super::language_label("auto"),
            crate::i18n::t("Follow system", "跟随系统")
        );
        assert_eq!(super::language_label("en"), "English");
        assert_eq!(super::language_label("zh"), "中文");
        assert_eq!(
            super::language_id_for_label(super::language_label("auto")),
            Some("auto")
        );
        assert_eq!(super::language_id_for_label("Follow system"), Some("auto"));
        assert_eq!(super::language_id_for_label("跟随系统"), Some("auto"));
        assert_eq!(super::language_id_for_label("English"), Some("en"));
        assert_eq!(super::language_id_for_label("中文"), Some("zh"));
    }
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

    /// Settings scrollport at a normal desktop size. The shell matches
    /// `#settings-content`: a fixed-height flex column whose page does not grow.
    struct AppearanceProbe;

    impl Render for AppearanceProbe {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = cx.theme().clone();
            let size_row = probe_font_size();
            let language = probe_dropdown(
                "language",
                "Language",
                "Interface language. English and Simplified Chinese are built in.",
                "English",
            );
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().h(px(44.)).flex_none())
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
                                        .gap_4()
                                        .px_6()
                                        .py_4()
                                        .child(
                                            div()
                                                .id("settings-page-header")
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .pb_1()
                                                .child(div().text_lg().child("General"))
                                                .child(
                                                    div().text_sm().child(
                                                        "Appearance, language, and identity",
                                                    ),
                                                ),
                                        )
                                        .child(settings_card(
                                            "appearance",
                                            "Appearance",
                                            Some("Applies immediately and is saved to settings."),
                                            &theme,
                                            vec![size_row, language],
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
                            .min_w_0()
                            .h_auto()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .text_sm()
                                    .whitespace_normal()
                                    .child(label.to_owned()),
                            )
                            .child(
                                div()
                                    .id(format!("dropdown-hint-{id}"))
                                    .w_full()
                                    .min_w_0()
                                    .text_xs()
                                    .opacity(0.5)
                                    .whitespace_normal()
                                    .child(description.to_owned())
                                    .test_support(),
                            ),
                    )
                    .child(
                        div()
                            .id(format!("dropdown-button-{id}"))
                            .h(px(32.))
                            .w(px(200.))
                            .max_w(px(200.))
                            .flex_none()
                            .flex_shrink_0()
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

    /// Mirrors `font_size_field`: a `settings_row` with the label column on
    /// the left and the `choice_chips` track on the right.
    fn probe_font_size() -> gpui_kit::AnyElement {
        div()
            .id("row-font-size")
            .w_full()
            .h_auto()
            .flex_none()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().whitespace_normal().child("Text size"))
                            .child(
                                div()
                                    .id("font-size-hint")
                                    .text_xs()
                                    .whitespace_normal()
                                    .child(
                                        "Body text at about 12 / 14 / 16 / 18 px. The typeface is always the system UI font.",
                                    )
                                    .test_support(),
                            ),
                    )
                    .child(
                        div().flex_none().child(
                            div()
                                .id("chips-font-size")
                                .max_w_full()
                                .min_w_0()
                                .h_auto()
                                .flex_none()
                                .self_start()
                                .flex()
                                .flex_row()
                                .flex_wrap()
                                .items_center()
                                .gap(px(2.))
                                .p(px(2.))
                                .border_1()
                                .test_support()
                                .children(["s", "m", "l", "xl"].into_iter().map(|value| {
                                    div()
                                        .id(format!("font-size-{value}"))
                                        .h(px(26.))
                                        .min_w(px(40.))
                                        .px(px(12.))
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .whitespace_nowrap()
                                        .child(value.to_uppercase())
                                        .test_support()
                                })),
                        ),
                    ),
            )
            .test_support()
            .into_any_element()
    }

    #[gpui_kit::test]
    fn appearance_card_packs_size_and_language(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| cx.set_reduce_motion(true));
        let handle = cx.open_window(size(px(1280.), px(480.)), |_, _| AppearanceProbe);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let size_row = window.find("row-font-size").bounds();
            let track = window.find("chips-font-size").bounds();
            let hint = window.find("font-size-hint").bounds();
            let language = window.find("dropdown-language").bounds();
            let language_button = window.find("dropdown-button-language").bounds();
            let language_hint = window.find("dropdown-hint-language").bounds();
            let chip = window.find("font-size-s").bounds();
            let chip_xl = window.find("font-size-xl").bounds();
            let card = window.find("card-appearance").bounds();
            let content = window.find("settings-content").bounds();

            assert!(
                size_row.origin.y >= content.origin.y && language.bottom() <= content.bottom(),
                "size and language should be on the first screen: size {size_row:?} language {language:?} content {content:?}"
            );
            assert!(
                size_row.size.height < px(80.) && language.size.height < px(120.),
                "rows should be content-sized, not stretched: size {size_row:?} language {language:?}"
            );
            assert!(
                (language_button.size.width - px(200.)).abs() <= px(2.)
                    && (language_button.size.height - px(32.)).abs() <= px(2.)
                    && language_button.right() <= card.right() + px(1.)
                    && language_hint.right() <= language_button.left() + px(4.),
                "language dropdown should sit right of its helper, inside the card: hint {language_hint:?} button {language_button:?} card {card:?}"
            );
            assert!(
                hint.right() <= track.left() + px(4.)
                    && chip_xl.right() <= card.right() + px(1.)
                    && track.right() > card.right() - px(80.),
                "size track should sit at the right edge beside its helper: hint {hint:?} track {track:?} card {card:?}"
            );
            assert!(
                chip.size.height > px(16.) && chip.size.height < px(40.),
                "S chip should paint at content height, got {chip:?}"
            );

            let language_gap = language.origin.y - size_row.bottom();
            assert!(
                language_gap >= px(0.) && language_gap < px(80.),
                "gap between size and language is {language_gap:?}"
            );

            // Scrolling the settings pane must not panic, and the rows have
            // to stay content-sized after the offset changes.
            window.scroll(
                "settings-content",
                ScrollDelta::Pixels(point(px(0.), px(-240.))),
                cx,
            );
            let size_after = window.find("row-font-size").bounds();
            let language_after = window.find("dropdown-language").bounds();
            let scrolled_gap = language_after.origin.y - size_after.bottom();
            assert!(
                size_after.size.height < px(80.)
                    && language_after.size.height < px(120.)
                    && scrolled_gap >= px(0.)
                    && scrolled_gap < px(80.),
                "scroll should keep size and language packed: size {size_after:?} language {language_after:?}"
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn size_helper_wraps_inside_a_narrow_appearance_card(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| cx.set_reduce_motion(true));
        let handle = cx.open_window(size(px(1024.), px(720.)), |_, _| AppearanceProbe);
        cx.update_window(handle.into(), |_, window, _cx| {
            window.render_frame(_cx);
            let hint = window.find("font-size-hint").bounds();
            let track = window.find("chips-font-size").bounds();
            let card = window.find("card-appearance").bounds();
            let language = window.find("dropdown-language").bounds();
            let content = window.find("settings-content").bounds();
            assert!(
                language.bottom() <= content.bottom(),
                "language should stay on the first screen, language {language:?} content {content:?}"
            );
            assert!(
                track.right() <= card.right() + px(1.) && hint.right() <= track.left() + px(4.),
                "track and helper should stay inside the card: hint {hint:?} track {track:?} card {card:?}"
            );
            assert!(
                hint.size.height > px(22.) && hint.size.height < px(80.),
                "size helper should wrap onto more than one line, got {hint:?}"
            );
        })
        .unwrap();
    }
}
