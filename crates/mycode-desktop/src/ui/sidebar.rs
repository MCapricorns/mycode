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
    StatefulInteractiveElement, Styled, Window, div, px,
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

/// Height of the New task button in the action strip.
const NEW_TASK_H: f32 = 34.;
const SEARCH_H: f32 = 32.;
const ACTION_PAD_Y: f32 = 8.;
const ACTION_GAP: f32 = 6.;
/// Label row of the Folders and Sessions sections. Add folder lives in the
/// Folders one, not in the session list.
const SECTION_HEAD_H: f32 = 28.;
/// Share of the space left between the fixed rows that the folder block may
/// take. The list only uses what its rows need, so a short list leaves the
/// space to sessions; a long one scrolls inside this share instead of pushing
/// sessions off screen.
const FOLDERS_BODY_SHARE: f32 = 0.5;
/// Never shrink the folder block below the old fixed cap, for a window too
/// short to divide.
const FOLDERS_BODY_MIN_H: f32 = 148.;
/// Settings and version row at the bottom of the sidebar, its top border
/// included.
const SIDEBAR_FOOTER_H: f32 = 47.;
/// Borders under the action strip and above the sessions block.
const SIDEBAR_ROW_BORDERS: f32 = 2.;

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

/// Height the folder list may use in a window of `window_h` logical pixels.
///
/// The cap follows the space left between the fixed rows (action strip, two
/// section headings, footer), so the folder block shows more rows in a taller
/// window while the session list keeps the other half below it.
fn folders_body_max_h(window_h: f32, has_folder: bool, has_search: bool) -> f32 {
    let sidebar = (window_h - super::title_bar::BAR_HEIGHT).max(0.);
    let fixed = action_strip_h(has_folder, has_search)
        + SECTION_HEAD_H * 2.
        + SIDEBAR_FOOTER_H
        + SIDEBAR_ROW_BORDERS;
    ((sidebar - fixed).max(0.) * FOLDERS_BODY_SHARE).max(FOLDERS_BODY_MIN_H)
}

pub(super) fn render_sidebar(
    workspace: &mut Workspace,
    window: &Window,
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
    let has_search = filter_input.is_some();
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let has_folder = crate::view_model::has_open_folder(workspace.vm());
    let armed_delete = workspace.armed_session_delete().map(str::to_owned);
    let armed_root = workspace.armed_root_delete().map(str::to_owned);
    let folders_max_h = folders_body_max_h(
        f32::from(window.viewport_size().height),
        has_folder,
        has_search,
    );

    div()
        .id("sidebar")
        .w(WIDTH)
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .bg(skin::glass_sidebar(theme))
        .child(action_strip(has_folder, filter_input, cx))
        .child(folders_section(
            cx,
            &roots,
            cwd.as_deref(),
            armed_root.as_deref(),
            folders_max_h,
        ))
        .child(sessions_section(
            cx,
            &visible_sessions,
            cwd.as_deref(),
            session_count,
            filter_miss,
            desk.faint,
            armed_delete.as_deref(),
        ))
        .child(render_sidebar_footer(view, cx))
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
    armed_root: Option<&str>,
    max_h: f32,
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
                // Labels line up with the row text below (list + row inset);
                // the add button's own `px_1` brings its text to the same
                // right edge as the session count.
                .pl_4()
                .pr_3()
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
                    list.max_h(px(max_h)).overflow_y_scroll()
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
                                "Add the folders sessions can work in. Removing a folder deletes its sessions.",
                                "添加会话可以工作的目录。移除目录会同时删除其中的会话。",
                            )),
                    )
                })
                .children(
                    roots
                        .iter()
                        .enumerate()
                        .map(|(index, root)| {
                            root_row(index, root, cwd, armed_root == Some(root.as_str()), cx)
                        }),
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
        .border_t_1()
        .border_color(skin::glass_border(theme))
        .child(
            div()
                .id("sidebar-sessions-head")
                .h(px(SECTION_HEAD_H))
                .px_4()
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

fn root_row(
    index: usize,
    root: &str,
    cwd: Option<&str>,
    armed: bool,
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
                        .map(|row| {
                            let id = format!("workspace-root-remove-{index}");
                            let on_remove = cx.listener(move |workspace, _, _, cx| {
                                workspace.on_remove_workspace_root(&remove, cx);
                            });
                            if armed {
                                // Removing the folder deletes its sessions.
                                row.child(armed_delete_button(
                                    id,
                                    t("Delete sessions?", "删除会话？"),
                                    on_remove,
                                    theme,
                                ))
                            } else {
                                row.child(super::hover_delete_button(
                                    id,
                                    IconName::X,
                                    "workspace-root",
                                    on_remove,
                                    cx,
                                ))
                            }
                        }),
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
fn armed_delete_button(
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

#[cfg(test)]
mod tests {
    use super::{
        FOLDERS_BODY_MIN_H, FOLDERS_BODY_SHARE, SECTION_HEAD_H, SIDEBAR_FOOTER_H,
        SIDEBAR_ROW_BORDERS, action_strip_h, folders_body_max_h,
    };

    /// The old fixed 148 px cap clipped the third folder row. The block now
    /// follows the leftover space, half each with sessions.
    #[test]
    fn the_folder_block_follows_the_window_and_shares_with_sessions() {
        let available = |window: f32| {
            window
                - crate::ui::title_bar::BAR_HEIGHT
                - action_strip_h(true, true)
                - SECTION_HEAD_H * 2.
                - SIDEBAR_FOOTER_H
                - SIDEBAR_ROW_BORDERS
        };
        let default_window = folders_body_max_h(900., true, true);
        let small_window = folders_body_max_h(600., true, true);

        assert!((default_window - available(900.) * FOLDERS_BODY_SHARE).abs() < 0.01);
        assert!((small_window - available(600.) * FOLDERS_BODY_SHARE).abs() < 0.01);
        assert!(default_window > small_window, "a taller window shows more");
        // Default 14 px type: a folder row is 12 px padding, a 22.6 px title
        // line, a 1 px gap and a 19.4 px path line (~55 px), plus the list's
        // 2 px row gap and 8 px bottom padding.
        const ROW: f32 = 55.;
        const GAP: f32 = 2.;
        const PAD: f32 = 8.;
        assert!(
            default_window >= 5. * ROW + 4. * GAP + PAD,
            "a 900 px window shows five folder rows, got {default_window}"
        );
        assert!(
            small_window >= 3. * ROW + 2. * GAP + PAD,
            "the smallest window still shows three rows, got {small_window}"
        );
        assert!(
            default_window < available(900.) * 0.6,
            "sessions keep their half of the leftover space"
        );
        assert_eq!(
            folders_body_max_h(0., true, true),
            FOLDERS_BODY_MIN_H,
            "a window too short to divide keeps the old block height"
        );
    }
}
