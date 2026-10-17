//! The folder menu that drops from the sidebar.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};

use super::project_menu_top;
use crate::i18n::t;
use crate::ui::{project_label, skin};
use crate::workspace::Workspace;

/// Recent folders plus browse, anchored under the folder add control.
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
