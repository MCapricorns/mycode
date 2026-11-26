//! Conversation header: its title, working directory, and change review.
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px, relative,
};

use super::skin;
use crate::i18n::t;
use crate::workspace::Workspace;

pub(super) const HEADER_HEIGHT: f32 = 60.;

/// The conversation owns its title; its project is secondary context.
pub(super) fn render_page_band(
    workspace: &Workspace,
    window: &Window,
    cx: &Context<Workspace>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let vm = workspace.vm();
    let active_id = vm.active.as_ref().map(|active| active.session_id.as_str());
    let title = vm
        .sessions
        .iter()
        .find(|summary| Some(summary.session_id.as_str()) == active_id)
        .map(|summary| summary.title.clone())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| t("New conversation", "新对话").to_owned());
    let scope = vm
        .project_dir
        .clone()
        .unwrap_or_else(|| t("Default workspace", "默认工作区").to_owned());
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
        .px_5()
        .py_2()
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.background)
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
                .gap(px(2.))
                .child(
                    div()
                        .text_sm()
                        .line_height(relative(1.2))
                        .font_weight(skin::WEIGHT_STRONG)
                        .truncate()
                        .child(title),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .min_w_0()
                        .text_xs()
                        .line_height(relative(1.2))
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(IconName::Folder).xsmall().flex_shrink_0())
                        .child(div().min_w_0().truncate().child(scope)),
                ),
        )
        .when(wide && !git.branch.is_empty(), |this| {
            this.child(
                div()
                    .max_w(px(180.))
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::GitBranch).xsmall().flex_shrink_0())
                    .child(div().min_w_0().truncate().child(git.branch.clone())),
            )
        })
        .when(vm.project_dir.is_some(), |this| {
            let review_open = vm.changes_panel_open;
            this.child(
                div()
                    .id("workbench-changes")
                    .h(px(30.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .flex_shrink_0()
                    .rounded(skin::radius_xs())
                    .text_xs()
                    .font_weight(skin::WEIGHT_STRONG)
                    .cursor_pointer()
                    .when(review_open, |button| {
                        button.bg(theme.accent).text_color(theme.accent_foreground)
                    })
                    .hover(move |button| {
                        button.bg(if review_open {
                            theme.accent
                        } else {
                            theme.secondary_hover
                        })
                    })
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_toggle_changes_panel(true, cx);
                    }))
                    .child(Icon::new(IconName::FileText).xsmall())
                    .child(changes),
            )
        })
}
