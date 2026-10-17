//! Sidebar: an action strip, folders, then sessions.
//!
//! New task and session search stay in the strip on top. Folders are their
//! own block; sessions scroll in the block below them. A session binds one
//! folder as its working directory; the others stay visible to tools as
//! absolute paths.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};

use super::{element_id, project_label, skin};
use crate::i18n::t;
use crate::view_model::{MainView, SessionSummary};
use crate::workspace::Workspace;

mod menus;
mod time;
pub(super) use self::menus::render_project_menu_layer;
use self::time::{DateGroup, classify_session_group, format_session_relative_time};

/// Sidebar width. Compact rail with room for session titles and folder paths.
pub(super) const WIDTH_PX: f32 = 248.;
const WIDTH: gpui_kit::Pixels = px(WIDTH_PX);

/// Matches [`super::skin::glass_button`].
const NEW_TASK_H: f32 = 34.;
const SEARCH_H: f32 = 32.;
const ACTION_PAD_Y: f32 = 8.;
const ACTION_GAP: f32 = 6.;
/// Label row of the Folders and Sessions sections. Add folder lives in the
/// Folders one, not in the session list.
const SECTION_HEAD_H: f32 = 28.;
/// Cap the folder block so a long list cannot push sessions off screen.
const FOLDERS_BODY_MAX_H: f32 = 148.;

fn action_strip_h(has_folder: bool, has_search: bool) -> f32 {
    let mut fields = 0.;
    let mut rows = 0.;
    if has_folder {
        fields += NEW_TASK_H;
        rows += 1.;
    }
    if has_search {
        fields += SEARCH_H;
        rows += 1.;
    }
    let gaps = if rows > 1. {
        ACTION_GAP * (rows - 1.)
    } else {
        0.
    };
    ACTION_PAD_Y + fields + gaps + ACTION_PAD_Y
}

/// Project menu top, measured from the window origin (title bar included).
fn project_menu_top(has_folder: bool, has_search: bool) -> gpui_kit::Pixels {
    px(super::title_bar::BAR_HEIGHT + action_strip_h(has_folder, has_search) + SECTION_HEAD_H + 4.)
}

pub(super) fn render_sidebar(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let roots = workspace.vm().workspace_roots.clone();
    let cwd = workspace.vm().project_dir.clone();
    let view = workspace.vm().view;
    // Every session lists here; the project binding only decorates the row.
    // Rows are filtered by reference; only the shown ones are cloned.
    let filter = workspace.vm().session_filter.clone();
    let rows = crate::view_model::session_rows(workspace.vm());
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
    let has_folder = crate::view_model::has_open_folder(workspace.vm());

    div()
        .id("sidebar")
        .w(WIDTH)
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .bg(skin::glass_sidebar(theme))
        .child(action_strip(has_folder, filter_input, cx))
        .child(folders_section(cx, &roots, cwd.as_deref()))
        .child(sessions_section(
            cx,
            &visible_sessions,
            cwd.as_deref(),
            session_count,
            filter_miss,
            desk.faint,
        ))
        .child(render_sidebar_footer(workspace, view, cx))
}

/// New task and session search. A strip, not rows in the session list.
fn action_strip(
    has_folder: bool,
    filter_input: Option<gpui_kit::Entity<InputState>>,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    div()
        .id("sidebar-actions")
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(ACTION_GAP))
        .px_2()
        .py(px(ACTION_PAD_Y))
        .border_b_1()
        .border_color(skin::glass_border(theme))
        .when(has_folder, |strip| {
            strip.child(
                skin::glass_button("new-task", false, theme)
                    .w_full()
                    .h(px(NEW_TASK_H))
                    .justify_start()
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_new_session(cx);
                    }))
                    .child(Icon::new(IconName::Plus).xsmall().flex_shrink_0())
                    .child(super::chat::new_task_label()),
            )
        })
        .when_some(filter_input, |strip, input| {
            strip.child(
                div()
                    .id("sidebar-search")
                    .h(px(SEARCH_H))
                    .text_sm()
                    .child(Input::new(&input)),
            )
        })
}

fn section_label(text: &str, theme: &Theme) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(crate::ui::skin::WEIGHT_STRONG)
        .text_color(theme.muted_foreground)
        .child(text.to_owned())
}

/// Folders for the workspace the sidebar shows. Add folder stays in the
/// heading so it is not a session row.
fn folders_section(
    cx: &mut Context<Workspace>,
    roots: &[String],
    cwd: Option<&str>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let empty = roots.is_empty();
    div()
        .id("sidebar-folders")
        .flex_shrink_0()
        .flex()
        .flex_col()
        .child(
            div()
                .id("sidebar-folders-head")
                .h(px(SECTION_HEAD_H))
                .px_2()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .child(section_label(t("Folders", "目录"), theme))
                .child(
                    div()
                        .id("workspace-add")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .h(px(22.))
                        .px_1()
                        .flex_shrink_0()
                        .rounded(skin::radius_control())
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .cursor_pointer()
                        .hover(|this| {
                            this.bg(skin::frost_hover(theme))
                                .text_color(theme.foreground)
                        })
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            let open = !workspace.vm().project_menu_open;
                            workspace.on_toggle_project_menu(open, cx);
                        }))
                        .child(Icon::new(IconName::Plus).xsmall().flex_shrink_0())
                        .child(t("Add folder", "添加目录")),
                ),
        )
        .child(
            div()
                .id("workspace-roots")
                .flex()
                .flex_col()
                .gap(px(2.))
                .px_2()
                .pb_2()
                .when(!empty, |list| {
                    list.max_h(px(FOLDERS_BODY_MAX_H)).overflow_y_scroll()
                })
                .when(empty, |list| {
                    list.child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .whitespace_normal()
                            .text_color(theme.muted_foreground)
                            .child(t(
                                "Add the folders sessions can work in. Deleting a folder deletes its sessions.",
                                "添加会话可以工作的目录。删除目录会同时删除其中的会话。",
                            )),
                    )
                })
                .children(
                    roots
                        .iter()
                        .enumerate()
                        .map(|(index, root)| root_row(index, root, cwd, cx)),
                ),
        )
}

/// Sessions of the workspace the sidebar shows. Separate from folders.
fn sessions_section(
    cx: &mut Context<Workspace>,
    sessions: &[(SessionSummary, Option<String>)],
    cwd: Option<&str>,
    session_count: usize,
    filter_miss: bool,
    faint: gpui_kit::Hsla,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let now = chrono::Local::now();
    let grouped_rows = build_grouped_session_rows(sessions, cwd, now, cx);
    div()
        .id("sidebar-sessions")
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .border_t_1()
        .border_color(skin::glass_border(theme))
        .child(
            div()
                .id("sidebar-sessions-head")
                .h(px(SECTION_HEAD_H))
                .px_3()
                .flex_shrink_0()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .child(section_label(t("Sessions", "会话"), theme))
                .child(
                    div()
                        .text_xs()
                        .flex_shrink_0()
                        .text_color(faint)
                        .child(session_count.to_string()),
                ),
        )
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
                            .text_color(theme.muted_foreground)
                            .child(t("No matching sessions", "没有匹配的会话")),
                    )
                })
                .when(sessions.is_empty() && !filter_miss, |list| {
                    list.child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t("No sessions yet", "还没有会话")),
                    )
                })
                .children(grouped_rows),
        )
}

fn build_grouped_session_rows(
    sessions: &[(SessionSummary, Option<String>)],
    cwd: Option<&str>,
    now: chrono::DateTime<chrono::Local>,
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
                session_row(summary, project.as_deref(), cwd, relative, cx).into_any_element(),
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

fn root_row(
    index: usize,
    root: &str,
    cwd: Option<&str>,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let is_cwd = cwd.is_some_and(|current| crate::view_model::same_project_path(current, root));
    let path = root.to_owned();
    let remove = path.clone();
    let label: SharedString = project_label(root).into();
    div()
        .id(format!("workspace-root-{}", element_id(root)))
        .relative()
        .group("workspace-root")
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .px_2()
        .py(px(6.))
        .rounded(skin::radius_control())
        .when(is_cwd, |this| this.bg(theme.secondary_hover))
        .cursor_pointer()
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .on_click({
            let path = path.clone();
            cx.listener(move |workspace, _, _, cx| {
                workspace.on_open_workspace_folder(&path, cx);
            })
        })
        .when(is_cwd, |this| {
            this.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(6.))
                    .bottom(px(6.))
                    .w(px(2.))
                    .rounded_full()
                    .bg(theme.primary),
            )
        })
        .child(div().mt(px(3.)).flex_shrink_0().child(
            Icon::new(IconName::Folder).xsmall().text_color(if is_cwd {
                theme.primary
            } else {
                theme.muted_foreground
            }),
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .min_h(px(20.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .truncate()
                                .text_color(theme.foreground)
                                .child(label),
                        )
                        .child(super::hover_delete_button(
                            format!("workspace-root-remove-{index}"),
                            IconName::X,
                            "workspace-root",
                            cx.listener(move |workspace, _, _, cx| {
                                workspace.on_remove_workspace_root(&remove, cx);
                            }),
                            cx,
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_start()
                        .child(path),
                ),
        )
}

fn displayed_session_title(summary: &SessionSummary, project: Option<&str>) -> String {
    if summary.corrupt {
        t("Unreadable session", "无法读取的会话").to_owned()
    } else if !summary.title.is_empty() {
        summary.title.clone()
    } else if let Some(project) = project {
        project_label(project)
    } else {
        super::chat::new_task_label().to_owned()
    }
}

fn session_row(
    summary: &SessionSummary,
    project: Option<&str>,
    cwd: Option<&str>,
    relative_time: Option<String>,
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
    let time_label: Option<SharedString> = relative_time.map(Into::into);
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
                "That session's stored data is unreadable; delete it to clean up.",
                "该会话的存储数据无法读取；删除它以清理。",
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
        .py(px(6.))
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
        .when(is_open, |this| {
            this.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(6.))
                    .bottom(px(6.))
                    .w(px(2.))
                    .rounded_full()
                    .bg(theme.primary),
            )
        })
        .child(
            div()
                .flex_shrink_0()
                .child(
                    Icon::new(IconName::MessageSquare)
                        .xsmall()
                        .text_color(if is_open {
                            theme.primary
                        } else {
                            theme.muted_foreground
                        }),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_sm().truncate().child(title))
                .when_some(folder, |this, folder| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(folder),
                    )
                }),
        )
        .when_some(time_label, |this, label| {
            this.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(desk.faint)
                    .child(label),
            )
        })
        .child(super::hover_delete_button(
            format!("session-delete-{}", summary.session_id),
            IconName::Trash,
            "session-row",
            cx.listener(move |workspace, _, _, cx| {
                workspace.on_delete_session(&session_id, cx);
            }),
            cx,
        ))
}

fn render_sidebar_footer(
    _workspace: &mut Workspace,
    view: MainView,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("sidebar-footer")
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
                    .child(format!("v{}", mycode_app::current_version())),
            ),
        )
}
