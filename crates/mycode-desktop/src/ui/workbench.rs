//! The MYCode workbench: project shelf, session page, and a visible change trail.
//!
//! At 1440 px: 232 px ledger + (1440 - 232 - 8) px sheet + 8 px inset.
//! At the 960 px native minimum the ledger is 196 px; the work keeps 756 px.
//! The shelf owns project scope; the page band owns the current session.
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};

use super::{project_label, skin};
use crate::i18n::t;
use crate::workspace::Workspace;

pub(super) const SHELF_HEIGHT: f32 = 76.;
pub(super) const SLOGAN: (&str, &str) = ("Local changes. Clear trail.", "每次改动，有迹可循。");

/// A session page uses its random suffix; the shared "sess-" prefix carries no identity.
pub(super) fn session_page_id(session_id: &str) -> String {
    session_id
        .rsplit('-')
        .next()
        .unwrap_or(session_id)
        .chars()
        .take(6)
        .collect()
}

/// Projects stay above the notebook, rather than competing with session rows.
pub(super) fn render_project_shelf(
    workspace: &Workspace,
    window: &Window,
    cx: &Context<Workspace>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let roots = &workspace.vm().workspace_roots;
    let cwd = workspace.vm().project_dir.as_deref();
    let armed = workspace.armed_root_delete();
    div()
        .id("project-shelf")
        .h(px(SHELF_HEIGHT))
        .w_full()
        .flex_shrink_0()
        .flex()
        .flex_row()
        .items_center()
        .child(
            div()
                .w(px(super::sidebar::width(window)))
                .flex_shrink_0()
                .px_4()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(skin::WEIGHT_STRONG)
                        .child(t("Project shelf", "项目书架")),
                )
                .child(
                    div()
                        .id("project-add")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .h(px(24.))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .cursor_pointer()
                        .hover(|this| this.text_color(theme.foreground))
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            let open = !workspace.vm().project_menu_open;
                            workspace.on_toggle_project_menu(open, cx);
                        }))
                        .child(Icon::new(IconName::Plus).xsmall())
                        .child(t("Add folder", "添加目录")),
                ),
        )
        .child(
            div()
                .id("project-shelf-scroll")
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_x_scroll()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .pr_2()
                .when(roots.is_empty(), |this| {
                    this.child(div().text_sm().text_color(theme.muted_foreground).child(t(
                        "Open a folder to set the scope of your work.",
                        "打开目录，确定这次工作的范围。",
                    )))
                })
                .children(roots.iter().enumerate().map(|(index, root)| {
                    project_tab(index, root, cwd, armed == Some(root.as_str()), cx)
                })),
        )
}

fn project_tab(
    index: usize,
    root: &str,
    cwd: Option<&str>,
    armed: bool,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let selected = cwd.is_some_and(|cwd| crate::view_model::same_project_path(cwd, root));
    let path = root.to_owned();
    let remove = path.clone();
    div()
        .id(format!("workspace-root-{}", super::element_id(root)))
        .group("workspace-root")
        .w(px(if armed { 276. } else { 224. }))
        .h(px(60.))
        .flex_shrink_0()
        .px_3()
        .py_2()
        .border_b_2()
        .border_color(if selected {
            theme.foreground
        } else {
            theme.border.opacity(0.)
        })
        .when(selected, |this| this.bg(skin::ambient(theme)))
        .cursor_pointer()
        .hover(|this| this.bg(theme.secondary_hover))
        .on_click({
            let path = path.clone();
            cx.listener(move |workspace, _, _, cx| {
                workspace.on_open_workspace_folder(&path, cx);
            })
        })
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    Icon::new(IconName::Folder)
                        .xsmall()
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
                    let id = format!("workspace-root-remove-{index}");
                    let on_remove = cx.listener(move |workspace, _, _, cx| {
                        workspace.on_remove_workspace_root(&remove, cx);
                    });
                    if armed {
                        row.child(super::sidebar::armed_delete_button(
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
        )
}

/// The working page names its session, exact scope, and live working-tree evidence.
pub(super) fn render_page_band(
    workspace: &Workspace,
    window: &Window,
    cx: &Context<Workspace>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let vm = workspace.vm();
    let active_id = vm.active.as_ref().map(|active| active.session_id.as_str());
    let title = crate::view_model::session_rows(vm)
        .into_iter()
        .find(|(summary, _)| Some(summary.session_id.as_str()) == active_id)
        .map(|(summary, _)| summary.title.clone())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| t("Working page", "工作页").to_owned());
    let scope = vm
        .project_dir
        .clone()
        .unwrap_or_else(|| t("No folder selected", "尚未选择目录").to_owned());
    let scope = active_id
        .map(|id| format!("{} · {scope}", session_page_id(id)))
        .unwrap_or(scope);
    let git = workspace.git();
    let changes = if git.note.is_some() {
        t("Review changes", "查看改动").to_owned()
    } else {
        format!(
            "{} {}{}",
            t("Changes", "改动"),
            git.files.len(),
            if git.stopped_early { "+" } else { "" }
        )
    };
    let wide = f32::from(window.viewport_size().width) >= 1100.;
    div()
        .id("working-page-band")
        .w_full()
        .flex_shrink_0()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(theme.border)
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(skin::WEIGHT_STRONG)
                        .truncate()
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(scope),
                ),
        )
        .when(wide && !git.branch.is_empty(), |this| {
            this.child(
                div()
                    .max_w(px(180.))
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(git.branch.clone()),
            )
        })
        .child(
            skin::glass_button("workbench-changes", false, theme)
                .h(px(32.))
                .flex_shrink_0()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_toggle_changes_panel(true, cx);
                }))
                .child(
                    Icon::new(IconName::FileText)
                        .xsmall()
                        .text_color(if git.files.is_empty() {
                            theme.muted_foreground
                        } else {
                            theme.primary
                        }),
                )
                .child(changes),
        )
}
