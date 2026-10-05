//! Workspace sidebar: named workspaces, their folders, and their sessions.
//!
//! The header switches, creates, renames, and deletes workspaces. Each
//! workspace holds several folders, the way projects sit in a solution;
//! chats belong to exactly one workspace. The selected folder is the working
//! directory; the others stay visible to tools as absolute paths.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
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

/// Sidebar width. Narrow enough that the session list stays primary.
const WIDTH: gpui_kit::Pixels = px(220.);

pub(super) fn render_sidebar(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let roots = workspace.vm().workspace_roots.clone();
    let cwd = workspace.vm().project_dir.clone();
    let sessions = workspace.vm().sessions.clone();
    let bindings = workspace.vm().session_projects.clone();
    let view = workspace.vm().view;
    let active_id =
        crate::view_model::active_workspace(workspace.vm()).map(|workspace| workspace.id.clone());
    let active_name = crate::view_model::active_workspace(workspace.vm())
        .map(|workspace| workspace.name.clone())
        .unwrap_or_else(|| t("Workspace", "工作区").to_owned());
    // Sessions of the workspace the sidebar shows. A session without a
    // membership predates named workspaces and belongs to the first one.
    let in_workspace = |session: &SessionSummary| {
        crate::view_model::workspace_of_session(workspace.vm(), &session.session_id).is_some_and(
            |owner| {
                active_id
                    .as_ref()
                    .is_some_and(|active| owner.id.as_str() == active.as_str())
            },
        )
    };
    let workspace_sessions: Vec<SessionSummary> = sessions
        .iter()
        .filter(|session| in_workspace(session))
        .cloned()
        .collect();
    let session_count = workspace_sessions.len();
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);

    div()
        .id("sidebar")
        .w(WIDTH)
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .bg(skin::glass_sidebar(theme))
        .border_r_1()
        .border_color(skin::glass_border(theme))
        .child(workspace_head(&active_name, session_count, desk.faint, cx))
        .child(
            div().px_2().pt_1().child(
                div()
                    .id("new-chat")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .h(px(30.))
                    .px_2()
                    .rounded(skin::radius_control())
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .hover(|this| {
                        this.bg(skin::frost_hover(theme))
                            .text_color(theme.foreground)
                    })
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_new_session(cx);
                    }))
                    .child(Icon::new(IconName::Plus).xsmall().flex_shrink_0())
                    .child(t("New session", "新建会话")),
            ),
        )
        .child(workspace_roots(
            cx,
            &roots,
            &workspace_sessions,
            &bindings,
            cwd.as_deref(),
        ))
        .child(render_sidebar_footer(workspace, view, cx))
}

/// The sidebar header: the active workspace's name, its session count, and
/// the switcher menu toggle.
fn workspace_head(
    name: &str,
    session_count: usize,
    faint: gpui_kit::Hsla,
    cx: &Context<Workspace>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    div()
        .id("workspace-switcher")
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_3()
        .pt(px(10.))
        .pb(px(8.))
        .border_b_1()
        .border_color(theme.border)
        .cursor_pointer()
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .on_click(cx.listener(|workspace, _, _, cx| {
            let open = !workspace.vm().workspace_menu_open;
            workspace.on_toggle_workspace_menu(open, cx);
        }))
        .child(
            Icon::new(IconName::Folder)
                .xsmall()
                .flex_shrink_0()
                .text_color(theme.muted_foreground),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .truncate()
                .text_color(theme.foreground)
                .child(name.to_owned()),
        )
        .child(
            div()
                .text_xs()
                .flex_shrink_0()
                .text_color(faint)
                .child(session_count.to_string()),
        )
        .child(
            Icon::new(IconName::ChevronDown)
                .xsmall()
                .flex_shrink_0()
                .text_color(theme.muted_foreground),
        )
}

fn workspace_roots(
    cx: &mut Context<Workspace>,
    roots: &[String],
    sessions: &[SessionSummary],
    bindings: &[(String, String)],
    cwd: Option<&str>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    div()
        .id("workspace-roots")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(2.))
        .px_2()
        .py_2()
        .when(roots.is_empty(), |this| {
            this.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t(
                        "Add the folders this workspace should see. Chats can use all of them.",
                        "添加工作区要包含的目录。会话可以使用全部目录。",
                    )),
            )
        })
        .children(
            roots
                .iter()
                .enumerate()
                .map(|(index, root)| root_row(index, root, cwd, cx)),
        )
        .when(!sessions.is_empty(), |this| {
            this.child(
                div()
                    .px_2()
                    .pt_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t("Sessions", "会话")),
            )
            .children(sessions.iter().map(|summary| {
                let project = bindings
                    .iter()
                    .find_map(|(id, path)| (id == &summary.session_id).then_some(path.as_str()));
                session_row(summary, project, cwd, cx)
            }))
        })
        .child(
            div()
                .id("workspace-add")
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .h(px(28.))
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
        )
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
    let label: SharedString = project_label(root).into();
    div()
        .id(format!("workspace-root-{}", element_id(root)))
        .group("workspace-root")
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .px_2()
        .py(px(6.))
        .rounded(skin::radius_control())
        .when(is_cwd, |this| this.bg(theme.accent.opacity(0.45)))
        .cursor_pointer()
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .on_click({
            let path = path.clone();
            cx.listener(move |workspace, _, _, cx| {
                workspace.on_focus_workspace_folder(&path, cx);
            })
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
                        .when(!is_cwd, |row| {
                            let remove = path.clone();
                            row.child(super::hover_delete_button(
                                format!("workspace-root-remove-{index}"),
                                IconName::X,
                                "workspace-root",
                                cx.listener(move |workspace, _, _, cx| {
                                    workspace.on_remove_workspace_root(&remove, cx);
                                }),
                                cx,
                            ))
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_start()
                        .child(path.clone()),
                ),
        )
}

fn session_row(
    summary: &SessionSummary,
    project: Option<&str>,
    cwd: Option<&str>,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let is_open = summary.active;
    let title: SharedString = if summary.corrupt {
        t("Unreadable session", "无法读取的会话").into()
    } else if !summary.title.is_empty() {
        summary.title.clone().into()
    } else if let Some(project) = project {
        project_label(project).into()
    } else {
        t("New session", "新建会话").into()
    };
    let elsewhere = !summary.corrupt
        && project.is_some_and(|path| {
            cwd.is_none_or(|current| !crate::view_model::same_project_path(current, path))
        });
    let folder: Option<SharedString> = elsewhere
        .then(|| project.map(project_label))
        .flatten()
        .map(Into::into);
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
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .group("session-row")
        .px_2()
        .py(px(6.))
        .rounded(skin::radius_control())
        .when(is_open, |this| this.bg(theme.accent.opacity(0.45)))
        .cursor_pointer()
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .text_color(if is_open {
            theme.foreground
        } else {
            theme.sidebar_foreground
        })
        .when(summary.corrupt, |this| this.on_click(corrupt_listener))
        .when(!summary.corrupt, |this| this.on_click(open_listener))
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
        .when(summary.event_count > 0, |this| {
            this.child(
                div()
                    .text_xs()
                    .flex_shrink_0()
                    .text_color(desk.faint)
                    .child(summary.event_count.to_string()),
            )
        })
        .child(super::hover_delete_button(
            format!("session-delete-{}", summary.session_id),
            IconName::Trash,
            "session-row",
            {
                let session_id = session_id.clone();
                cx.listener(move |workspace, _, _, cx| {
                    workspace.on_delete_session(&session_id, cx);
                })
            },
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
        .border_color(theme.sidebar_border)
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

/// Recent folders plus browse, anchored under the workspace add control.
pub(super) fn render_project_menu_layer(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let recents = workspace.vm().recents.clone();
    let roots = workspace.vm().workspace_roots.clone();
    div()
        .id("project-menu-layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .id("project-menu-backdrop")
                .absolute()
                .size_full()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_toggle_project_menu(false, cx);
                })),
        )
        .child(
            skin::popover_panel("project-menu", theme)
                .absolute()
                .top(px(78.))
                .left(px(8.))
                .w(px(244.))
                .max_h(px(360.))
                .overflow_y_scroll()
                .p_1()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(menu_row(
                    "project-menu-browse",
                    t("Browse…", "浏览…"),
                    cx.listener(|workspace, _, _, cx| {
                        workspace.on_toggle_project_menu(false, cx);
                        workspace.on_open_project_dialog(cx);
                    }),
                    IconName::FolderOpen,
                    theme,
                ))
                .when(!recents.is_empty(), |this| {
                    this.child(
                        div()
                            .px_2()
                            .pt_2()
                            .pb(px(2.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t("Recent", "最近")),
                    )
                })
                .children(recents.iter().enumerate().map(|(index, project)| {
                    let path = project.clone();
                    let already = roots
                        .iter()
                        .any(|root| crate::view_model::same_project_path(root, project));
                    div()
                        .id(format!("project-menu-recent-{index}"))
                        .h(px(30.))
                        .px_2()
                        .flex()
                        .flex_row()
                        .items_center()
                        .rounded(skin::radius_control())
                        .text_sm()
                        .when(!already, |row| {
                            row.cursor_pointer()
                                .text_color(theme.foreground)
                                .hover(|row| row.bg(skin::frost_hover(theme)))
                                .on_click(cx.listener(move |workspace, _, _, cx| {
                                    workspace.on_toggle_project_menu(false, cx);
                                    workspace.on_add_workspace_root(&path, cx);
                                }))
                        })
                        .when(already, |row| row.text_color(theme.muted_foreground))
                        .child(div().min_w_0().truncate().child(project_label(project)))
                })),
        )
        .into_any_element()
}

/// Workspace switcher layer: the workspace list plus create, rename, and
/// delete, anchored under the sidebar header.
pub(super) fn render_workspace_menu_layer(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let workspaces = workspace.vm().workspaces.clone();
    let active_id =
        crate::view_model::active_workspace(workspace.vm()).map(|workspace| workspace.id.clone());
    let rename_open = workspace.vm().workspace_rename_open;
    let rename_input = workspace.workspace_rename_input.clone();
    let session_count = |id: &str| -> usize {
        let first = workspaces.first().map(|workspace| workspace.id.as_str());
        workspace
            .vm()
            .sessions
            .iter()
            .filter(|session| {
                match workspace
                    .vm()
                    .session_workspaces
                    .iter()
                    .find(|(existing, _)| existing == &session.session_id)
                {
                    Some((_, bound)) => bound == id,
                    None => first == Some(id),
                }
            })
            .count()
    };
    let mut panel = skin::popover_panel("workspace-menu", theme)
        .absolute()
        .top(px(44.))
        .left(px(8.))
        .w(px(244.))
        .max_h(px(360.))
        .overflow_y_scroll()
        .p_1()
        .flex()
        .flex_col()
        .gap_0p5();
    if rename_open && let Some(input) = rename_input {
        panel = panel
            .child(
                div()
                    .px_2()
                    .pt_1()
                    .pb(px(2.))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t("Workspace name", "工区名称")),
            )
            .child(div().h(px(30.)).px_1().text_sm().child(Input::new(&input)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .px_2()
                    .pt_2()
                    .child(
                        Button::new("workspace-rename-confirm")
                            .icon(IconName::Check)
                            .label(t("Save", "保存"))
                            .small()
                            .primary()
                            .on_click(cx.listener(|workspace, _, _, cx| {
                                workspace.on_confirm_workspace_rename(cx);
                            })),
                    )
                    .child(
                        Button::new("workspace-rename-cancel")
                            .label(t("Cancel", "取消"))
                            .small()
                            .ghost()
                            .on_click(cx.listener(|workspace, _, _, cx| {
                                workspace.on_cancel_workspace_rename(cx);
                            })),
                    ),
            );
    } else {
        for item in &workspaces {
            let id = item.id.clone();
            let selected = active_id.as_deref() == Some(id.as_str());
            let count = session_count(&id);
            panel = panel.child(
                div()
                    .id(format!("workspace-menu-{}", element_id(&item.id)))
                    .h(px(30.))
                    .px_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .rounded(skin::radius_control())
                    .text_sm()
                    .cursor_pointer()
                    .text_color(theme.foreground)
                    .when(selected, |this| this.bg(skin::frost_accent(theme)))
                    .hover(|this| this.bg(skin::frost_hover(theme)))
                    .on_click(cx.listener(move |workspace, _, _, cx| {
                        workspace.on_switch_workspace(&id, cx);
                    }))
                    .child(Icon::new(IconName::Check).xsmall().text_color(if selected {
                        theme.primary
                    } else {
                        theme.muted_foreground
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(item.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .flex_shrink_0()
                            .text_color(theme.muted_foreground)
                            .child(count.to_string()),
                    ),
            );
        }
        panel = panel
            .child(div().mx_2().my(px(2.)).h(px(1.)).bg(theme.border))
            .child(menu_row(
                "workspace-menu-create",
                t("New workspace", "新建工区"),
                cx.listener(|workspace, _, _, cx| {
                    workspace.on_create_workspace(cx);
                }),
                IconName::Plus,
                theme,
            ))
            .child(menu_row(
                "workspace-menu-rename",
                t("Rename", "重命名"),
                cx.listener(|workspace, _, window, cx| {
                    workspace.on_start_workspace_rename(window, cx);
                }),
                IconName::Pen,
                theme,
            ))
            .child(menu_row(
                "workspace-menu-delete",
                t("Delete workspace", "删除工区"),
                cx.listener(move |workspace, _, _, cx| {
                    let id = active_id.clone().unwrap_or_default();
                    workspace.on_delete_workspace(&id, cx);
                }),
                IconName::Trash,
                theme,
            ));
    }
    div()
        .id("workspace-menu-layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .id("workspace-menu-backdrop")
                .absolute()
                .size_full()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_toggle_workspace_menu(false, cx);
                })),
        )
        .child(panel)
        .into_any_element()
}

fn menu_row(
    id: impl Into<gpui_kit::ElementId>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
    icon: IconName,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(30.))
        .px_2()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .rounded(skin::radius_control())
        .text_sm()
        .cursor_pointer()
        .text_color(theme.foreground)
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .on_click(on_click)
        .child(
            Icon::new(icon)
                .xsmall()
                .flex_shrink_0()
                .text_color(theme.muted_foreground),
        )
        .child(div().min_w_0().truncate().child(label.into()))
}
