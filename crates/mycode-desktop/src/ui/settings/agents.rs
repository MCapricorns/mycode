//! The Agents settings page: delegation capacity and one card per subagent
//! role. Model and thinking are one picker, not three dropdowns.
use gpui_kit::component::button::Button;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

use super::widgets::{row_header, settings_card, settings_row};
use crate::i18n::t;
use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

pub(super) fn render_agents_section(workspace: &Workspace, cx: &Context<Workspace>) -> AnyElement {
    let Some(settings) = workspace.vm().settings.clone() else {
        return div().into_any_element();
    };
    let catalog = mycode_config::builtin_roles();
    let providers: Vec<(String, Vec<String>)> = settings
        .providers
        .iter()
        .filter(|provider| provider.enabled)
        .map(|provider| (provider.id.clone(), provider.models.clone()))
        .collect();
    let max_concurrent = settings.subagents.max_concurrent;
    let role_cards: Vec<AnyElement> = catalog
        .roles
        .iter()
        .map(|role| {
            let entry = settings.subagents.role(&role.name);
            let enabled = entry.is_none_or(|item| item.enabled);
            let thinking = entry
                .and_then(|item| item.thinking.clone())
                .unwrap_or_else(|| "inherit".to_owned());
            let provider = entry
                .and_then(|item| item.provider.clone())
                .unwrap_or_else(|| "inherit".to_owned());
            let model = entry
                .and_then(|item| item.model.clone())
                .unwrap_or_else(|| "inherit".to_owned());
            agent_role_card(
                workspace,
                role.name.clone(),
                role.description.clone(),
                role.isolation.as_str(),
                role.origin.as_str(),
                enabled,
                thinking,
                provider,
                model,
                providers.clone(),
                cx,
            )
        })
        .collect();
    let theme = cx.theme();
    div()
        .id("agents-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_card(
            "agents-capacity",
            t("Delegation", "任务委派"),
            Some(t(
                "The parent model may hand work to these roles. Inherit uses the session \
                 provider and the role's own thinking level. 0 concurrent slots means \
                 automatic capacity.",
                "主模型可以把工作交给这些角色。继承 表示沿用会话的服务商与角色自身的思考档位。\
                 并发数为 0 表示自动分配。",
            )),
            theme,
            vec![settings_row(
                "agents-concurrent",
                t("Max concurrent", "最大并发"),
                Some(t("0 = automatic", "0 = 自动")),
                Button::new("agents-concurrent-cycle")
                    .label(if max_concurrent == 0 {
                        t("auto", "自动").to_owned()
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
                        next.max_concurrent =
                            if next.max_concurrent >= mycode_config::MAX_SUBAGENT_CONCURRENCY {
                                0
                            } else {
                                next.max_concurrent + 1
                            };
                        workspace.apply_action(DesktopAction::SettingsSubagentsChanged(next), cx);
                    }))
                    .into_any_element(),
            )],
        ))
        .child(settings_card(
            "agents-roles",
            t("Roles", "角色"),
            Some(t(
                "Scout returns a read-only map. Artisan implements in a worktree; you integrate.",
                "Scout 返回只读地图。Artisan 在 worktree 中实现，由你整合。",
            )),
            theme,
            role_cards,
        ))
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn agent_role_card(
    workspace: &Workspace,
    name: String,
    description: String,
    isolation: &'static str,
    origin: &'static str,
    enabled: bool,
    thinking: String,
    provider: String,
    model: String,
    providers: Vec<(String, Vec<String>)>,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme: &Theme = cx.theme();
    let role = name.clone();
    let thinking_label = thinking.clone();
    let provider_label = provider.clone();
    let model_label = model.clone();
    let thinking_options = {
        let mut levels = vec!["inherit".to_owned()];
        levels.extend(crate::view_model::reasoning_levels_for(
            workspace.vm(),
            (provider != "inherit").then_some(provider.as_str()),
            (model != "inherit").then_some(model.as_str()),
        ));
        levels
    };
    let open_field = workspace
        .vm()
        .subagent_menu
        .as_ref()
        .filter(|(open_role, _)| open_role == &name)
        .map(|(_, field)| field.as_str());
    div()
        .id(format!("agent-role-{name}"))
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(px(3.))
        .border_1()
        .border_color(theme.border)
        .child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap_3()
                .child(
                    row_header(&name, description).child(
                        div()
                            .text_xs()
                            .opacity(0.45)
                            .child(format!("{origin} \u{b7} {isolation}")),
                    ),
                )
                .child(
                    Switch::new(format!("agent-enabled-{name}"))
                        .checked(enabled)
                        .on_click({
                            let role = role.clone();
                            cx.listener(move |workspace, checked: &bool, _, cx| {
                                workspace.on_subagent_role_enabled(&role, *checked, cx);
                            })
                        }),
                ),
        )
        .child(agent_route_picker(
            &role,
            &format!("{provider_label} \u{b7} {model_label} \u{b7} {thinking_label}"),
            &thinking_options,
            &thinking_label,
            &providers,
            open_field == Some("route"),
            cx,
        ))
        .into_any_element()
}

/// One route control: thinking chips and model rows in the same panel.
fn agent_route_picker(
    role: &str,
    current: &str,
    thinking: &[String],
    thinking_current: &str,
    providers: &[(String, Vec<String>)],
    open: bool,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    let toggle_role = role.to_owned();
    div()
        .id(format!("agent-route-{role}"))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .id(format!("agent-route-toggle-{role}"))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_2()
                .h(px(28.))
                .rounded(px(3.))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|this| this.bg(theme.secondary))
                .on_click({
                    let role = toggle_role.clone();
                    cx.listener(move |workspace, _, _, cx| {
                        workspace.on_toggle_subagent_menu(&role, "route", !open, cx);
                    })
                })
                .child(div().text_sm().truncate().child(current.to_owned()))
                .child(div().text_xs().opacity(0.5).child(if open {
                    "\u{25b4}"
                } else {
                    "\u{25be}"
                })),
        )
        .when(open, |this| {
            let role = role.to_owned();
            this.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded(px(3.))
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .px_2()
                            .pt_1()
                            .child(t("Thinking", "思考")),
                    )
                    .children(thinking.iter().map(|level| {
                        let picked = level.clone();
                        let role = role.clone();
                        let on = level == thinking_current;
                        div()
                            .id(format!("agent-think-{role}-{level}"))
                            .h(px(28.))
                            .px_2()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .rounded(px(6.))
                            .text_sm()
                            .cursor_pointer()
                            .when(on, |row| row.bg(theme.accent))
                            .hover(|row| row.bg(theme.secondary_hover))
                            .on_click(cx.listener(move |workspace, _, _, cx| {
                                workspace.on_set_subagent_thinking(&role, Some(picked.clone()), cx);
                            }))
                            .child(level.clone())
                            .when(on, |row| {
                                row.child(
                                    div().text_xs().text_color(theme.primary).child("\u{2713}"),
                                )
                            })
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .px_2()
                            .pt_2()
                            .child(t("Model", "模型")),
                    )
                    .child(agent_model_row(&role, "inherit", None, cx))
                    .children(providers.iter().flat_map(|(provider, models)| {
                        let mut rows = vec![
                            div()
                                .text_xs()
                                .opacity(0.5)
                                .pt_1()
                                .child(provider.clone())
                                .into_any_element(),
                        ];
                        rows.extend(models.iter().map(|model| {
                            agent_model_row(&role, model, Some(provider.as_str()), cx)
                        }));
                        rows
                    })),
            )
        })
        .into_any_element()
}

fn agent_model_row(
    role: &str,
    model: &str,
    provider: Option<&str>,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    let role = role.to_owned();
    let model = model.to_owned();
    let provider = provider.map(str::to_owned);
    div()
        .id(format!(
            "agent-model-{role}-{}-{model}",
            provider.as_deref().unwrap_or("inherit")
        ))
        .h(px(26.))
        .flex()
        .items_center()
        .px_2()
        .rounded(px(3.))
        .text_sm()
        .cursor_pointer()
        .hover(|this| this.bg(theme.secondary))
        .on_click({
            let model = model.clone();
            cx.listener(move |workspace, _, _, cx| {
                if model == "inherit" {
                    workspace.on_set_subagent_route(&role, Some("inherit".to_owned()), None, cx);
                } else {
                    workspace.on_set_subagent_route(
                        &role,
                        provider.clone(),
                        Some(model.clone()),
                        cx,
                    );
                }
            })
        })
        .child(model)
        .into_any_element()
}
