//! Project and conversation entry points for an empty conversation.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, Div, Focusable as _, InteractiveElement, IntoElement, ParentElement, Stateful,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::i18n::t;
use crate::ui::{element_id, hover_delete_button, project_label, skin};
use crate::view_model::{SessionSummary, WorkspaceState, project_session_rows, session_rows};
use crate::workspace::Workspace;

const MAX_RECENT_ROWS: usize = 4;
pub(super) const CONTENT_WIDTH: f32 = 640.;

/// Welcome scrolls from the top, independently of transcript tail-follow.
pub(super) fn scroll_handle(window: &mut gpui_kit::Window) -> gpui_kit::ScrollHandle {
    window.with_global_id(gpui_kit::ElementId::from("welcome-scroll"), |id, window| {
        window.with_element_state(id, |handle: Option<gpui_kit::ScrollHandle>, _| {
            let handle = handle.unwrap_or_default();
            (handle.clone(), handle)
        })
    })
}

#[must_use]
pub(crate) fn new_task_label() -> &'static str {
    t("New conversation", "新对话")
}

fn recent_conversations(state: &WorkspaceState) -> Vec<(&SessionSummary, Option<&str>)> {
    let rows = if state.project_dir.is_some() {
        project_session_rows(state)
    } else {
        session_rows(state)
    };
    rows.into_iter()
        .filter(|(session, _)| !session.active && !session.corrupt && session.event_count > 0)
        .take(MAX_RECENT_ROWS)
        .collect()
}

pub(super) fn render_welcome(
    workspace: &Workspace,
    cx: &Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let state = workspace.vm();
    let project = state.project_dir.as_deref();
    let recent_projects = project.is_none() && !state.recents.is_empty();
    let conversations = recent_conversations(state);
    let heading = project
        .map(project_label)
        .unwrap_or_else(|| new_task_label().to_owned());

    div()
        .id("welcome")
        .w_full()
        .max_w(px(CONTENT_WIDTH))
        .min_w_0()
        .flex_none()
        .mx_auto()
        .pt(px(48.))
        .pb_4()
        .flex()
        .flex_col()
        .gap_6()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .id("welcome-heading")
                        .text_xl()
                        .font_weight(skin::WEIGHT_STRONG)
                        .truncate()
                        .child(heading),
                )
                .child(if let Some(path) = project {
                    div()
                        .text_sm()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(path.to_owned())
                } else {
                    div().text_sm().text_color(theme.muted_foreground).child(t(
                        "Open a project to work with its files.",
                        "打开项目后，就可以在对话中处理其中的文件。",
                    ))
                }),
        )
        .child(welcome_actions(project.is_some(), cx))
        .when(recent_projects || !conversations.is_empty(), |this| {
            this.child(
                recent_sections()
                    .when(recent_projects, |sections| {
                        sections.child(
                            recent_section(
                                "welcome-projects",
                                t("Recent projects", "最近项目"),
                                theme,
                            )
                            .children(
                                state
                                    .recents
                                    .iter()
                                    .take(MAX_RECENT_ROWS)
                                    .map(|project| recent_project_row(project, cx)),
                            ),
                        )
                    })
                    .when(!conversations.is_empty(), |sections| {
                        sections.child(
                            recent_section(
                                "welcome-conversations",
                                t("Recent conversations", "最近对话"),
                                theme,
                            )
                            .children(conversations.into_iter().map(
                                |(session, binding)| {
                                    let context = if project.is_none() {
                                        Some(binding.map(project_label).unwrap_or_else(|| {
                                            t("Default chat", "默认聊天").to_owned()
                                        }))
                                    } else {
                                        None
                                    };
                                    recent_conversation_row(session, context, cx)
                                },
                            )),
                        )
                    }),
            )
        })
        .into_any_element()
}

fn welcome_actions(has_project: bool, cx: &Context<Workspace>) -> Stateful<Div> {
    let theme = cx.theme();
    let project = welcome_action(
        "welcome-open-project",
        IconName::FolderOpen,
        if has_project {
            t("Switch project", "切换项目")
        } else {
            t("Open project", "打开项目")
        },
        !has_project,
        theme,
    )
    .on_click(cx.listener(|workspace, _, _, cx| workspace.on_open_project_dialog(cx)));
    let compose = welcome_action(
        "welcome-begin-task",
        IconName::ArrowRight,
        if has_project {
            t("Write a task", "输入任务")
        } else {
            t("Chat directly", "直接对话")
        },
        has_project,
        theme,
    )
    .on_click(cx.listener(|workspace, _, window, cx| {
        workspace
            .composer()
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
    }));
    if has_project {
        action_row().child(compose).child(project)
    } else {
        action_row().child(project).child(compose)
    }
}

fn action_row() -> Stateful<Div> {
    div()
        .id("welcome-actions")
        .w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
}

fn welcome_action(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    emphasized: bool,
    theme: &Theme,
) -> Stateful<Div> {
    skin::glass_button(id, emphasized, theme)
        .max_w_full()
        .min_w_0()
        .h(px(36.))
        .child(Icon::new(icon).with_size(px(15.)).flex_shrink_0())
        .child(div().min_w_0().truncate().child(label))
}

fn recent_sections() -> Stateful<Div> {
    div()
        .id("welcome-recents")
        .flex()
        .flex_wrap()
        .items_start()
        .gap_5()
        .w_full()
}

fn recent_section(id: &'static str, title: &'static str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .flex_1()
        .flex_basis(px(236.))
        .min_w_0()
        .max_w_full()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .pb_2()
                .mb_1()
                .border_b_1()
                .border_color(theme.border)
                .text_xs()
                .font_weight(skin::WEIGHT_STRONG)
                .text_color(theme.muted_foreground)
                .child(title),
        )
}

fn recent_row(
    id: String,
    icon: IconName,
    title: String,
    detail: Option<String>,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .min_h(px(36.))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded(skin::radius_xs())
        .cursor_pointer()
        .hover(|row| row.bg(theme.secondary_hover))
        .child(
            Icon::new(icon)
                .with_size(px(15.))
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
                .when_some(detail, |row, text| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(text),
                    )
                }),
        )
}

fn recent_project_row(project: &str, cx: &Context<Workspace>) -> Stateful<Div> {
    let id = element_id(project);
    recent_row(
        format!("recent-{id}"),
        IconName::Folder,
        project_label(project),
        Some(project.to_owned()),
        cx.theme(),
    )
    .group("recent-row")
    .on_click({
        let project = project.to_owned();
        cx.listener(move |workspace, _, _, cx| workspace.on_open_recent(&project, cx))
    })
    .child(hover_delete_button(
        format!("recent-remove-{id}"),
        IconName::X,
        "recent-row",
        {
            let project = project.to_owned();
            cx.listener(move |workspace, _, _, cx| workspace.on_remove_recent(&project, cx))
        },
        cx,
    ))
}

fn recent_conversation_row(
    session: &SessionSummary,
    context: Option<String>,
    cx: &Context<Workspace>,
) -> Stateful<Div> {
    let session_id = session.session_id.clone();
    let title = if session.title.trim().is_empty() {
        new_task_label().to_owned()
    } else {
        session.title.clone()
    };
    recent_row(
        format!("welcome-session-{session_id}"),
        IconName::MessageSquare,
        title,
        context,
        cx.theme(),
    )
    .on_click(cx.listener(move |workspace, _, _, cx| {
        workspace.on_open_session(&session_id, cx);
    }))
    .child(
        Icon::new(IconName::ChevronRight)
            .xsmall()
            .flex_shrink_0()
            .text_color(cx.theme().muted_foreground),
    )
}

#[cfg(all(test, feature = "test-support"))]
mod layout_tests;
