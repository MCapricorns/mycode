//! The folder and workspace menus that drop from the sidebar header.
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

use super::{project_menu_top, workspace_menu_top};
use crate::i18n::t;
use crate::ui::{element_id, project_label, skin};
use crate::workspace::Workspace;

/// Recent folders plus browse, anchored under the workspace add control.
pub(crate) fn render_project_menu_layer(
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
                .bg(skin::menu_scrim(theme))
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_toggle_project_menu(false, cx);
                })),
        )
        .child(
            skin::popover_panel("project-menu", theme)
                .absolute()
                .top(project_menu_top(
                    crate::view_model::has_open_folder(workspace.vm()),
                    workspace.session_filter_input().is_some(),
                ))
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
pub(crate) fn render_workspace_menu_layer(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let workspaces = workspace.vm().workspaces.clone();
    let active_id =
        crate::view_model::active_workspace(workspace.vm()).map(|workspace| workspace.id.clone());
    let rename_open = workspace.vm().workspace_rename_open;
    let rename_input = workspace.workspace_rename_input.clone();
    let session_counts: std::collections::HashMap<String, usize> =
        crate::view_model::workspace_session_counts(workspace.vm())
            .into_iter()
            .map(|(id, count)| (id.to_owned(), count))
            .collect();
    let mut panel = skin::popover_panel("workspace-menu", theme)
        .absolute()
        .top(workspace_menu_top())
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
                    .child(t("Workspace name", "工作区名称")),
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
            let count = session_counts.get(&id).copied().unwrap_or(0);
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
                t("New workspace", "新建工作区"),
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
                t("Delete workspace", "删除工作区"),
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
                .bg(skin::menu_scrim(theme))
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
