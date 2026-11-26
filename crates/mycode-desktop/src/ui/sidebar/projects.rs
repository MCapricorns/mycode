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

const PROJECT_ROW_H: f32 = 32.;
const PROJECT_ROW_GAP: f32 = 2.;

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
    let row_pitch = PROJECT_ROW_H + PROJECT_ROW_GAP;
    let visible_rows = (f32::from(window.viewport_size().height) * 0.24 / row_pitch)
        .floor()
        .clamp(3., 5.);
    let list_height = visible_rows * row_pitch - PROJECT_ROW_GAP;
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
                .gap(px(PROJECT_ROW_GAP))
                .px_2()
                .children(roots.iter().map(|root| project_row(root, cwd, cx)))
                .when(has_unassigned, |list| {
                    list.child(unassigned_row(cwd.is_none(), cx))
                }),
        )
}

fn unassigned_row(selected: bool, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    super::navigation_row("workspace-unassigned", selected, theme)
        .h(px(PROJECT_ROW_H))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.on_open_unassigned_conversations(cx);
        }))
        .child(
            Icon::new(IconName::MessageSquare)
                .xsmall()
                .flex_shrink_0()
                .text_color(if selected {
                    theme.primary
                } else {
                    theme.muted_foreground
                }),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(t("Default chat", "默认聊天")),
        )
}

fn project_row(root: &str, cwd: Option<&str>, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let selected = cwd.is_some_and(|cwd| crate::view_model::same_project_path(cwd, root));
    let path = root.to_owned();
    let remove = path.clone();
    let tooltip_path = path.clone();
    super::navigation_row(
        format!("workspace-root-{}", element_id(root)),
        selected,
        theme,
    )
    .group("workspace-root")
    .h(px(PROJECT_ROW_H))
    .tooltip(move |window, cx| Tooltip::new(tooltip_path.clone()).build(window, cx))
    .on_click(cx.listener(move |workspace, _, _, cx| {
        workspace.on_open_workspace_folder(&path, cx);
    }))
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
            .truncate()
            .child(project_label(root)),
    )
    .child(crate::ui::hover_delete_button(
        format!("workspace-root-remove-{}", element_id(root)),
        IconName::X,
        "workspace-root",
        cx.listener(move |workspace, _, _, cx| {
            workspace.on_remove_workspace_root(&remove, cx);
        }),
        cx,
    ))
}
