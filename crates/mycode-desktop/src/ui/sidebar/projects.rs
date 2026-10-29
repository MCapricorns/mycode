//! Project folders occupy their own vertical list above conversations.
use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};

use crate::i18n::t;
use crate::ui::{element_id, project_label, skin};
use crate::workspace::Workspace;

pub(super) fn render_projects(
    workspace: &Workspace,
    window: &Window,
    cx: &Context<Workspace>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let roots = &workspace.vm().workspace_roots;
    let cwd = workspace.vm().project_dir.as_deref();
    let has_unassigned = crate::view_model::session_rows(workspace.vm())
        .iter()
        .any(|(_, project)| project.is_none());
    // A long project list scrolls independently and leaves conversations visible.
    // Whole rows keep the last visible project label clear of the scroll clip.
    let visible_rows = (f32::from(window.viewport_size().height) * 0.24 / 40.)
        .floor()
        .clamp(3., 5.);
    let list_height = visible_rows * 40. - 4.;
    div()
        .id("sidebar-projects")
        .flex_shrink_0()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .id("sidebar-projects-head")
                .h(px(super::SECTION_HEAD_H))
                .px_4()
                .flex()
                .items_center()
                .justify_between()
                .child(super::section_label(t("Projects", "项目"), theme))
                .child(
                    div()
                        .id("project-add")
                        .h(px(28.))
                        .px_1()
                        .flex()
                        .items_center()
                        .gap_1()
                        .rounded(skin::radius_xs())
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.secondary_hover).text_color(theme.foreground))
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_toggle_project_menu(!workspace.vm().project_menu_open, cx);
                        }))
                        .child(Icon::new(IconName::Plus).xsmall())
                        .child(t("Add", "添加")),
                ),
        )
        .child(
            div()
                .id("sidebar-project-list")
                .max_h(px(list_height))
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .when(roots.is_empty(), |this| {
                    this.child(
                        div()
                            .px_2()
                            .py_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t("Add a project folder", "添加一个项目目录")),
                    )
                })
                .children(roots.iter().map(|root| project_row(root, cwd, cx)))
                .when(has_unassigned, |list| {
                    list.child(unassigned_row(cwd.is_none(), cx))
                }),
        )
}

fn unassigned_row(selected: bool, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("workspace-unassigned")
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .px_2()
        .h(px(36.))
        .rounded(skin::radius_control())
        .when(selected, |this| this.bg(theme.secondary_hover))
        .cursor_pointer()
        .hover(|this| this.bg(theme.secondary_hover))
        .on_click(cx.listener(|workspace, _, _, cx| {
            workspace.on_open_unassigned_conversations(cx);
        }))
        .flex()
        .items_center()
        .gap_2()
        .child(
            Icon::new(IconName::MessageSquare)
                .xsmall()
                .flex_shrink_0()
                .text_color(theme.muted_foreground),
        )
        .child(div().text_sm().truncate().child(t("Unassigned", "未分配")))
}

fn project_row(root: &str, cwd: Option<&str>, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let selected = cwd.is_some_and(|cwd| crate::view_model::same_project_path(cwd, root));
    let path = root.to_owned();
    let remove = path.clone();
    let tooltip_path = path.clone();
    div()
        .id(format!("workspace-root-{}", element_id(root)))
        .group("workspace-root")
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .px_2()
        .h(px(36.))
        .rounded(skin::radius_control())
        .when(selected, |this| this.bg(theme.secondary_hover))
        .cursor_pointer()
        .hover(|this| this.bg(theme.secondary_hover))
        .tooltip(move |window, cx| Tooltip::new(tooltip_path.clone()).build(window, cx))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.on_open_workspace_folder(&path, cx);
        }))
        .flex()
        .items_center()
        .gap_2()
        .child(
            Icon::new(if selected {
                IconName::FolderOpen
            } else {
                IconName::Folder
            })
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
                .child(project_label(root)),
        )
        .map(|row| {
            let id = format!("workspace-root-remove-{}", element_id(root));
            let on_remove = cx.listener(move |workspace, _, _, cx| {
                workspace.on_remove_workspace_root(&remove, cx);
            });
            row.child(crate::ui::hover_delete_button(
                id,
                IconName::X,
                "workspace-root",
                on_remove,
                cx,
            ))
        })
}
