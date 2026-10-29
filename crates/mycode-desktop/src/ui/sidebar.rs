//! Separate vertical project and conversation navigation.
//! At 1440 px: 248 px sidebar + 1184 px conversation + 8 px inset.
//! At the 960 px minimum: 216 px sidebar + 420 px conversation + 316 px inspector + 8 px inset.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::{project_label, skin};
use crate::i18n::t;
use crate::view_model::{MainView, SessionSummary};
use crate::workspace::Workspace;

mod menus;
mod projects;
mod time;
pub(super) use self::menus::render_project_menu_layer;
use self::time::{DateGroup, classify_session_group, format_session_relative_time};

/// Navigation keeps enough room for titles without squeezing the conversation.
pub(super) const WIDTH_PX: f32 = 248.;
const NEW_TASK_H: f32 = 36.;
const SEARCH_H: f32 = 32.;
const ACTION_PAD_Y: f32 = 12.;
const SECTION_HEAD_H: f32 = 36.;

pub(super) fn width(window: &Window) -> f32 {
    if f32::from(window.viewport_size().width) < 1100. {
        216.
    } else {
        WIDTH_PX
    }
}

/// The add-project menu starts below the fixed project heading.
fn project_menu_top() -> gpui_kit::Pixels {
    px(super::title_bar::BAR_HEIGHT + NEW_TASK_H + ACTION_PAD_Y * 2. + SECTION_HEAD_H)
}

pub(super) fn render_sidebar(
    workspace: &mut Workspace,
    window: &Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let cwd = workspace.vm().project_dir.clone();
    let view = workspace.vm().view;
    // Project scope applies before text search, including empty search results.
    let filter = workspace.vm().session_filter.clone();
    let rows = crate::view_model::project_session_rows(workspace.vm());
    let session_count = rows.len();
    let filtering = !filter.trim().is_empty();
    let visible_sessions: Vec<(SessionSummary, Option<String>)> = rows
        .into_iter()
        .filter(|(summary, project)| {
            !filtering
                || crate::view_model::session_title_matches(
                    &displayed_session_title(summary, *project),
                    &filter,
                )
        })
        .map(|(summary, project)| (summary.clone(), project.map(str::to_owned)))
        .collect();
    let filter_miss = !filter.trim().is_empty() && visible_sessions.is_empty() && session_count > 0;
    let filter_input = workspace.session_filter_input();
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let armed_delete = workspace.armed_session_delete().map(str::to_owned);

    div()
        .id("sidebar")
        .w(px(width(window)))
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .min_h_0()
        .overflow_hidden()
        .bg(skin::glass_sidebar(theme))
        .child(action_strip(cx))
        .child(projects::render_projects(workspace, window, cx))
        .child(sessions_section(
            cx,
            &visible_sessions,
            cwd.as_deref(),
            filter_input,
            filter_miss,
            desk.faint,
            armed_delete.as_deref(),
        ))
        .child(render_sidebar_footer(view, cx))
}

/// A new conversation is always reachable, including before the first project.
fn action_strip(cx: &Context<Workspace>) -> impl IntoElement + use<> {
    let theme = cx.theme();
    div()
        .id("sidebar-actions")
        .flex_shrink_0()
        .px_3()
        .py(px(ACTION_PAD_Y))
        .child(
            skin::glass_button("new-task", false, theme)
                .w_full()
                .h(px(NEW_TASK_H))
                .justify_start()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    if !workspace.on_new_session(cx) {
                        workspace.on_open_project_dialog(cx);
                    }
                }))
                .child(Icon::new(IconName::Plus).xsmall().flex_shrink_0())
                .child(super::chat::new_task_label()),
        )
}

fn section_label(text: &str, theme: &Theme) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(crate::ui::skin::WEIGHT_STRONG)
        .text_color(theme.muted_foreground)
        .child(text.to_owned())
}

/// Sessions of the workspace the sidebar shows. Separate from folders.
fn sessions_section(
    cx: &mut Context<Workspace>,
    sessions: &[(SessionSummary, Option<String>)],
    cwd: Option<&str>,
    filter_input: Option<gpui_kit::Entity<InputState>>,
    filter_miss: bool,
    faint: gpui_kit::Hsla,
    armed_delete: Option<&str>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let now = chrono::Local::now();
    let grouped_rows = build_grouped_session_rows(sessions, cwd, now, armed_delete, cx);
    div()
        .id("sidebar-sessions")
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .mt_3()
        .overflow_hidden()
        .child(
            div()
                .id("sidebar-sessions-head")
                .h(px(SECTION_HEAD_H))
                .px_4()
                .flex_shrink_0()
                .flex()
                .items_center()
                .child(section_label(t("Conversations", "对话"), theme)),
        )
        .when_some(filter_input, |section, input| {
            section.child(
                div()
                    .id("sidebar-search")
                    .flex_shrink_0()
                    .mx_3()
                    .mb_2()
                    .h(px(SEARCH_H))
                    .text_sm()
                    .child(Input::new(&input)),
            )
        })
        .child(
            div()
                .id("sidebar-session-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(2.))
                .px_2()
                .pb_2()
                .when(filter_miss, |list| {
                    list.child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(faint)
                            .child(t("No matching conversations", "没有匹配的对话")),
                    )
                })
                .when(sessions.is_empty() && !filter_miss, |list| {
                    list.child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(faint)
                            .child(t("No conversations yet", "还没有对话")),
                    )
                })
                .children(grouped_rows),
        )
}

fn build_grouped_session_rows(
    sessions: &[(SessionSummary, Option<String>)],
    cwd: Option<&str>,
    now: chrono::DateTime<chrono::Local>,
    armed_delete: Option<&str>,
    cx: &Context<Workspace>,
) -> Vec<gpui_kit::AnyElement> {
    let theme = cx.theme();
    // Each session is bucketed once instead of once per date group.
    let mut buckets: Vec<Vec<&(SessionSummary, Option<String>)>> =
        vec![Vec::new(); DateGroup::ALL.len()];
    for row in sessions {
        let group = classify_session_group(row.0.updated_at_ms, row.0.corrupt, now);
        if let Some(index) = DateGroup::ALL
            .iter()
            .position(|candidate| *candidate == group)
        {
            buckets[index].push(row);
        }
    }
    let mut elements = Vec::with_capacity(sessions.len() + DateGroup::ALL.len());
    let mut first_group = true;
    for (group, group_sessions) in DateGroup::ALL.into_iter().zip(buckets) {
        if group_sessions.is_empty() {
            continue;
        }
        elements.push(date_group_header(group, first_group, theme).into_any_element());
        first_group = false;
        for (summary, project) in group_sessions {
            let relative =
                format_session_relative_time(summary.updated_at_ms, summary.corrupt, now);
            elements.push(
                session_row(
                    summary,
                    project.as_deref(),
                    cwd,
                    relative,
                    armed_delete == Some(summary.session_id.as_str()),
                    cx,
                )
                .into_any_element(),
            );
        }
    }
    elements
}

fn date_group_header(group: DateGroup, first: bool, theme: &Theme) -> impl IntoElement {
    let desk = super::desk::Desk::of(theme);
    div()
        .id(format!("session-group-{}", group.id_slug()))
        .px_2()
        .when(first, |this| this.pt_1())
        .when(!first, |this| this.pt_2p5())
        .pb(px(2.))
        .text_xs()
        .text_color(desk.faint)
        .child(group.label())
}

fn displayed_session_title(summary: &SessionSummary, _project: Option<&str>) -> String {
    if summary.corrupt {
        t("Unreadable conversation", "无法读取的对话").to_owned()
    } else if !summary.title.is_empty() {
        summary.title.clone()
    } else {
        super::chat::new_task_label().to_owned()
    }
}

fn session_row(
    summary: &SessionSummary,
    project: Option<&str>,
    cwd: Option<&str>,
    relative_time: Option<String>,
    armed: bool,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let is_open = summary.active;
    let title: SharedString = displayed_session_title(summary, project).into();
    let elsewhere = !summary.corrupt
        && project.is_some_and(|path| {
            cwd.is_none_or(|current| !crate::view_model::same_project_path(current, path))
        });
    let folder: Option<SharedString> = elsewhere
        .then(|| project.map(project_label))
        .flatten()
        .map(Into::into);
    let metadata = [relative_time, folder.map(|folder| folder.to_string())]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    let session_id = summary.session_id.clone();
    let open_listener = {
        let session_id = session_id.clone();
        cx.listener(move |workspace, _, _, cx| {
            workspace.on_open_session(&session_id, cx);
        })
    };
    let corrupt_listener = cx.listener(move |workspace, _, _, cx| {
        workspace.push_toast(
            t(
                "That conversation's stored data is unreadable; delete it to clean up.",
                "该对话的存储数据无法读取；可删除此对话进行清理。",
            ),
            crate::workspace::ToastKind::Info,
            cx,
        );
    });
    div()
        .id(format!("session-row-{}", summary.session_id))
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .group("session-row")
        .px_2()
        .py(px(10.))
        .rounded(skin::radius_control())
        .when(is_open, |this| this.bg(theme.secondary_hover))
        .cursor_pointer()
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .text_color(if is_open {
            theme.foreground
        } else {
            theme.sidebar_foreground
        })
        .when(summary.corrupt, |this| this.on_click(corrupt_listener))
        .when(!summary.corrupt, |this| this.on_click(open_listener))
        .min_w_0()
        .flex_shrink_0()
        .child(
            Icon::new(IconName::MessageSquare)
                .xsmall()
                .flex_shrink_0()
                .text_color(theme.muted_foreground),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_sm().truncate().child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(desk.faint)
                        .truncate()
                        .child(metadata),
                ),
        )
        .map(|row| {
            let on_delete = cx.listener(move |workspace, _, _, cx| {
                workspace.on_delete_session(&session_id, cx);
            });
            if armed {
                row.child(armed_delete_button(
                    format!("session-delete-{}", summary.session_id),
                    t("Delete?", "删除？"),
                    on_delete,
                    theme,
                ))
            } else {
                row.child(super::hover_delete_button(
                    format!("session-delete-{}", summary.session_id),
                    IconName::Trash,
                    "session-row",
                    on_delete,
                    cx,
                ))
            }
        })
}

/// A delete control after its first click: always visible, red, and
/// labeled, so the second click is a choice and not a slip.
pub(super) fn armed_delete_button(
    id: String,
    label: &'static str,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_1()
        .h(px(20.))
        .rounded(px(4.))
        .flex_shrink_0()
        .cursor_pointer()
        .bg(theme.danger.opacity(0.12))
        .text_color(theme.danger)
        .text_xs()
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        })
        .child(Icon::new(IconName::Trash).xsmall())
        .child(label)
}

fn render_sidebar_footer(view: MainView, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("sidebar-footer")
        .flex_shrink_0()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(skin::glass_border(theme))
        .child(super::icon_button_marked(
            "footer-settings",
            IconName::Settings,
            view == MainView::Settings,
            cx.listener(|workspace, _, _, cx| {
                let next = if workspace.vm().view == MainView::Settings {
                    MainView::Chat
                } else {
                    MainView::Settings
                };
                workspace.on_show_main_view(next, cx);
            }),
            cx,
        ))
        .child(
            div().flex().flex_row().items_center().gap_2().child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("v{}", mycode_frontend::current_version())),
            ),
        )
}
