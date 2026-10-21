//! Two-step model picker shared by the composer, the default-model control,
//! and each subagent role.
//!
//! The provider step is a short card list. The model step is a search box
//! plus a virtualized list of that provider's catalog and configured ids.
//! The catalog is directory data: the list is filtered, never truncated.
//! Which rows exist and which one Enter takes live in
//! [`mycode_frontend::picker`]; this module only paints them.
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::i18n::t;
use crate::ui::skin::{self, menu_panel};
use crate::view_model::FrontendAction;
pub(crate) use crate::view_model::{model_display_name, provider_display_name};
use crate::workspace::Workspace;
use mycode_frontend::picker::{ModelChoice, current_provider, enabled_providers, recent_pins};
pub(crate) use mycode_frontend::picker::{
    ModelPickerTarget, ProviderChoice, filter_model_choices, preferred_index, provider_choices,
    provider_model_choices, provider_scroll_index, selected_in_provider, selected_model_label,
};

/// Fixed height of one virtualized model row.
const MODEL_ROW_HEIGHT: f32 = 32.;
/// How many model rows the panel shows before the list scrolls.
const MODEL_LIST_VISIBLE: usize = 8;

/// The picker panel. Caller decides when it is mounted.
pub(crate) fn render_model_picker(
    workspace: &mut Workspace,
    window: &mut Window,
    target: ModelPickerTarget,
    max_height: gpui_kit::Pixels,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let browse = workspace.vm().model_menu_browse.clone();
    let body = if let Some(provider_id) = browse {
        let query = workspace.vm().picker_query.clone();
        let input = workspace.model_picker_input(window, cx);
        if workspace.take_model_step_focus() {
            input.update(cx, |state, cx| {
                state.set_value("", window, cx);
                state.focus(window, cx);
            });
        }
        render_model_step(workspace, &provider_id, &query, &input, &target, cx)
    } else {
        render_provider_step(workspace, &target, cx)
    };
    menu_panel("model-picker", cx.theme())
        .w(px(340.))
        .max_w_full()
        .max_h(max_height)
        .overflow_y_scroll()
        .flex_none()
        .p_1()
        .flex()
        .flex_col()
        .gap_1()
        .child(body)
        .into_any_element()
}

fn render_provider_step(
    workspace: &mut Workspace,
    target: &ModelPickerTarget,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    let enabled = enabled_providers(workspace.vm());
    let recent = recent_pins(workspace.vm());
    let mut rows: Vec<AnyElement> = Vec::new();
    let preferred = workspace.provider_highlight(provider_choices(workspace.vm(), target).len());
    let mut index = 0;
    if let ModelPickerTarget::Role(role) = target {
        rows.push(session_model_row(role, index == preferred, cx));
        index += 1;
    }
    if !recent.is_empty() {
        rows.push(section_label(
            "model-picker-recent",
            t("Recent", "最近"),
            theme,
        ));
        for pin in &recent {
            rows.push(pin_row(workspace, pin, target, index == preferred, cx));
            index += 1;
        }
    }
    if enabled.is_empty() {
        rows.push(
            div()
                .id("model-picker-no-providers")
                .px_2()
                .py_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(t(
                    "No model source. Install or start magpie, then pick a model in its Agents page.",
                    "没有模型来源。请安装或启动 magpie，然后在它的 Agents 页选一个模型。",
                ))
                .into_any_element(),
        );
    } else {
        rows.push(section_label(
            "model-picker-providers",
            t("Providers", "服务商"),
            theme,
        ));
        let current = current_provider(workspace.vm(), target);
        for (id, name) in &enabled {
            let selected = current.as_deref() == Some(id.as_str());
            let provider_id = id.clone();
            rows.push(
                div()
                    .id(format!("model-provider-{id}"))
                    .h(px(40.))
                    .px_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .rounded(skin::radius_control())
                    .cursor_pointer()
                    .hover(|this| this.bg(skin::frost_hover(theme)))
                    .when(index == preferred, |this| this.bg(skin::frost(theme)))
                    .on_click(cx.listener(move |workspace, _, _, cx| {
                        workspace.on_browse_model_provider(Some(&provider_id), cx);
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_sm()
                            .child(name.clone()),
                    )
                    // The current provider is marked with a check, not a
                    // heavier face, so the row does not reflow on select.
                    .when(selected, |this| {
                        this.child(
                            Icon::new(IconName::Check)
                                .xsmall()
                                .text_color(theme.foreground),
                        )
                    })
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .xsmall()
                            .text_color(theme.muted_foreground),
                    )
                    .into_any_element(),
            );
            index += 1;
        }
    }
    div()
        .id("model-picker-provider-step")
        .track_scroll(workspace.provider_scroll())
        .max_h(px(400.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_1()
        .children(rows)
        .into_any_element()
}

fn render_model_step(
    workspace: &mut Workspace,
    provider_id: &str,
    query: &str,
    input: &gpui_kit::Entity<gpui_kit::component::input::InputState>,
    target: &ModelPickerTarget,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    let provider_name = provider_display_name(workspace.vm(), provider_id);
    let choices = provider_model_choices(workspace.vm(), provider_id);
    let filtered = filter_model_choices(&choices, query);
    let selected = selected_in_provider(workspace.vm(), target, provider_id);
    let preferred = workspace.model_highlight(target, provider_id, &filtered);
    let scroll = workspace.model_scroll().clone();
    let row_id_prefix = match target {
        ModelPickerTarget::Session => format!("model-{provider_id}"),
        ModelPickerTarget::Role(role) => format!("agent-model-{role}-{provider_id}"),
    };
    let list = if filtered.is_empty() {
        div()
            .id("model-picker-empty")
            .h(px(MODEL_ROW_HEIGHT))
            .px_2()
            .flex()
            .items_center()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(t("No matches", "没有匹配的模型"))
            .into_any_element()
    } else {
        let rows = Rc::new(filtered);
        let list_rows = rows.clone();
        let weak = cx.weak_entity();
        let provider = provider_id.to_owned();
        let target = target.clone();
        let height = px((rows.len().clamp(1, MODEL_LIST_VISIBLE) as f32) * MODEL_ROW_HEIGHT + 2.);
        div()
            .id("model-picker-list")
            .w_full()
            .h(height)
            .overflow_hidden()
            .child(
                gpui_kit::uniform_list(
                    "model-picker-rows",
                    rows.len(),
                    move |range, _window, cx| {
                        let theme = cx.theme();
                        range
                            .map(|index| {
                                model_row(
                                    ModelRowSpec {
                                        choice: &list_rows[index],
                                        provider: &provider,
                                        id_prefix: &row_id_prefix,
                                        target: &target,
                                        preferred: index == preferred,
                                        selected: selected.as_deref()
                                            == Some(list_rows[index].id.as_str()),
                                    },
                                    &weak,
                                    theme,
                                )
                            })
                            .collect()
                    },
                )
                .track_scroll(&scroll)
                .h_full(),
            )
            .into_any_element()
    };
    div()
        .id("model-picker-model-step")
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .id("model-picker-back")
                .h(px(32.))
                .px_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .rounded(skin::radius_control())
                .cursor_pointer()
                .hover(|this| this.bg(skin::frost_hover(theme)))
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_browse_model_provider(None, cx);
                }))
                .child(
                    Icon::new(IconName::ArrowLeft)
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .child(div().text_sm().child(t("Providers", "服务商")))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(provider_name),
                ),
        )
        .child(div().h(px(32.)).text_sm().child(Input::new(input)))
        .child(list)
        .into_any_element()
}

/// Everything one model row needs that varies per row.
struct ModelRowSpec<'a> {
    choice: &'a ModelChoice,
    provider: &'a str,
    id_prefix: &'a str,
    target: &'a ModelPickerTarget,
    preferred: bool,
    selected: bool,
}

fn model_row(
    spec: ModelRowSpec<'_>,
    weak: &gpui_kit::WeakEntity<Workspace>,
    theme: &gpui_kit::component::theme::Theme,
) -> AnyElement {
    let ModelRowSpec {
        choice,
        provider,
        id_prefix,
        target,
        preferred,
        selected,
    } = spec;
    let model_id = choice.id.clone();
    let provider_id = provider.to_owned();
    let target = target.clone();
    let weak_select = weak.clone();
    div()
        .id(format!("{id_prefix}-row-{model_id}"))
        .h(px(MODEL_ROW_HEIGHT))
        .w_full()
        .px_1()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .rounded(skin::radius_control())
        .when(preferred, |this| this.bg(skin::frost(theme)))
        .child(
            div()
                .id(format!("{id_prefix}-{model_id}"))
                .min_w_0()
                .flex_1()
                .h_full()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_1()
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    let _ = weak_select.update(cx, |workspace, cx| {
                        select_model(workspace, &target, &provider_id, &model_id, cx);
                    });
                })
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_sm()
                        .child(choice.name.clone()),
                )
                .when(selected, |this| {
                    this.child(
                        Icon::new(IconName::Check)
                            .xsmall()
                            .flex_shrink_0()
                            .text_color(theme.foreground),
                    )
                }),
        )
        .into_any_element()
}

pub(crate) fn select_model(
    workspace: &mut Workspace,
    target: &ModelPickerTarget,
    provider: &str,
    model: &str,
    cx: &mut Context<Workspace>,
) {
    match target {
        ModelPickerTarget::Session => workspace.on_select_model_on(provider, model, cx),
        ModelPickerTarget::Role(role) => {
            workspace.on_set_subagent_route(
                role,
                Some(provider.to_owned()),
                Some(model.to_owned()),
                cx,
            );
            workspace.apply_action(FrontendAction::SubagentMenuToggled(None), cx);
        }
    }
}

fn pin_row(
    workspace: &Workspace,
    pin: &mycode_config::ModelPin,
    target: &ModelPickerTarget,
    highlighted: bool,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    let provider = pin.provider.clone();
    let model = pin.model.clone();
    let name = model_display_name(workspace.vm(), &provider, &model);
    let provider_name = provider_display_name(workspace.vm(), &provider);
    let target = target.clone();
    div()
        .id(format!("model-pin-{provider}-{model}"))
        .when(highlighted, |this| this.bg(skin::frost(theme)))
        // Two lines (model name and provider). A 32px catalog row clips the
        // second line into the next "Recent" row.
        .min_h(px(44.))
        .h_auto()
        .py(px(4.))
        .px_1()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .rounded(skin::radius_control())
        .overflow_hidden()
        .child(
            div()
                .id(format!("model-pin-open-{provider}-{model}"))
                .min_w_0()
                .flex_1()
                .h_auto()
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(1.))
                .px_1()
                .cursor_pointer()
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    select_model(workspace, &target, &provider, &model, cx);
                }))
                .child(div().text_sm().truncate().child(name))
                .child(
                    div()
                        .text_xs()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(provider_name),
                ),
        )
        .into_any_element()
}

fn session_model_row(role: &str, highlighted: bool, cx: &Context<Workspace>) -> AnyElement {
    let theme = cx.theme();
    let role = role.to_owned();
    div()
        .id(format!("agent-model-{role}-inherit-inherit"))
        .when(highlighted, |this| this.bg(skin::frost(theme)))
        .h(px(36.))
        .px_2()
        .flex()
        .flex_row()
        .items_center()
        .rounded(skin::radius_control())
        .cursor_pointer()
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.on_set_subagent_route(&role, Some("inherit".to_owned()), None, cx);
            workspace.apply_action(FrontendAction::SubagentMenuToggled(None), cx);
        }))
        .child(div().text_sm().child(t("Session model", "会话模型")))
        .into_any_element()
}

fn section_label(
    id: &'static str,
    label: &'static str,
    theme: &gpui_kit::component::theme::Theme,
) -> AnyElement {
    div()
        .id(id)
        .px_2()
        .pt_1()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(label)
        .into_any_element()
}
