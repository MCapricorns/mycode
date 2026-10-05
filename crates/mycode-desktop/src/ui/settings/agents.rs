//! The Agents settings page: delegation capacity and one card each for
//! Scout and Artisan. User-added role files stay out of this page.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::Button;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::widgets::{choice_chips, settings_card};
use crate::i18n::t;
use crate::ui::model_picker::{ModelPickerTarget, model_display_name, render_model_picker};
use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

/// The settings copy hard-codes this default. Keep it aligned with config.
const _: () = assert!(mycode_config::DEFAULT_SUBAGENT_CONCURRENCY == 4);

pub(super) fn render_agents_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let Some(settings) = workspace.vm().settings.clone() else {
        return div().into_any_element();
    };
    let max_concurrent = settings.subagents.max_concurrent;
    let theme = cx.theme();
    let mut cards: Vec<AnyElement> = Vec::new();
    cards.push(
        settings_card(
            "agents-capacity",
            t("Delegation", "任务委派"),
            Some(t(
                "The parent model may hand work to Scout and Artisan. A limit of 0 uses the \
                 default of 4 concurrent sub-agents. It does not mean zero agents, and it is \
                 not a count of role types.",
                "主模型可以把工作交给 Scout 和 Artisan。并发数为 0 时使用默认的 4 个同时运行的子代理，\
                 不是零个，也不是角色种类的数量。",
            )),
            theme,
            vec![super::widgets::settings_row(
                "agents-concurrent",
                t("Max concurrent", "最大并发"),
                Some(t(
                    "0 = default (4), not zero agents",
                    "0 = 默认 (4)，不是零个",
                )),
                Button::new("agents-concurrent-cycle")
                    .label(if max_concurrent == 0 {
                        t("default (4)", "默认 (4)").to_owned()
                    } else {
                        max_concurrent.to_string()
                    })
                    .small()
                    .outline()
                    .on_click(cx.listener(move |workspace, _, _, cx| {
                        let Some(settings) = workspace.vm().settings.clone() else {
                            return;
                        };
                        let mut next = settings.subagents;
                        next.max_concurrent = next_concurrency(next.max_concurrent);
                        workspace.apply_action(DesktopAction::SettingsSubagentsChanged(next), cx);
                    }))
                    .into_any_element(),
            )],
        )
        .into_any_element(),
    );
    for role in mycode_config::builtin_roles().roles {
        cards.push(role_card(workspace, window, &role, cx));
    }
    div()
        .id("agents-section")
        .flex()
        .flex_col()
        .gap_3()
        .children(cards)
        .into_any_element()
}

fn next_concurrency(current: u32) -> u32 {
    if current >= mycode_config::MAX_SUBAGENT_CONCURRENCY {
        0
    } else {
        current + 1
    }
}

fn role_card(
    workspace: &mut Workspace,
    window: &mut Window,
    role: &mycode_config::SubagentRole,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let Some(settings) = workspace.vm().settings.clone() else {
        return div().into_any_element();
    };
    let entry = settings.subagents.role(&role.name);
    let enabled = entry.is_none_or(|item| item.enabled);
    let thinking = entry
        .and_then(|item| item.thinking.clone())
        .unwrap_or_else(|| "inherit".to_owned());
    let provider = entry.and_then(|item| item.provider.clone());
    let model = entry.and_then(|item| item.model.clone());
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
                .child(capability_chip(role.isolation.as_str(), theme))
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
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t("Model", "模型")),
                )
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
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t("Thinking", "思考")),
                )
                .child(choice_chips(
                    &format!("agent-think-{name}"),
                    &thinking_options,
                    &thinking,
                    {
                        let role = name.clone();
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

fn capability_chip(isolation: &str, theme: &gpui_kit::component::theme::Theme) -> AnyElement {
    let label = if isolation == "worktree" {
        t("Worktree", "工作树")
    } else {
        t("Read-only", "只读")
    };
    div()
        .h(px(24.))
        .px_2()
        .flex()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(label)
        .into_any_element()
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
    use super::next_concurrency;

    #[test]
    fn settings_roles_are_scout_and_artisan() {
        let roles = mycode_config::builtin_roles();
        let names: Vec<_> = roles.roles.iter().map(|role| role.name.as_str()).collect();
        assert_eq!(names, ["scout", "artisan"]);
    }

    #[test]
    fn concurrency_cycle_wraps_to_the_default_sentinel() {
        assert_eq!(next_concurrency(0), 1);
        assert_eq!(next_concurrency(4), 5);
        assert_eq!(next_concurrency(mycode_config::MAX_SUBAGENT_CONCURRENCY), 0);
        assert_eq!(mycode_config::DEFAULT_SUBAGENT_CONCURRENCY, 4);
    }
}
