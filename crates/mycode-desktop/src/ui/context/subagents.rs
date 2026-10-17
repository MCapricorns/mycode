//! Running subagents: the inspector list and the tall detail window.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::theme::Theme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};

use super::{SUBAGENT_WINDOW_BOTTOM_CLEAR, section_heading, subagent_window_bounds};
use crate::i18n::t;
use crate::workspace::Workspace;

pub(super) fn render_subagents(workspace: &Workspace, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let desk = crate::ui::desk::Desk::of(theme);
    let jobs = &workspace.vm().live_jobs;
    let capped = crate::view_model::capped_artisan_count(jobs);
    let summary = if jobs.len() == 1 {
        t("1 running", "1 个运行中").to_owned()
    } else {
        format!("{} {}", jobs.len(), t("running", "个运行中"))
    };
    let rows: Vec<(String, String, String, bool)> = jobs
        .iter()
        .map(|job| {
            // The step line already owns a row, so the running time rides
            // on it instead of adding a third line per card.
            (
                job.call_id.clone(),
                job.title(),
                format!(
                    "{} · {}",
                    job.step_label(),
                    crate::view_model::elapsed_label(job.elapsed_ms())
                ),
                job.capped,
            )
        })
        .collect();
    div()
        .id("subagents")
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(section_heading(t("Subagents", "子代理"), theme))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(summary),
                ),
        )
        .when(capped > 0, |this| {
            this.child(
                div()
                    .id("subagents-cap")
                    .text_xs()
                    .whitespace_normal()
                    .px_2()
                    .py_1()
                    .rounded(crate::ui::skin::radius_control())
                    .bg(theme.warning.opacity(0.18))
                    .text_color(theme.warning_foreground)
                    .child(crate::view_model::capped_artisan_status(capped)),
            )
        })
        .children(
            rows.into_iter()
                .enumerate()
                .map(|(index, (call_id, title, step, warned))| {
                    let cancel_id = call_id.clone();
                    div()
                        .id(format!("subagent-{index}"))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_1()
                        .py(px(4.))
                        .rounded(crate::ui::skin::radius_control())
                        .cursor_pointer()
                        .hover(|card| card.bg(crate::ui::skin::frost_hover(theme)))
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            workspace.on_open_subagent(&call_id, cx);
                        }))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .min_w_0()
                                .child(crate::ui::lamp(if warned {
                                    theme.warning
                                } else {
                                    desk.amber
                                }))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .truncate()
                                        .text_color(theme.foreground)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .id(format!("subagent-cancel-{index}"))
                                        .flex_shrink_0()
                                        .px_1()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .h(px(18.))
                                        .rounded(px(4.))
                                        .cursor_pointer()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .hover(|this| this.text_color(theme.danger))
                                        .on_click(cx.listener(move |workspace, _, _, cx| {
                                            cx.stop_propagation();
                                            workspace.on_cancel_subagent(&cancel_id, cx);
                                        }))
                                        .child(t("Cancel", "取消")),
                                ),
                        )
                        .child(
                            div()
                                .pl(px(15.))
                                .text_xs()
                                .truncate()
                                .text_color(if warned {
                                    theme.warning_foreground
                                } else {
                                    theme.muted_foreground
                                })
                                .child(step),
                        )
                }),
        )
}

fn window_label(text: &str, theme: &Theme) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(text.to_owned())
}

fn window_body(text: String) -> impl IntoElement {
    div().text_sm().whitespace_normal().child(text)
}

/// Tall window for one running subagent so its progress is readable. It
/// ends above the composer so the composer controls stay clickable.
pub(crate) fn render_subagent_window(
    workspace: &mut Workspace,
    window: &Window,
    inspector_span: f32,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let call_id = workspace.vm().subagent_window.clone().unwrap_or_default();
    let job = workspace
        .vm()
        .live_jobs
        .iter()
        .find(|job| job.call_id == call_id)
        .cloned();
    let title = job.as_ref().map(crate::view_model::LiveJob::title);
    let elapsed = job
        .as_ref()
        .map(|job| crate::view_model::elapsed_label(job.elapsed_ms()));
    let goal = job
        .as_ref()
        .map(|job| job.label.clone())
        .unwrap_or_default();
    let prompt = job
        .as_ref()
        .map(|job| job.prompt.clone())
        .unwrap_or_default();
    let path = job.as_ref().map(|job| job.path.clone()).unwrap_or_default();
    let now = job.as_ref().map(|job| job.step.clone()).unwrap_or_default();
    let log = job.map(|job| job.log).unwrap_or_default();
    let last = log.len().saturating_sub(1);
    let scroll = workspace.subagent_scroll_handle().clone();
    let viewport_width = f32::from(window.viewport_size().width);
    let (window_right, window_width) = subagent_window_bounds(
        viewport_width,
        inspector_span,
        workspace.vm().changes_panel_open,
        workspace.git_diff_panel_open(),
    );
    div()
        .id("subagent-window-layer")
        .occlude()
        .absolute()
        .top(px(crate::ui::title_bar::BAR_HEIGHT + 8.))
        .right(px(window_right))
        .bottom(px(SUBAGENT_WINDOW_BOTTOM_CLEAR))
        .min_h(px(200.))
        .w(px(window_width))
        .flex()
        .flex_col()
        .rounded(crate::ui::skin::radius_card())
        .border_1()
        .border_color(crate::ui::skin::border_strong(theme))
        .bg(crate::ui::skin::popover(theme))
        .shadow(crate::ui::skin::elevation_shadow(theme))
        .overflow_hidden()
        .child(
            div()
                .px_3()
                .py_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .truncate()
                        .child(title.unwrap_or_else(|| t("Subagent", "子代理").to_owned())),
                )
                .when_some(elapsed, |row, elapsed| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(elapsed),
                    )
                })
                .child(
                    div()
                        .id("subagent-window-close")
                        .px_2()
                        .py(px(2.))
                        .rounded(px(6.))
                        .text_xs()
                        .cursor_pointer()
                        .text_color(theme.muted_foreground)
                        .hover(|this| this.bg(theme.secondary_hover))
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            cx.stop_propagation();
                            workspace.on_open_subagent("", cx);
                        }))
                        .child(t("Close", "关闭")),
                ),
        )
        .child(
            div()
                .id("subagent-window-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(window_label(t("Goal", "目标"), theme))
                .child(window_body(if goal.is_empty() {
                    t("No short goal yet.", "暂无简短目标。").to_owned()
                } else {
                    goal
                }))
                .child(window_label(t("Task", "任务"), theme))
                .child(window_body(if prompt.is_empty() {
                    t("Waiting for the task brief.", "等待任务简报。").to_owned()
                } else {
                    prompt
                }))
                .when(!path.is_empty(), |this| {
                    this.child(window_label(t("Path", "路径"), theme))
                        .child(window_body(path))
                })
                .child(window_label(&format!("{}  {now}", t("Now", "当前")), theme))
                .children(log.into_iter().enumerate().map(|(index, line)| {
                    let current = index == last;
                    div()
                        .id(format!("subagent-log-{index}"))
                        .text_xs()
                        .text_color(if current {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(line)
                })),
        )
        .into_any_element()
}
