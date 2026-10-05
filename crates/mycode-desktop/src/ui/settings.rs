//! The full-page settings shell: a top bar back to the desk, a grouped left
//! nav, and a sectioned content pane.
//!
//! Rendering order matters: entities are created and row lists materialized
//! with `&mut Context` first, and only then is `cx.theme()` borrowed for the
//! layout pass.
mod about;
mod agents;
mod data;
mod general;
mod mcp;
mod models;
mod provider_detail;
mod shell;
mod skills;
mod web;
mod widgets;

pub(crate) use mcp::{McpForm, build_mcp_server};
pub(crate) use models::ProviderForm;
pub(crate) use web::BackendForm;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px, rems,
};

use crate::i18n::t;
use crate::view_model::{MainView, SettingsSection, UpdateState};
use crate::workspace::Workspace;

pub(super) fn render_settings_view(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    workspace.ensure_agent_roles();
    let search = workspace.settings_search_input(window, cx);
    let section = workspace.vm().settings_section;
    let query = workspace.vm().settings_query.clone();
    let settings_ready = workspace.vm().settings.is_some();
    let header_meta = workspace.vm().settings.clone().map(|s| (s.dirty, s.saving));
    let nav = render_settings_nav(workspace, section, &query, cx).into_any_element();
    let theme = cx.theme();
    let border = crate::ui::skin::glass_border(theme);
    div()
        .id("settings-view")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(
            div()
                .id("settings-header")
                .h(px(56.))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_4()
                .gap_3()
                .border_b_1()
                .border_color(border)
                .bg(crate::ui::skin::glass(theme))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .min_w_0()
                        .child(
                            crate::ui::skin::glass_button("settings-back", false, theme)
                                .flex_shrink_0()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_show_main_view(MainView::Chat, cx);
                                }))
                                .child(Icon::new(IconName::ArrowLeft).with_size(px(16.)))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .child(t("Back to desk", "返回工作台")),
                                )
                                .child(
                                    div()
                                        .px(px(6.))
                                        .h(px(18.))
                                        .flex()
                                        .items_center()
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(theme.border)
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("Esc"),
                                ),
                        )
                        .child(
                            div()
                                .text_lg()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(t("Settings", "设置")),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(220.))
                                .h(px(32.))
                                .text_sm()
                                .child(Input::new(&search)),
                        )
                        .when_some(header_meta, |this, (dirty, saving)| {
                            this.child(
                                Button::new("settings-save")
                                    .icon(IconName::Check)
                                    .label(t("Save changes", "保存更改"))
                                    .small()
                                    .primary()
                                    .disabled(!dirty || saving)
                                    .on_click(cx.listener(|workspace, _, _, cx| {
                                        workspace.on_save_settings(cx);
                                    })),
                            )
                        }),
                ),
        )
        .child(
            div()
                .id("settings-body")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .child(nav)
                .child(
                    div()
                        .id("settings-content")
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_y_scroll()
                        // The scrollport fills the pane. Its page is a flex
                        // item with no grow, so the cards stay as tall as
                        // their content instead of stretching to this viewport.
                        .flex()
                        .flex_col()
                        .justify_start()
                        .child(super::motion::fade_in(
                            format!("settings-page-{}", settings_page_key(workspace)),
                            cx.reduce_motion(),
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
                                        .w_full()
                                        .h_auto()
                                        .flex_none()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .pb_1()
                                        .child(
                                            div()
                                                .text_lg()
                                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                .child(section.label()),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(theme.muted_foreground)
                                                .whitespace_normal()
                                                .child(section.hint()),
                                        ),
                                )
                                .when(!settings_ready, |this| {
                                    this.child(div().text_sm().opacity(0.6).child(t(
                                        "Loading settings\u{2026}",
                                        "正在加载设置\u{2026}",
                                    )))
                                })
                                .when(settings_ready, |this| {
                                    this.child(match section {
                                        SettingsSection::General => {
                                            general::render_general_section(workspace, window, cx)
                                        }
                                        SettingsSection::Models => {
                                            models::render_models_section(workspace, window, cx)
                                        }
                                        SettingsSection::Agents => {
                                            agents::render_agents_section(workspace, window, cx)
                                        }
                                        SettingsSection::Skills => {
                                            skills::render_skills_section(workspace, cx)
                                        }
                                        SettingsSection::Shell => {
                                            shell::render_shell_section(workspace, window, cx)
                                        }
                                        SettingsSection::Mcp => {
                                            mcp::render_mcp_section(workspace, window, cx)
                                        }
                                        SettingsSection::Web => {
                                            web::render_web_section(workspace, window, cx)
                                        }
                                        SettingsSection::Data => {
                                            data::render_data_section(workspace, cx)
                                        }
                                        SettingsSection::About => {
                                            about::render_about_section(workspace, cx)
                                        }
                                    })
                                }),
                        )),
                ),
        )
        .into_any_element()
}

/// Identity of the settings page that should fade in. Menus are left out so
/// opening a dropdown does not replay the fade.
fn settings_page_key(workspace: &Workspace) -> String {
    let vm = workspace.vm();
    match vm.settings_section {
        SettingsSection::Models => match vm.models_subview {
            crate::view_model::ModelsSubview::List => format!(
                "models-list-{}",
                vm.provider_detail.as_deref().unwrap_or("all")
            ),
            crate::view_model::ModelsSubview::Catalog => "models-catalog".to_owned(),
            crate::view_model::ModelsSubview::Custom => "models-custom".to_owned(),
        },
        SettingsSection::Web => match vm.web_subview {
            crate::view_model::WebSubview::List => "web-list".to_owned(),
            crate::view_model::WebSubview::Custom => "web-custom".to_owned(),
        },
        SettingsSection::Mcp => match vm.mcp_subview {
            crate::view_model::McpSubview::List => "mcp-list".to_owned(),
            crate::view_model::McpSubview::Catalog => "mcp-catalog".to_owned(),
            crate::view_model::McpSubview::Json => "mcp-json".to_owned(),
            crate::view_model::McpSubview::Custom => "mcp-custom".to_owned(),
        },
        other => other.id().to_owned(),
    }
}

/// What the nav shows beside each section: a count or a status lamp.
#[derive(Clone, Copy, Default)]
struct NavBadge {
    /// Right-aligned mono figure (configured providers, enabled servers).
    count: Option<usize>,
    /// Attention lamp (an update is waiting, a provider has no key).
    lamp: Option<gpui_kit::Hsla>,
}

/// Per-section badges derived from live state.
fn nav_badges(workspace: &Workspace, cx: &Context<Workspace>) -> Vec<(SettingsSection, NavBadge)> {
    let theme = cx.theme();
    let desk = crate::ui::desk::Desk::of(theme);
    let vm = workspace.vm();
    let settings = vm.settings.as_ref();
    let providers = settings.map(|s| s.providers.iter().filter(|p| p.enabled).count());
    let servers = settings.map(|s| s.mcp_servers.iter().filter(|m| m.enabled).count());
    let backends = settings.map(|s| s.web_backends.iter().filter(|b| b.enabled).count());
    let missing_key = settings.is_some_and(|s| {
        s.providers
            .iter()
            .any(|p| p.enabled && !s.providers_with_keys.contains(&p.id))
    });
    let update_lamp = match vm.update {
        UpdateState::Available { .. } | UpdateState::Ready { .. } => Some(desk.amber),
        UpdateState::Failed(_) => Some(desk.red),
        _ => None,
    };
    vec![
        (SettingsSection::General, NavBadge::default()),
        (
            SettingsSection::Models,
            NavBadge {
                count: providers,
                lamp: missing_key.then_some(desk.red),
            },
        ),
        (
            SettingsSection::Agents,
            NavBadge {
                count: settings.map(|s| {
                    workspace
                        .agent_roles()
                        .roles
                        .iter()
                        .filter(|role| s.subagents.is_enabled(&role.name))
                        .count()
                }),
                lamp: None,
            },
        ),
        (
            SettingsSection::Skills,
            NavBadge {
                count: Some(vm.skills.len()),
                lamp: None,
            },
        ),
        (SettingsSection::Shell, NavBadge::default()),
        (
            SettingsSection::Mcp,
            NavBadge {
                count: servers,
                lamp: None,
            },
        ),
        (
            SettingsSection::Web,
            NavBadge {
                count: backends,
                lamp: None,
            },
        ),
        (SettingsSection::Data, NavBadge::default()),
        (
            SettingsSection::About,
            NavBadge {
                count: None,
                lamp: update_lamp,
            },
        ),
    ]
}

/// Settings navigation: grouped single-line rows, filtered by the top-bar search.
fn render_settings_nav(
    workspace: &Workspace,
    section: SettingsSection,
    query: &str,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let badges = nav_badges(workspace, cx);
    let role_names = workspace.agent_roles().names();
    let dirty = workspace.vm().settings.as_ref().is_some_and(|s| s.dirty);
    let theme = cx.theme();
    let desk = crate::ui::desk::Desk::of(theme);
    let mut groups: Vec<AnyElement> = Vec::new();
    for (group, members) in SettingsSection::GROUPS {
        let mut rows: Vec<AnyElement> = Vec::new();
        for candidate in members.iter().copied() {
            if !section_matches(candidate, query, &role_names) {
                continue;
            }
            let badge = badges
                .iter()
                .find(|(s, _)| *s == candidate)
                .map(|(_, badge)| *badge)
                .unwrap_or_default();
            rows.push(nav_row(candidate, candidate == section, badge, theme, cx));
        }
        if rows.is_empty() {
            continue;
        }
        groups.push(
            div()
                .id(format!("settings-nav-group-{group}"))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .px_2()
                        .pt(px(10.))
                        .pb(px(2.))
                        .text_xs()
                        .text_color(desk.faint)
                        .child(SettingsSection::group_label(group)),
                )
                .children(rows)
                .into_any_element(),
        );
    }
    div()
        .id("settings-nav")
        .w(px(236.))
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .border_r_1()
        .border_color(crate::ui::skin::glass_border(theme))
        .bg(crate::ui::skin::glass_sidebar(theme))
        .child(
            div()
                .id("settings-nav-groups")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .px_2()
                .pb_2()
                .children(groups),
        )
        .child(
            div()
                .id("settings-nav-footer")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(desk.faint)
                .child(format!("v{}", env!("CARGO_PKG_VERSION")))
                .when(dirty, |this| {
                    this.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .text_color(desk.amber)
                            .child(crate::ui::lamp(desk.amber))
                            .child(t("UNSAVED", "未保存")),
                    )
                }),
        )
}

fn section_matches(section: SettingsSection, query: &str, role_names: &[String]) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let query = query.to_lowercase();
    section.label().to_lowercase().contains(&query)
        || section.hint().to_lowercase().contains(&query)
        || (section == SettingsSection::Agents
            && role_names
                .iter()
                .any(|name| name.to_lowercase().contains(&query)))
}

/// One nav row: icon, label, and the badge column.
fn nav_row(
    candidate: SettingsSection,
    selected: bool,
    badge: NavBadge,
    theme: &Theme,
    cx: &Context<Workspace>,
) -> AnyElement {
    let ink = if selected {
        theme.sidebar_accent_foreground
    } else {
        theme.sidebar_foreground
    };
    div()
        .id(format!("settings-nav-{}", candidate.id()))
        .h(px(36.))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .rounded(crate::ui::skin::radius_control())
        .cursor_pointer()
        .when(selected, |this| {
            this.bg(crate::ui::skin::frost_accent(theme))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
        })
        .hover(|this| this.bg(crate::ui::skin::frost_hover(theme)))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.on_show_settings_section(candidate, cx);
        }))
        .child(
            Icon::new(candidate.icon())
                .with_size(px(14.))
                .text_color(if selected {
                    theme.primary
                } else {
                    theme.muted_foreground
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(ink)
                .child(candidate.label()),
        )
        .when_some(badge.lamp, |this, color| this.child(crate::ui::lamp(color)))
        .when_some(badge.count, |this, count| {
            this.child(
                div()
                    .min_w(px(18.))
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(if count > 0 {
                        ink
                    } else {
                        theme.muted_foreground
                    })
                    .child(count.to_string()),
            )
        })
        .into_any_element()
}

/// A nested-page header. The label names the page this control returns to,
/// and it is not the control that leaves settings for the desk.
pub(super) fn subview_header(
    back_label: &str,
    title: &str,
    hint: Option<&str>,
    on_back: impl Fn(&mut Workspace, &mut Context<Workspace>) + 'static,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    div()
        .id("subview-header")
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .pb_1()
        .child(
            crate::ui::skin::glass_button("subview-back", false, theme)
                .flex_shrink_0()
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    on_back(workspace, cx);
                }))
                .child(Icon::new(IconName::ArrowLeft).with_size(px(14.)))
                .child(div().text_sm().child(back_label.to_owned())),
        )
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child(title.to_owned()),
        )
        .when_some(hint, |this, hint| {
            this.child(div().text_xs().opacity(0.5).child(hint.to_owned()))
        })
        .into_any_element()
}
