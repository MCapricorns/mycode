//! The Agents settings page: Scout and Artisan, then any further roles
//! `discover_roles` found. Concurrency is fixed and subagents have no
//! wall-clock limit, so neither is a setting. Stored steward and sentinel
//! entries stay on disk and are not product defaults; they appear only when
//! a role file actually defines them.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::widgets::{choice_chips, field_label, settings_card};
use crate::i18n::t;
use crate::ui::model_picker::{ModelPickerTarget, model_display_name, render_model_picker};
use crate::workspace::Workspace;

pub(super) fn render_agents_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    if workspace.vm().settings.is_none() {
        return div().into_any_element();
    }
    let theme = cx.theme().clone();
    let catalog = workspace.agent_roles();
    let (primary, additional) = group_settings_roles(&catalog);
    let mut cards: Vec<AnyElement> = Vec::new();
    for role in primary {
        cards.push(role_card(workspace, window, role, cx));
    }
    if !additional.is_empty() {
        cards.push(
            div()
                .id("agents-additional")
                .flex()
                .flex_col()
                .gap_1()
                .pt_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(crate::ui::skin::WEIGHT_STRONG)
                        .child(t("Additional roles", "其他角色")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .whitespace_normal()
                        .child(t(
                            "From your MYCode home agents directory, or this project's .mycode/agents. \
                             Enable, model, and thinking work the same way.",
                            "来自 MYCode 主目录下的 agents，或当前项目的 .mycode/agents。启用、模型和思考与上面相同。",
                        )),
                )
                .into_any_element(),
        );
        for role in additional {
            cards.push(role_card(workspace, window, role, cx));
        }
    }
    div()
        .id("agents-section")
        .flex()
        .flex_col()
        .gap_3()
        .children(cards)
        .into_any_element()
}

fn role_card(
    workspace: &mut Workspace,
    window: &mut Window,
    role: &mycode_config::SubagentRole,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let (enabled, thinking, provider, model) = {
        let Some(settings) = workspace.vm().settings.as_ref() else {
            return div().into_any_element();
        };
        let entry = settings.subagents.role(&role.name);
        (
            entry.is_none_or(|item| item.enabled),
            entry
                .and_then(|item| item.thinking.clone())
                .unwrap_or_else(|| "inherit".to_owned()),
            entry.and_then(|item| item.provider.clone()),
            entry.and_then(|item| item.model.clone()),
        )
    };
    let open = workspace
        .vm()
        .subagent_menu
        .as_ref()
        .is_some_and(|(open_role, field)| open_role == &role.name && field == "model");
    let picker = open.then(|| {
        render_model_picker(
            workspace,
            window,
            ModelPickerTarget::Role(role.name.clone()),
            px(420.),
            cx,
        )
    });
    let route_label = match (provider.as_deref(), model.as_deref()) {
        (Some(provider), Some(model)) => model_display_name(workspace.vm(), provider, model),
        _ => t("Session model", "会话模型").to_owned(),
    };
    let thinking_options = thinking_options(workspace, provider.as_deref(), model.as_deref());
    let theme = cx.theme();
    let name = role.name.clone();
    let title = title_case(&role.name);
    settings_card(
        &format!("agent-{name}"),
        &title,
        Some(&role.description),
        theme,
        vec![
            div()
                .id(format!("agent-role-{name}"))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(field_label(t("Enabled", "启用")))
                        .child(capability_chip(role.isolation.as_str(), theme))
                        .when(role.origin != mycode_config::RoleOrigin::Builtin, |this| {
                            this.child(origin_chip(role.origin, theme))
                        }),
                )
                .child(
                    Switch::new(format!("agent-enabled-{name}"))
                        .checked(enabled)
                        .on_click({
                            let role = name.clone();
                            cx.listener(move |workspace, checked: &bool, _, cx| {
                                workspace.on_subagent_role_enabled(&role, *checked, cx);
                            })
                        }),
                )
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(field_label(t("Model", "模型")))
                .child(
                    div()
                        .id(format!("agent-route-toggle-{name}"))
                        .h(px(36.))
                        .px_2()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .rounded(crate::ui::skin::radius_control())
                        .border_1()
                        .border_color(crate::ui::skin::glass_border(theme))
                        .bg(crate::ui::skin::frost(theme))
                        .cursor_pointer()
                        .hover(|this| this.bg(crate::ui::skin::frost_hover(theme)))
                        .on_click({
                            let role = name.clone();
                            cx.listener(move |workspace, _, _, cx| {
                                let open = workspace.vm().subagent_menu.as_ref().is_some_and(
                                    |(open_role, field)| open_role == &role && field == "model",
                                );
                                workspace.on_toggle_subagent_menu(&role, "model", !open, cx);
                            })
                        })
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_sm()
                                .child(route_label),
                        )
                        .child(
                            Icon::new(IconName::ChevronDown)
                                .xsmall()
                                .text_color(theme.muted_foreground),
                        ),
                )
                .when_some(picker, |this, picker| this.child(picker))
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(field_label(t("Thinking", "思考")))
                .child(choice_chips(
                    &format!("agent-think-{name}"),
                    &thinking_options,
                    &thinking,
                    {
                        let role = name;
                        move |workspace, level, cx| {
                            workspace.on_set_subagent_thinking(&role, Some(level.to_owned()), cx);
                        }
                    },
                    cx,
                ))
                .into_any_element(),
        ],
    )
    .into_any_element()
}

fn thinking_options(
    workspace: &Workspace,
    provider: Option<&str>,
    model: Option<&str>,
) -> Vec<(String, String)> {
    let (provider, model) = match (provider, model) {
        (Some(provider), Some(model)) => (Some(provider), Some(model)),
        _ => (
            workspace.vm().selected_provider.as_deref(),
            workspace.vm().selected_model.as_deref(),
        ),
    };
    let mut options = vec![(
        "inherit".to_owned(),
        t("Role default", "角色默认").to_owned(),
    )];
    for level in crate::view_model::reasoning_levels_for(workspace.vm(), provider, model) {
        if level == "default" || options.iter().any(|(id, _)| id == &level) {
            continue;
        }
        options.push((level.clone(), crate::ui::chat::reasoning_row_label(&level)));
    }
    options
}

fn origin_chip(
    origin: mycode_config::RoleOrigin,
    theme: &gpui_kit::component::theme::Theme,
) -> AnyElement {
    let label = match origin {
        mycode_config::RoleOrigin::User => t("User", "用户"),
        mycode_config::RoleOrigin::Project => t("Project", "项目"),
        mycode_config::RoleOrigin::Builtin => t("Built-in", "内置"),
    };
    div()
        .h(px(20.))
        .px_2()
        .flex()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(crate::ui::skin::border_strong(theme))
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(label)
        .into_any_element()
}

fn capability_chip(isolation: &str, theme: &gpui_kit::component::theme::Theme) -> AnyElement {
    let label = if isolation == "worktree" {
        t("Worktree", "工作树")
    } else {
        t("Read-only", "只读")
    };
    div()
        .h(px(20.))
        .px_2()
        .flex()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(crate::ui::skin::border_strong(theme))
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(label)
        .into_any_element()
}

/// Scout and Artisan stay the two leading cards, in that order. Every other
/// discovered role is listed after them. Retired names are not inserted.
fn group_settings_roles(
    catalog: &mycode_config::RoleCatalog,
) -> (
    Vec<&mycode_config::SubagentRole>,
    Vec<&mycode_config::SubagentRole>,
) {
    const PRIMARY: [&str; 2] = ["scout", "artisan"];
    let primary = PRIMARY
        .into_iter()
        .filter_map(|name| catalog.role(name))
        .collect();
    let additional = catalog
        .roles
        .iter()
        .filter(|role| !PRIMARY.contains(&role.name.as_str()))
        .collect();
    (primary, additional)
}

fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn settings_roles_are_scout_and_artisan() {
        let roles = mycode_config::builtin_roles();
        let names: Vec<_> = roles.roles.iter().map(|role| role.name.as_str()).collect();
        assert_eq!(names, ["scout", "artisan"]);
        let (primary, additional) = super::group_settings_roles(&roles);
        assert_eq!(
            primary
                .iter()
                .map(|role| role.name.as_str())
                .collect::<Vec<_>>(),
            ["scout", "artisan"]
        );
        assert!(additional.is_empty());
    }

    #[test]
    fn discovered_roles_follow_the_builtin_cards() {
        let mut catalog = mycode_config::builtin_roles();
        catalog
            .roles
            .push(sample_role("reviewer", mycode_config::RoleOrigin::User));
        catalog
            .roles
            .push(sample_role("steward", mycode_config::RoleOrigin::Project));
        catalog
            .roles
            .sort_by(|left, right| left.name.cmp(&right.name));
        let (primary, additional) = super::group_settings_roles(&catalog);
        assert_eq!(
            primary
                .iter()
                .map(|role| role.name.as_str())
                .collect::<Vec<_>>(),
            ["scout", "artisan"]
        );
        assert_eq!(
            additional
                .iter()
                .map(|role| role.name.as_str())
                .collect::<Vec<_>>(),
            ["reviewer", "steward"]
        );
        assert!(
            !mycode_config::builtin_roles()
                .names()
                .iter()
                .any(|name| name == "steward" || name == "sentinel")
        );
    }

    fn sample_role(name: &str, origin: mycode_config::RoleOrigin) -> mycode_config::SubagentRole {
        mycode_config::SubagentRole {
            name: name.to_owned(),
            description: "role".into(),
            isolation: mycode_config::RoleIsolation::Shared,
            thinking: mycode_config::RoleThinking::Default,
            tools: None,
            prompt: "work".into(),
            origin,
        }
    }
}
