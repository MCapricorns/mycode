//! Conversation header: its title, working directory, and change review.
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};

use super::skin;
use crate::i18n::t;
use crate::workspace::Workspace;

pub(super) const HEADER_HEIGHT: f32 = 72.;

/// The conversation owns its title; its project is secondary context.
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
        .unwrap_or_else(|| t("New conversation", "新对话").to_owned());
    let scope = vm
        .project_dir
        .clone()
        .unwrap_or_else(|| t("Choose a project to get started", "选择项目后开始对话").to_owned());
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
        .id("conversation-header")
        .w_full()
        .min_h(px(HEADER_HEIGHT))
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
                    .max_w(px(160.))
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(git.branch.clone()),
            )
        })
        .when(vm.project_dir.is_some(), |this| {
            this.child(
                skin::glass_button("workbench-changes", false, theme)
                    .h(px(32.))
                    .flex_shrink_0()
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_toggle_changes_panel(true, cx);
                    }))
                    .child(Icon::new(IconName::FileText).xsmall())
                    .child(changes),
            )
        })
}
