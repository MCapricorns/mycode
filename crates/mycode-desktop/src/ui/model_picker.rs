//! Two-step model picker shared by the composer, the default-model control,
//! and each subagent role.
//!
//! The provider step is a short card list. The model step is a search box
//! plus a virtualized list of that provider's catalog and configured ids.
//! The catalog is directory data: the list is filtered, never truncated.
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
use crate::ui::skin::{self, popover_panel};
use crate::view_model::{DesktopAction, WorkspaceState};
use crate::workspace::Workspace;

/// One model the picker can show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelChoice {
    /// Model id sent to the provider.
    pub id: String,
    /// Catalog display name, or the id when the catalog has none.
    pub name: String,
}

/// Who a pick is for. Session picks change the open chat; role picks only
/// change that role's route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModelPickerTarget {
    /// Composer and the settings default model.
    Session,
    /// One subagent role. The provider step includes a session-model row.
    Role(String),
}

/// Fixed height of one virtualized model row.
const MODEL_ROW_HEIGHT: f32 = 32.;
/// How many model rows the panel shows before the list scrolls.
const MODEL_LIST_VISIBLE: usize = 8;

/// Catalog models first, then configured ids the catalog does not list.
///
/// Nothing is dropped for length. Selecting an unlisted id still goes
/// through the reducer, which enforces the provider row cap.
pub(crate) fn merge_model_choices(
    catalog: &[(String, String)],
    configured: &[String],
) -> Vec<ModelChoice> {
    let mut choices = Vec::with_capacity(catalog.len() + configured.len());
    for (id, name) in catalog {
        let name = if name.is_empty() {
            id.clone()
        } else {
            name.clone()
        };
        choices.push(ModelChoice {
            id: id.clone(),
            name,
        });
    }
    for id in configured {
        if choices.iter().any(|choice| choice.id == *id) {
            continue;
        }
        choices.push(ModelChoice {
            id: id.clone(),
            name: id.clone(),
        });
    }
    choices
}

/// Case-insensitive filter on id and display name. An empty query keeps
/// every choice.
pub(crate) fn filter_model_choices(choices: &[ModelChoice], query: &str) -> Vec<ModelChoice> {
    let query = query.trim();
    if query.is_empty() {
        return choices.to_vec();
    }
    let needle = query.to_lowercase();
    choices
        .iter()
        .filter(|choice| {
            choice.id.to_lowercase().contains(&needle)
                || choice.name.to_lowercase().contains(&needle)
        })
        .cloned()
        .collect()
}

/// Starred pins, then recent pins that are not already starred.
///
/// Providers that are not enabled are omitted. The caller passes the enabled
/// provider ids.
pub(crate) fn visible_pins<'a>(
    starred: &'a [mycode_config::ModelPin],
    recent: &'a [mycode_config::ModelPin],
    enabled: &[String],
) -> (
    Vec<&'a mycode_config::ModelPin>,
    Vec<&'a mycode_config::ModelPin>,
) {
    let enabled = |pin: &mycode_config::ModelPin| enabled.iter().any(|id| id == &pin.provider);
    let starred: Vec<_> = starred.iter().filter(|pin| enabled(pin)).collect();
    let recent: Vec<_> = recent
        .iter()
        .filter(|pin| {
            enabled(pin)
                && !starred
                    .iter()
                    .any(|star| star.provider == pin.provider && star.model == pin.model)
        })
        .collect();
    (starred, recent)
}

/// Row Enter should activate.
///
/// A non-empty query selects the first match. An empty query selects the
/// current model when it is still in the list, otherwise the first row.
pub(crate) fn preferred_index(ids: &[String], query: &str, selected: Option<&str>) -> usize {
    if ids.is_empty() {
        return 0;
    }
    if !query.trim().is_empty() {
        return 0;
    }
    selected
        .and_then(|id| ids.iter().position(|item| item == id))
        .unwrap_or(0)
}

/// Catalog display name for one model, or the id when the catalog has none.
pub(crate) fn model_display_name(vm: &WorkspaceState, provider: &str, model_id: &str) -> String {
    vm.catalog
        .as_ref()
        .and_then(|catalog| catalog.model(provider, model_id))
        .map(|model| {
            if model.name.is_empty() {
                model.id.clone()
            } else {
                model.name.clone()
            }
        })
        .unwrap_or_else(|| model_id.to_owned())
}

/// Composer and default-model label: the model name only.
pub(crate) fn selected_model_label(vm: &WorkspaceState) -> String {
    match (
        vm.selected_provider.as_deref(),
        vm.selected_model.as_deref(),
    ) {
        (Some(provider), Some(model)) => model_display_name(vm, provider, model),
        (_, Some(model)) => model.to_owned(),
        _ => t("Select model", "选择模型").to_owned(),
    }
}

/// Every model the picker shows for one configured provider.
pub(crate) fn provider_model_choices(vm: &WorkspaceState, provider_id: &str) -> Vec<ModelChoice> {
    let catalog = vm
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.provider(provider_id))
        .map(|provider| {
            provider
                .models
                .iter()
                .map(|model| (model.id.clone(), model.name.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let configured = vm
        .settings
        .as_ref()
        .and_then(|settings| {
            settings
                .providers
                .iter()
                .find(|provider| provider.id == provider_id)
        })
        .map(|provider| provider.models.clone())
        .unwrap_or_default();
    merge_model_choices(&catalog, &configured)
}

/// The picker panel. Caller decides when it is mounted.
pub(crate) fn render_model_picker(
    workspace: &mut Workspace,
    window: &mut Window,
    target: ModelPickerTarget,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let browse = workspace.vm().model_menu_browse.clone();
    let theme = cx.theme().clone();
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
    popover_panel("model-picker", &theme)
        .w(px(340.))
        .flex_none()
        .p_2()
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
    let theme = cx.theme().clone();
    let enabled = enabled_providers(workspace.vm());
    let (starred, recent) = visible_pins(
        &workspace.vm().starred_models,
        &workspace.vm().recent_models,
        &enabled.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
    );
    let starred: Vec<_> = starred.into_iter().cloned().collect();
    let recent: Vec<_> = recent.into_iter().cloned().collect();
    let mut rows: Vec<AnyElement> = Vec::new();
    if let ModelPickerTarget::Role(role) = target {
        rows.push(session_model_row(role, cx));
    }
    if !starred.is_empty() {
        rows.push(section_label(
            "model-picker-starred",
            t("Starred", "星标"),
            &theme,
        ));
        rows.extend(
            starred
                .iter()
                .map(|pin| pin_row(workspace, pin, target, cx)),
        );
    }
    if !recent.is_empty() {
        rows.push(section_label(
            "model-picker-recent",
            t("Recent", "最近"),
            &theme,
        ));
        rows.extend(recent.iter().map(|pin| pin_row(workspace, pin, target, cx)));
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
                    "No enabled providers. Add one in Settings.",
                    "没有启用的服务商。请先在设置中添加。",
                ))
                .into_any_element(),
        );
    } else {
        rows.push(section_label(
            "model-picker-providers",
            t("Providers", "服务商"),
            &theme,
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
                    .hover(|this| this.bg(skin::frost_hover(&theme)))
                    .when(selected, |this| this.bg(skin::frost_accent(&theme)))
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
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .xsmall()
                            .text_color(theme.muted_foreground),
                    )
                    .into_any_element(),
            );
        }
    }
    div()
        .id("model-picker-provider-step")
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
    let theme = cx.theme().clone();
    let provider_name = provider_display_name(workspace.vm(), provider_id);
    let choices = provider_model_choices(workspace.vm(), provider_id);
    let filtered = filter_model_choices(&choices, query);
    let selected = selected_in_provider(workspace.vm(), target, provider_id);
    let ids: Vec<String> = filtered.iter().map(|choice| choice.id.clone()).collect();
    let preferred = preferred_index(&ids, query, selected.as_deref());
    let starred = workspace.vm().starred_models.clone();
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
                        let theme = cx.theme().clone();
                        range
                            .map(|index| {
                                model_row(
                                    &list_rows[index],
                                    &provider,
                                    &row_id_prefix,
                                    &target,
                                    index == preferred,
                                    selected.as_deref() == Some(list_rows[index].id.as_str()),
                                    starred.iter().any(|pin| {
                                        pin.provider == provider && pin.model == list_rows[index].id
                                    }),
                                    &weak,
                                    &theme,
                                )
                            })
                            .collect()
                    },
                )
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
                .hover(|this| this.bg(skin::frost_hover(&theme)))
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

#[allow(clippy::too_many_arguments)]
fn model_row(
    choice: &ModelChoice,
    provider: &str,
    id_prefix: &str,
    target: &ModelPickerTarget,
    preferred: bool,
    selected: bool,
    starred: bool,
    weak: &gpui_kit::WeakEntity<Workspace>,
    theme: &gpui_kit::component::theme::Theme,
) -> AnyElement {
    let model_id = choice.id.clone();
    let provider_id = provider.to_owned();
    let target = target.clone();
    let weak_select = weak.clone();
    let weak_star = weak.clone();
    let star_model = model_id.clone();
    let star_provider = provider_id.clone();
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
        .when(preferred, |this| this.bg(skin::frost_accent(theme)))
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
                    let model_id = model_id.clone();
                    let provider_id = provider_id.clone();
                    let target = target.clone();
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
                            .text_color(theme.primary),
                    )
                }),
        )
        .child(
            div()
                .id(format!("model-star-{provider}-{id}", id = choice.id))
                .size(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .flex_shrink_0()
                .rounded(skin::radius_control())
                .cursor_pointer()
                .text_color(if starred {
                    theme.primary
                } else {
                    theme.muted_foreground
                })
                .hover(|this| this.bg(skin::frost_hover(theme)))
                .on_click(move |_, _, cx| {
                    let _ = weak_star.update(cx, |workspace, cx| {
                        workspace.on_toggle_model_star(&star_provider, &star_model, cx);
                    });
                })
                .child(
                    Icon::new(if starred {
                        IconName::StarFill
                    } else {
                        IconName::Star
                    })
                    .xsmall(),
                ),
        )
        .into_any_element()
}

fn select_model(
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
            workspace.apply_action(DesktopAction::SubagentMenuToggled(None), cx);
        }
    }
}

fn pin_row(
    workspace: &Workspace,
    pin: &mycode_config::ModelPin,
    target: &ModelPickerTarget,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    let provider = pin.provider.clone();
    let model = pin.model.clone();
    let name = model_display_name(workspace.vm(), &provider, &model);
    let provider_name = provider_display_name(workspace.vm(), &provider);
    let target = target.clone();
    let starred = workspace
        .vm()
        .starred_models
        .iter()
        .any(|item| item.provider == provider && item.model == model);
    div()
        .id(format!("model-pin-{provider}-{model}"))
        .h(px(MODEL_ROW_HEIGHT))
        .px_1()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .rounded(skin::radius_control())
        .child(
            div()
                .id(format!("model-pin-open-{provider}-{model}"))
                .min_w_0()
                .flex_1()
                .h_full()
                .flex()
                .flex_col()
                .justify_center()
                .px_1()
                .cursor_pointer()
                .on_click({
                    let provider = provider.clone();
                    let model = model.clone();
                    let target = target.clone();
                    cx.listener(move |workspace, _, _, cx| {
                        select_model(workspace, &target, &provider, &model, cx);
                    })
                })
                .child(div().text_sm().truncate().child(name))
                .child(
                    div()
                        .text_xs()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(provider_name),
                ),
        )
        .child(
            div()
                .id(format!("model-pin-star-{provider}-{model}"))
                .size(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_color(if starred {
                    theme.primary
                } else {
                    theme.muted_foreground
                })
                .on_click({
                    let provider = provider.clone();
                    let model = model.clone();
                    cx.listener(move |workspace, _, _, cx| {
                        workspace.on_toggle_model_star(&provider, &model, cx);
                    })
                })
                .child(
                    Icon::new(if starred {
                        IconName::StarFill
                    } else {
                        IconName::Star
                    })
                    .xsmall(),
                ),
        )
        .into_any_element()
}

fn session_model_row(role: &str, cx: &Context<Workspace>) -> AnyElement {
    let theme = cx.theme();
    let role = role.to_owned();
    div()
        .id(format!("agent-model-{role}-inherit-inherit"))
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
            workspace.apply_action(DesktopAction::SubagentMenuToggled(None), cx);
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

fn enabled_providers(vm: &WorkspaceState) -> Vec<(String, String)> {
    vm.settings
        .as_ref()
        .map(|settings| {
            settings
                .providers
                .iter()
                .filter(|provider| provider.enabled)
                .map(|provider| {
                    let name = provider_display_name(vm, &provider.id);
                    (provider.id.clone(), name)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn provider_display_name(vm: &WorkspaceState, id: &str) -> String {
    vm.catalog
        .as_ref()
        .map(|catalog| catalog.display_name(id))
        .unwrap_or_else(|| id.to_owned())
}

fn current_provider(vm: &WorkspaceState, target: &ModelPickerTarget) -> Option<String> {
    match target {
        ModelPickerTarget::Session => vm.selected_provider.clone(),
        ModelPickerTarget::Role(role) => vm
            .settings
            .as_ref()
            .and_then(|settings| settings.subagents.role(role))
            .and_then(|entry| entry.provider.clone()),
    }
}

fn selected_in_provider(
    vm: &WorkspaceState,
    target: &ModelPickerTarget,
    provider_id: &str,
) -> Option<String> {
    match target {
        ModelPickerTarget::Session => vm
            .selected_provider
            .as_deref()
            .filter(|provider| *provider == provider_id)
            .and_then(|_| vm.selected_model.clone()),
        ModelPickerTarget::Role(role) => vm
            .settings
            .as_ref()
            .and_then(|settings| settings.subagents.role(role))
            .filter(|entry| entry.provider.as_deref() == Some(provider_id))
            .and_then(|entry| entry.model.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ModelChoice, filter_model_choices, merge_model_choices, preferred_index, visible_pins,
    };
    use mycode_config::ModelPin;

    #[test]
    fn filter_keeps_the_whole_catalog_and_matches_name() {
        let choices: Vec<ModelChoice> = (0..300)
            .map(|index| ModelChoice {
                id: format!("m{index}"),
                name: if index == 42 {
                    "Flagship".to_owned()
                } else {
                    format!("Model {index}")
                },
            })
            .collect();
        assert_eq!(filter_model_choices(&choices, "").len(), 300);
        assert_eq!(filter_model_choices(&choices, "   ").len(), 300);
        let matched = filter_model_choices(&choices, "flag");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "m42");
        assert_eq!(filter_model_choices(&choices, "m299").len(), 1);
    }

    #[test]
    fn merge_appends_configured_ids_without_a_cap() {
        let catalog: Vec<(String, String)> = (0..300)
            .map(|index| (format!("m{index}"), format!("Name {index}")))
            .collect();
        let configured = vec!["m0".to_owned(), "custom-only".to_owned()];
        let merged = merge_model_choices(&catalog, &configured);
        assert_eq!(merged.len(), 301);
        assert_eq!(merged[0].name, "Name 0");
        assert_eq!(
            merged.last().map(|choice| choice.id.as_str()),
            Some("custom-only")
        );
    }

    #[test]
    fn pins_drop_disabled_providers_and_skip_starred_recents() {
        let starred = vec![ModelPin::new("openai", "gpt")];
        let recent = vec![
            ModelPin::new("openai", "gpt"),
            ModelPin::new("openai", "o3"),
            ModelPin::new("off", "hidden"),
        ];
        let enabled = vec!["openai".to_owned()];
        let (starred, recent) = visible_pins(&starred, &recent, &enabled);
        assert_eq!(starred.len(), 1);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].model, "o3");
    }

    #[test]
    fn preferred_row_follows_the_query() {
        let ids = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(preferred_index(&ids, "", Some("c")), 2);
        assert_eq!(preferred_index(&ids, "", Some("missing")), 0);
        assert_eq!(preferred_index(&ids, "bee", Some("c")), 0);
        assert_eq!(preferred_index(&[], "", None), 0);
    }
}
