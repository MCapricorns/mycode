//! Compact role settings with inline model and reasoning pickers.
//! Built-in overrides keep their place; custom roles follow the built-ins.
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled,
    TestSupportExt as _, Window, div, px,
};

use super::widgets::choice_chips;
use crate::i18n::t;
use crate::ui::model_picker::{ModelPickerTarget, model_display_name, render_model_picker};
use crate::ui::skin;
use crate::workspace::Workspace;
use mycode::config::{RoleIsolation, RoleOrigin, SubagentRole};

pub(super) fn render_agents_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    if workspace.vm().settings.is_none() {
        return div().into_any_element();
    }
    let catalog = workspace.agent_roles();
    let (primary, additional) = group_settings_roles(&catalog);
    let wide = f32::from(window.viewport_size().width) >= 1200.;
    let mut groups = Vec::new();
    for (id, roles) in [("builtin", primary), ("custom", additional)] {
        if roles.is_empty() {
            continue;
        }
        let rows: Vec<_> = roles
            .into_iter()
            .map(|role| role_row(workspace, window, role, wide, cx))
            .collect();
        let theme = cx.theme();
        groups.push(
            div()
                .w_full()
                .min_w_0()
                .flex_none()
                .flex()
                .flex_col()
                .gap_2()
                .when(id == "custom", |this| {
                    this.child(
                        div()
                            .text_sm()
                            .font_weight(skin::WEIGHT_STRONG)
                            .child(t("Additional roles", "其他角色")),
                    )
                })
                .child(
                    div()
                        .id(format!("agents-{id}"))
                        .w_full()
                        .min_w_0()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .rounded(skin::radius_control())
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .children(rows.into_iter().enumerate().map(|(index, row)| {
                            div()
                                .w_full()
                                .min_w_0()
                                .flex_none()
                                .when(index > 0, |this| {
                                    this.border_t_1().border_color(theme.border)
                                })
                                .child(row)
                        })),
                )
                .into_any_element(),
        );
    }
    div()
        .id("agents-section")
        .w_full()
        .min_w_0()
        .flex_none()
        .flex()
        .flex_col()
        .gap_4()
        .children(groups)
        .into_any_element()
}

fn role_row(
    workspace: &mut Workspace,
    window: &mut Window,
    role: &SubagentRole,
    wide: bool,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let settings = workspace.vm().settings.as_ref().expect("settings loaded");
    let entry = settings.subagents.role(&role.name);
    let enabled = entry.is_none_or(|item| item.enabled);
    let thinking = entry
        .and_then(|item| item.thinking.clone())
        .unwrap_or_else(|| "inherit".into());
    let provider = entry.and_then(|item| item.provider.clone());
    let model = entry.and_then(|item| item.model.clone());
    let open_field = workspace
        .vm()
        .subagent_menu
        .as_ref()
        .filter(|(name, _)| name == &role.name)
        .map(|(_, field)| field.clone());
    let route_label = match (provider.as_deref(), model.as_deref()) {
        (Some(provider), Some(model)) => model_display_name(workspace.vm(), provider, model),
        _ => t("Session model", "会话模型").to_owned(),
    };
    let thinking_label = if thinking == "inherit" {
        match role.thinking.effort() {
            Some(level) => format!("{} · {}", t("Default", "默认"), thinking_value_label(level)),
            None => t("Role default", "角色默认").to_owned(),
        }
    } else {
        thinking_value_label(&thinking)
    };
    let expanded = match open_field.as_deref() {
        Some("model") => Some(render_model_picker(
            workspace,
            window,
            ModelPickerTarget::Role(role.name.clone()),
            px(320.),
            cx,
        )),
        Some("thinking") => {
            let options = thinking_options(workspace, provider.as_deref(), model.as_deref());
            let name = role.name.clone();
            Some(choice_chips(
                &format!("agent-think-{name}"),
                &options,
                &thinking,
                move |workspace, level, cx| {
                    workspace.on_set_subagent_thinking(&name, Some(level.to_owned()), cx);
                    workspace.on_toggle_subagent_menu(&name, "thinking", false, cx);
                },
                cx,
            ))
        }
        _ => None,
    };
    let theme = cx.theme();
    let field = |kind: &'static str, label: &str| {
        let name = role.name.clone();
        let open = open_field.as_deref() == Some(kind);
        role_field_button(&name, kind, label, open, theme)
            .on_click(cx.listener(move |workspace, _, _, cx| {
                workspace.on_toggle_subagent_menu(&name, kind, !open, cx);
            }))
            .into_any_element()
    };
    let controls = RoleControls {
        model: field("model", &route_label),
        thinking: field("thinking", &thinking_label),
        toggle: Switch::new(format!("agent-enabled-{}", role.name))
            .checked(enabled)
            .accessibility_label(format!("{} {}", t("Enable", "启用"), role.name))
            .on_click({
                let name = role.name.clone();
                cx.listener(move |workspace, checked: &bool, _, cx| {
                    workspace.on_subagent_role_enabled(&name, *checked, cx);
                })
            })
            .into_any_element(),
    };
    role_settings_row(role, controls, expanded, wide, theme)
}

fn role_field_button(name: &str, field: &str, label: &str, open: bool, theme: &Theme) -> Button {
    let field_label = if field == "model" {
        t("Model", "模型")
    } else {
        t("Thinking", "思考")
    };
    let tooltip = gpui_kit::SharedString::from(format!("{field_label}: {label}"));
    Button::new(format!("agent-{field}-toggle-{name}"))
        .outline()
        .small()
        .w_full()
        .min_w_0()
        .h_auto()
        .min_h(px(32.))
        .px_2()
        .py_1()
        .border_color(if open { theme.primary } else { theme.input })
        .accessibility_label(format!("{name} {field_label}: {label}"))
        .tooltip(tooltip)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .child(label.to_owned()),
        )
        .child(
            Icon::new(if open {
                IconName::ChevronUp
            } else {
                IconName::ChevronDown
            })
            .xsmall()
            .flex_shrink_0(),
        )
}

struct RoleControls {
    model: AnyElement,
    thinking: AnyElement,
    toggle: AnyElement,
}

fn role_settings_row(
    role: &SubagentRole,
    controls: RoleControls,
    expanded: Option<AnyElement>,
    wide: bool,
    theme: &Theme,
) -> AnyElement {
    let summary = div()
        .id(format!("agent-summary-{}", role.name))
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(skin::WEIGHT_STRONG)
                        .child(title_case(&role.name)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(role_metadata(role)),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(role_description(role)),
        )
        .test_support();
    let picks = div()
        .id(format!("agent-controls-{}", role.name))
        .min_w_0()
        .flex_none()
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .child(div().flex_1().min_w_0().child(controls.model))
        .child(div().w(px(144.)).flex_none().child(controls.thinking))
        .test_support();
    let toggle = div().flex_none().child(controls.toggle);
    let row = if wide {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_4()
            .child(summary)
            .child(picks.w(px(360.)))
            .child(toggle)
    } else {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_3()
                    .child(summary)
                    .child(toggle),
            )
            .child(picks.w_full())
    };
    div()
        .id(format!("agent-role-{}", role.name))
        .w_full()
        .min_w_0()
        .flex_none()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .child(row)
        .when_some(expanded, |this, picker| this.child(picker))
        .test_support()
        .into_any_element()
}

fn thinking_value_label(level: &str) -> String {
    match level {
        "off" | "none" => t("Off", "关闭"),
        "on" => t("On", "开启"),
        "minimal" => t("Minimal", "最低"),
        "low" => t("Low", "低"),
        "medium" => t("Medium", "中"),
        "high" => t("High", "高"),
        "xhigh" => t("Extra high", "更高"),
        "max" => t("Maximum", "最高"),
        _ => level,
    }
    .to_owned()
}

fn role_metadata(role: &SubagentRole) -> String {
    let capability = if !role.is_write_capable() {
        t("Read-only", "只读")
    } else if role.isolation == RoleIsolation::Worktree {
        t("Worktree", "工作树")
    } else {
        t("Shared workspace", "共享工作区")
    };
    match role.origin {
        RoleOrigin::Builtin => capability.to_owned(),
        RoleOrigin::User => format!("{capability} · {}", t("User", "用户")),
        RoleOrigin::Project => format!("{capability} · {}", t("Project", "项目")),
    }
}

fn role_description(role: &SubagentRole) -> String {
    if role.origin != RoleOrigin::Builtin {
        return role.description.clone();
    }
    match role.name.as_str() {
        "scout" => t(
            "Find code, documentation, and evidence.",
            "检索代码、文档与证据。",
        ),
        "artisan" => t(
            "Implement changes and run relevant checks.",
            "实施修改并运行相关检查。",
        ),
        "sentinel" => t(
            "Review changes or challenge a specific claim with counterexamples.",
            "审查改动或针对明确假设寻找反例。",
        ),
        _ => return role.description.clone(),
    }
    .to_owned()
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
        options.push((level.clone(), thinking_value_label(&level)));
    }
    options
}

/// The three built-ins lead in a stable order; custom roles follow.
fn group_settings_roles(
    catalog: &mycode::config::RoleCatalog,
) -> (
    Vec<&mycode::config::SubagentRole>,
    Vec<&mycode::config::SubagentRole>,
) {
    const PRIMARY: [&str; 3] = ["scout", "artisan", "sentinel"];
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
    fn discovered_roles_follow_the_builtin_cards() {
        let mut catalog = mycode::config::builtin_roles();
        catalog
            .roles
            .iter_mut()
            .find(|role| role.name == "sentinel")
            .unwrap()
            .origin = mycode::config::RoleOrigin::User;
        catalog
            .roles
            .push(sample_role("reviewer", mycode::config::RoleOrigin::User));
        catalog
            .roles
            .push(sample_role("steward", mycode::config::RoleOrigin::Project));
        catalog
            .roles
            .sort_by(|left, right| left.name.cmp(&right.name));
        let (primary, additional) = super::group_settings_roles(&catalog);
        assert_eq!(
            primary
                .iter()
                .map(|role| role.name.as_str())
                .collect::<Vec<_>>(),
            ["scout", "artisan", "sentinel"]
        );
        assert_eq!(
            additional
                .iter()
                .map(|role| role.name.as_str())
                .collect::<Vec<_>>(),
            ["reviewer", "steward"]
        );
        assert!(
            !mycode::config::builtin_roles()
                .names()
                .iter()
                .any(|name| name == "steward")
        );
    }

    #[test]
    fn shared_writers_are_not_labeled_read_only() {
        let writer = sample_role("reviewer", mycode::config::RoleOrigin::User);
        assert!(
            super::role_metadata(&writer)
                .contains(crate::i18n::t("Shared workspace", "共享工作区"))
        );
        let mut reader = writer;
        reader.tools = Some(vec!["read".into()]);
        assert!(super::role_metadata(&reader).contains(crate::i18n::t("Read-only", "只读")));
    }

    fn sample_role(name: &str, origin: mycode::config::RoleOrigin) -> mycode::config::SubagentRole {
        mycode::config::SubagentRole {
            name: name.to_owned(),
            description: "role".into(),
            isolation: mycode::config::RoleIsolation::Shared,
            thinking: mycode::config::RoleThinking::Default,
            tools: None,
            prompt: "work".into(),
            origin,
        }
    }
}

#[cfg(all(test, feature = "test-support"))]
mod layout_tests;
