//! The Magpie settings page: the gateway binding, the model magpie picked,
//! and the model list the gateway published.
//!
//! MCode has no provider list of its own any more: everything here is read
//! from `settings.magpie` (written by magpie's agent plugin) and the gateway's
//! `/v1/models` answer.

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::Button;

use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::widgets::settings_card;
use crate::i18n::t;
use crate::ui::desk::Desk;
use crate::workspace::Workspace;

pub(crate) fn render_models_section(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme().clone();
    let desk = Desk::of(&theme);
    let vm = workspace.vm();
    let Some(settings) = vm.settings.as_ref() else {
        return div().into_any_element();
    };
    let magpie = settings.magpie.clone();
    let models = vm
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.provider(mycode_frontend::MAGPIE_PROVIDER_ID))
        .map(|provider| provider.models.clone())
        .unwrap_or_default();
    let connected = magpie.is_active();
    let listed = !models.is_empty();
    let status = if connected && listed {
        format!(
            "{} {}",
            models.len(),
            t("models from magpie", "个模型来自 magpie")
        )
    } else if connected {
        t(
            "Connected; the model list has not loaded yet",
            "已连接；模型列表还没加载",
        )
        .to_owned()
    } else {
        t(
            "Not connected. Pick MCode on magpie's Agents page.",
            "未连接。请在 magpie 的 Agents 页给 MCode 选一个模型。",
        )
        .to_owned()
    };
    let status_color = if connected { desk.green } else { desk.amber };

    // The detail row and the dropdown both name the model the way the model
    // list does, instead of showing the raw id here and a display name there.
    let current_name = if magpie.model.is_empty() {
        t("No model picked in magpie", "magpie 里还没选模型").to_owned()
    } else {
        crate::view_model::model_display_name(
            workspace.vm(),
            mycode_frontend::MAGPIE_PROVIDER_ID,
            &magpie.model,
        )
    };
    let current = current_name.clone();
    let base_url = if magpie.base_url.is_empty() {
        t("No gateway address", "没有网关地址").to_owned()
    } else {
        magpie.base_url.clone()
    };

    let selected_id = crate::view_model::canonical_model_id(
        vm,
        mycode_frontend::MAGPIE_PROVIDER_ID,
        &magpie.model,
    );
    let rows: Vec<AnyElement> = models
        .iter()
        .map(|model| {
            let selected = model.id == selected_id;
            let name = crate::view_model::model_display_name(
                vm,
                mycode_frontend::MAGPIE_PROVIDER_ID,
                &model.id,
            );
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .py_1()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .when(selected, |this| this.text_color(desk.green))
                                .child(name),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(model.id.clone()),
                        ),
                )
                .when(model.context > 0, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{}k ctx", model.context / 1_000)),
                    )
                })
                .when(selected, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(desk.green)
                            .child(t("current", "当前")),
                    )
                })
                .into_any_element()
        })
        .collect();

    let empty = div()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(if connected {
            t(
                "No models yet. Refresh, or pick one in magpie.",
                "还没有模型。刷新一下，或先在 magpie 里选一个。",
            )
        } else {
            t(
                "magpie owns the model list, MCP servers, skills, and rules.",
                "模型列表、MCP 服务、技能和规则都由 magpie 管理。",
            )
        });

    // One collapsed dropdown row instead of an always-visible list of every
    // gateway model: the row names the model magpie picked and expands to the
    // full list, matching the default-model dropdown the Models page used
    // before the gateway.
    let models_open = workspace.magpie_models_open;
    let toggle = div()
        .id("magpie-model-toggle")
        .w_full()
        .min_w_0()
        .h(px(36.))
        .px_2()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .rounded(crate::ui::skin::radius_control())
        .border_1()
        .border_color(crate::ui::skin::glass_border(&theme))
        .bg(crate::ui::skin::frost(&theme))
        .cursor_pointer()
        .hover(|this| this.bg(crate::ui::skin::frost_hover(&theme)))
        .on_click(cx.listener(|workspace, _, _, cx| {
            workspace.magpie_models_open = !workspace.magpie_models_open;
            cx.notify();
        }))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_sm()
                .child(current_name),
        )
        .child(
            Icon::new(if models_open {
                IconName::ChevronUp
            } else {
                IconName::ChevronDown
            })
            .xsmall()
            .text_color(theme.muted_foreground),
        )
        .into_any_element();

    let has_rows = !rows.is_empty();
    let list = models_open.then(|| div().flex().flex_col().children(rows).into_any_element());

    div()
        .id("magpie-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_card(
            "magpie",
            "Magpie",
            Some(t(
                "MCode runs every turn through magpie's local gateway. \
                 The model list, MCP servers, skills, and rules come from magpie.",
                "MCode 的每一轮请求都走 magpie 的本机网关；\
                 模型列表、MCP 服务、技能和规则都来自 magpie。",
            )),
            &theme,
            vec![
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(IconName::Check).small().text_color(status_color))
                    .child(div().text_sm().text_color(status_color).child(status))
                    .into_any_element(),
                div()
                    .when(!connected, |this| {
                        this.flex()
                            .flex_col()
                            .gap_1()
                            .p_2()
                            .rounded(px(8.))
                            .bg(theme.secondary)
                            .child(div().text_xs().child(t(
                                "Install magpie from https://usemagpie.ai, start it, then run:",
                                "从 https://usemagpie.ai 安装 magpie 并启动，然后运行：",
                            )))
                            .child(
                                div()
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .child("magpie plugin add github:MCapricorns/magpie-mycode"),
                            )
                            .child(div().text_xs().text_color(theme.muted_foreground).child(t(
                                "Then pick MCode and a model on magpie's Agents page.",
                                "然后在 magpie 的 Agents 页给 MCode 选一个模型。",
                            )))
                    })
                    .into_any_element(),
                status_row(t("Model", "模型"), current, &theme).into_any_element(),
                status_row(t("Gateway", "网关"), base_url, &theme).into_any_element(),
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .pt_1()
                    .child(
                        Button::new("magpie-refresh")
                            .icon(IconName::RefreshCw)
                            .label(t("Refresh models", "刷新模型"))
                            .small()
                            .on_click(cx.listener(|workspace, _event, _window, cx| {
                                workspace.on_refresh_catalog(cx);
                            })),
                    )
                    .into_any_element(),
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(crate::ui::skin::WEIGHT_STRONG)
                            .child(t("Models", "模型")),
                    )
                    .child(toggle)
                    .when_some(list, |this, list| this.child(list))
                    .when(!has_rows, |this| this.child(empty.into_any_element()))
                    .into_any_element(),
            ],
        ))
        .into_any_element()
}

fn status_row(label: &str, value: String, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_baseline()
        .gap_3()
        .child(
            div()
                .w(px(88.))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label.to_owned()),
        )
        .child(div().flex_1().min_w_0().text_sm().child(value))
}
