//! Session agent inspector: compact run rows and result-first details.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use mycode::tools::builtin::{AgentRunSnapshot, AgentRunStatus};

use super::{SUBAGENT_WINDOW_BOTTOM_CLEAR, subagent_window_bounds};
use crate::i18n::t;
use crate::view_model::{
    FrontendAction, WorkspaceState, agent_run_elapsed_ms, agent_run_status_label, elapsed_label,
};
use crate::workspace::Workspace;

#[cfg(test)]
mod controls_tests;
#[cfg(all(test, feature = "test-support"))]
mod layout_tests;

fn status_color(status: AgentRunStatus, theme: &Theme) -> gpui_kit::Hsla {
    match status {
        AgentRunStatus::Cancelled => theme.muted_foreground,
        AgentRunStatus::Running => theme.info,
        AgentRunStatus::Completed => theme.success,
        AgentRunStatus::Failed => theme.danger,
        AgentRunStatus::Queued | AgentRunStatus::Settling | AgentRunStatus::Blocked => {
            theme.warning
        }
    }
}

#[derive(Clone)]
struct AgentRow {
    id: String,
    title: String,
    role: String,
    status: AgentRunStatus,
    elapsed: String,
    activity: String,
    cancel_id: Option<String>,
}

fn can_stop(run: &AgentRunSnapshot) -> bool {
    matches!(run.status, AgentRunStatus::Queued | AgentRunStatus::Running)
        && !run.call_id.is_empty()
}

fn run_title(run: &AgentRunSnapshot) -> &str {
    if run.description.is_empty() {
        &run.role
    } else {
        &run.description
    }
}

fn run_row(run: &AgentRunSnapshot) -> AgentRow {
    AgentRow {
        id: run.id.clone(),
        title: run_title(run).to_owned(),
        role: run.role.clone(),
        status: run.status,
        elapsed: elapsed_label(agent_run_elapsed_ms(run)),
        activity: run
            .error
            .as_deref()
            .or(run.result.as_deref())
            .unwrap_or_else(|| {
                if run.status.is_terminal() {
                    ""
                } else {
                    &run.step
                }
            })
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned(),
        cancel_id: can_stop(run).then(|| run.id.clone()),
    }
}

fn rows(state: &WorkspaceState) -> Vec<AgentRow> {
    let mut rows = Vec::new();
    if let Some(session) = state.subagent_session() {
        rows.extend(session.runs.iter().rev().map(run_row));
    }
    rows.extend(
        state
            .live_jobs
            .iter()
            .rev()
            .filter(|job| {
                !state.subagent_session().is_some_and(|session| {
                    session.runs.iter().any(|run| {
                        run.call_id == job.call_id
                            || (!job.provider_call_id.is_empty()
                                && run.call_id == job.provider_call_id)
                    })
                })
            })
            .map(|job| AgentRow {
                id: job.call_id.clone(),
                title: if job.label.is_empty() {
                    job.title()
                } else {
                    job.label.clone()
                },
                role: job.role.clone(),
                status: if job.step == "queued" {
                    AgentRunStatus::Queued
                } else {
                    AgentRunStatus::Running
                },
                elapsed: elapsed_label(job.elapsed_ms()),
                activity: job.step_label(),
                cancel_id: (!job.call_id.is_empty()).then(|| job.call_id.clone()),
            }),
    );
    rows.sort_by_key(|row| row.status.is_terminal());
    rows
}

fn status_badge(status: AgentRunStatus, theme: &Theme) -> Div {
    let ink = status_color(status, theme);
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded(crate::ui::skin::radius_xs())
        .bg(ink.opacity(0.10))
        .text_color(ink)
        .text_xs()
        .child(crate::ui::lamp(ink))
        .child(agent_run_status_label(status))
}

fn row_content(row: &AgentRow, theme: &Theme) -> Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Icon::new(IconName::Bot)
                        .small()
                        .flex_shrink_0()
                        .text_color(theme.primary),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .truncate()
                        .font_weight(crate::ui::skin::WEIGHT_STRONG)
                        .text_color(theme.foreground)
                        .child(row.title.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(row.elapsed.clone()),
                ),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(status_badge(row.status, theme))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(row.role.clone()),
                ),
        )
        .when(!row.activity.is_empty(), |this| {
            this.child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_xs()
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .child(row.activity.clone()),
            )
        })
}

pub(super) fn render_subagents(workspace: &Workspace, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let rows = rows(workspace.vm());
    let active = rows
        .iter()
        .take_while(|row| !row.status.is_terminal())
        .count();
    let selected = workspace.vm().subagent_window.as_deref().map(|id| {
        workspace
            .vm()
            .subagent_session()
            .and_then(|session| session.run(id))
            .map_or(id, |run| run.id.as_str())
    });
    div()
        .id("subagents")
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .font_weight(crate::ui::skin::WEIGHT_STRONG)
                        .child(t("Subagents", "子代理")),
                )
                .when(active > 0, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.info)
                            .child(format!("{} {active}", t("Active", "进行中"))),
                    )
                }),
        )
        .when(rows.is_empty(), |this| {
            this.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded(crate::ui::skin::radius_sm())
                    .bg(theme.status_bar)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t(
                        "Delegated tasks will appear here.",
                        "派遣的任务将在这里显示。",
                    )),
            )
        })
        .child(
            div()
                .id("subagents-list")
                .max_h(px(360.))
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_2()
                .children(rows.into_iter().map(|row| {
                    let id = row.id.clone();
                    let is_selected = selected == Some(id.as_str());
                    div()
                        .flex()
                        .flex_row()
                        .items_start()
                        .min_w_0()
                        .flex_shrink_0()
                        .gap_1()
                        .rounded(crate::ui::skin::radius_sm())
                        .border_1()
                        .border_color(if is_selected {
                            theme.primary.opacity(0.65)
                        } else {
                            theme.border
                        })
                        .bg(if is_selected {
                            theme.accent
                        } else {
                            theme.status_bar
                        })
                        .child(
                            Button::new(format!("subagent-open-{id}"))
                                .ghost()
                                .h_auto()
                                .flex_1()
                                .min_w_0()
                                .p_2()
                                .rounded(crate::ui::skin::radius_sm())
                                .accessibility_label(format!(
                                    "{}: {}",
                                    t("Open details", "打开详情"),
                                    row.title
                                ))
                                .tooltip(t("Open run details", "打开运行详情"))
                                .child(row_content(&row, theme))
                                .on_click(cx.listener(move |workspace, _, _, cx| {
                                    workspace.on_open_subagent(&id, cx);
                                })),
                        )
                        .when_some(row.cancel_id, |this, cancel_id| {
                            this.child(
                                Button::new(format!("subagent-stop-{cancel_id}"))
                                    .icon(IconName::Square)
                                    .accessibility_label(t(
                                        "Stop this child run",
                                        "停止此子代理运行",
                                    ))
                                    .small()
                                    .danger()
                                    .outline()
                                    .mt_2()
                                    .mr_2()
                                    .tooltip(t("Stop this child run", "停止此子代理运行"))
                                    .on_click(cx.listener(move |workspace, _, _, cx| {
                                        workspace.on_cancel_subagent(&cancel_id, cx);
                                    })),
                            )
                        })
                })),
        )
}

// GPUI wraps at whitespace; add break opportunities inside long machine tokens.
// Copy controls always use the unmodified source text.
fn wrap_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut token = 0;
    for ch in text.chars() {
        out.push(ch);
        if ch.is_whitespace() {
            token = 0;
        } else {
            token += 1;
        }
        if matches!(ch, '/' | '\\') || token >= 32 {
            out.push('\u{200b}');
            token = 0;
        }
    }
    out
}

fn detail_text(id: &str, text: &str) -> gpui_kit::Stateful<Div> {
    div()
        .id(id.to_owned())
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .text_sm()
        .whitespace_normal()
        .child(wrap_text(text))
}

fn detail_section(id: &str, title: &str, text: &str, theme: &Theme) -> gpui_kit::Stateful<Div> {
    div()
        .id(id.to_owned())
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_weight(crate::ui::skin::WEIGHT_STRONG)
                .text_color(theme.muted_foreground)
                .child(title.to_owned()),
        )
        .child(detail_text(&format!("{id}-text"), text))
}

fn result_section(id: &str, title: &str, text: &str, theme: &Theme, window: &Window) -> Div {
    div()
        .w_full()
        .min_w_0()
        .flex_none()
        .flex()
        .flex_col()
        .gap_2()
        .text_sm()
        .child(
            div()
                .text_sm()
                .font_weight(crate::ui::skin::WEIGHT_STRONG)
                .child(title.to_owned()),
        )
        .child(crate::ui::chat::render_detail_markdown(
            id, text, theme, window,
        ))
}

fn copy_button(id: &str, label: &str, text: String, cx: &Context<Workspace>) -> Button {
    Button::new(id.to_owned())
        .label(label.to_owned())
        .small()
        .ghost()
        .tooltip(label.to_owned())
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.on_copy_plain_text(text.clone(), cx);
        }))
}

fn optional_section(
    id: &str,
    title: &str,
    text: &str,
    workspace: &Workspace,
    cx: &Context<Workspace>,
) -> AnyElement {
    let open = workspace.vm().subagent_sections.contains(id);
    let section_id = id.to_owned();
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .gap_2()
        .flex_shrink_0()
        .child(
            Button::new(format!("agent-toggle-{id}"))
                .label(format!(
                    "{} {}",
                    if open {
                        t("Hide", "收起")
                    } else {
                        t("Show", "展开")
                    },
                    title
                ))
                .small()
                .ghost()
                .tooltip(title.to_owned())
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    workspace.apply_action(
                        FrontendAction::SubagentSectionToggled(section_id.clone()),
                        cx,
                    );
                })),
        )
        .when(open, |this| this.child(detail_text(id, text)))
        .into_any_element()
}

fn run_detail(
    run: &AgentRunSnapshot,
    workspace: &Workspace,
    window: &Window,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    let row = run_row(run);
    let log = workspace
        .vm()
        .subagent_session()
        .and_then(|session| session.activity.iter().find(|(id, _)| id == &run.id))
        .map(|(_, lines)| lines.join("\n"))
        .unwrap_or_default();
    div()
        .flex()
        .flex_col()
        .gap_4()
        .min_w_0()
        .flex_shrink_0()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(status_badge(row.status, theme))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{} · {}", run.role, row.elapsed)),
                )
                .when_some(row.cancel_id, |this, cancel_id| {
                    this.child(
                        Button::new("agent-detail-stop")
                            .label(t("Stop", "停止"))
                            .small()
                            .outline()
                            .tooltip(t("Stop this child run", "停止此子代理运行"))
                            .on_click(cx.listener(move |workspace, _, _, cx| {
                                workspace.on_cancel_subagent(&cancel_id, cx);
                            })),
                    )
                }),
        )
        .when(!run.step.is_empty() && !run.status.is_terminal(), |this| {
            this.child(detail_section(
                "agent-current",
                t("Latest activity", "最新活动"),
                &run.step,
                theme,
            ))
        })
        .when_some(run.error.clone(), |this, error| {
            this.child(
                detail_section("agent-error", t("Error", "错误"), &error, theme)
                    .text_color(theme.danger),
            )
        })
        .when_some(run.result.clone(), |this, result| {
            this.child(result_section(
                "agent-result",
                t("Result", "结果"),
                &result,
                theme,
                window,
            ))
            .child(copy_button(
                "agent-copy-result",
                t("Copy result", "复制结果"),
                result,
                cx,
            ))
        })
        .when(
            run.status.is_terminal() && run.result.is_none() && run.error.is_none(),
            |this| {
                this.child(detail_text(
                    "agent-no-result",
                    t(
                        "No result was captured for this run.",
                        "此次运行未捕获结果。",
                    ),
                ))
            },
        )
        .when_some(run.artifact_path.clone(), |this, path| {
            this.child(detail_section(
                "agent-artifact",
                t("Full result artifact", "完整结果文件"),
                &path,
                theme,
            ))
            .child(copy_button(
                "agent-copy-artifact",
                t("Copy artifact path", "复制结果文件路径"),
                path,
                cx,
            ))
        })
        .when(!run.path.is_empty(), |this| {
            this.child(detail_section(
                "agent-path",
                t("Working directory", "工作目录"),
                &run.path,
                theme,
            ))
            .child(copy_button(
                "agent-copy-path",
                t("Copy working directory", "复制工作目录"),
                run.path.clone(),
                cx,
            ))
        })
        .when(!run.prompt.is_empty(), |this| {
            this.child(optional_section(
                "agent-prompt",
                t("Task brief", "任务简报"),
                &run.prompt,
                workspace,
                cx,
            ))
        })
        .when(!log.is_empty(), |this| {
            this.child(optional_section(
                "agent-activity",
                t("Activity history", "活动记录"),
                &log,
                workspace,
                cx,
            ))
        })
        .child(optional_section(
            "agent-identity",
            t("Run identity", "运行标识"),
            &format!("{}\n{}", run.id, run.call_id),
            workspace,
            cx,
        ))
        .into_any_element()
}

/// Scrollable detail that stays open after a run reaches its terminal state.
pub(crate) fn render_subagent_window(
    workspace: &mut Workspace,
    window: &Window,
    inspector_span: f32,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let selected = workspace.vm().subagent_window.clone().unwrap_or_default();
    let session = workspace.vm().subagent_session();
    let run = session.and_then(|session| session.run(&selected));
    let legacy = workspace
        .vm()
        .live_jobs
        .iter()
        .find(|job| job.call_id == selected);
    let title = run
        .map(|run| run_title(run).to_owned())
        .or_else(|| legacy.map(|job| job.title()))
        .unwrap_or_else(|| t("Subagent", "子代理").to_owned());
    let content = if let Some(run) = run {
        run_detail(run, workspace, window, cx)
    } else if let Some(job) = legacy {
        let run = AgentRunSnapshot {
            id: job.call_id.clone(),
            call_id: if job.provider_call_id.is_empty() {
                job.call_id.clone()
            } else {
                job.provider_call_id.clone()
            },
            role: job.role.clone(),
            description: job.label.clone(),
            prompt: job.prompt.clone(),
            status: if job.step == "queued" {
                AgentRunStatus::Queued
            } else {
                AgentRunStatus::Running
            },
            step: job.step_label(),
            path: job.path.clone(),
            started_ms: job.started_ms,
            finished_ms: None,
            result: None,
            error: None,
            artifact_path: None,
        };
        run_detail(&run, workspace, window, cx)
    } else {
        detail_text(
            "agent-expired",
            t(
                "This record is no longer in this session's recent history.",
                "此记录已不在当前会话的近期历史中。",
            ),
        )
        .into_any_element()
    };
    let theme = cx.theme();
    let viewport_width = f32::from(window.viewport_size().width);
    let viewport_height = f32::from(window.viewport_size().height);
    let (right, width) = subagent_window_bounds(
        viewport_width,
        inspector_span,
        workspace.vm().changes_panel_open,
        workspace.git_diff_panel_open(),
    );
    let top = crate::ui::title_bar::BAR_HEIGHT + 8.;
    let bottom = detail_bottom_inset(viewport_height, top);
    let scroll = workspace.subagent_scroll_handle().clone();
    div()
        .id("subagent-window-layer")
        .occlude()
        .absolute()
        .top(px(top))
        .right(px(right))
        .bottom(px(bottom))
        .w(px(width))
        .flex()
        .flex_col()
        .rounded(crate::ui::skin::radius_card())
        .border_1()
        .border_color(theme.border)
        .bg(crate::ui::skin::panel_fill(theme))
        .shadow(crate::ui::skin::elevation_shadow(theme))
        .overflow_hidden()
        .child(
            div()
                .flex_shrink_0()
                .px_4()
                .py_3()
                .flex()
                .items_center()
                .gap_3()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.title_bar)
                .child(Icon::new(IconName::Bot).small().text_color(theme.primary))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .font_weight(crate::ui::skin::WEIGHT_STRONG)
                        .whitespace_normal()
                        .child(title),
                )
                .child(crate::ui::icon_button(
                    "subagent-window-close",
                    IconName::X,
                    cx.listener(|workspace, _, _, cx| {
                        workspace.on_open_subagent("", cx);
                    }),
                    cx,
                )),
        )
        .child(
            div()
                .id("subagent-window-body")
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .p_4()
                .flex()
                .flex_col()
                .gap_4()
                .child(content),
        )
        .into_any_element()
}

fn detail_bottom_inset(height: f32, top: f32) -> f32 {
    SUBAGENT_WINDOW_BOTTOM_CLEAR.min((height - top - 180.).max(8.))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_machine_tokens_have_break_opportunities_without_changing_copy_source() {
        let source = format!("C:\\workspace\\{}\\result.md", "long_segment".repeat(20));
        let wrapped = wrap_text(&source);
        assert_eq!(wrapped.replace('\u{200B}', ""), source);
        assert!(
            wrapped
                .split('\u{200B}')
                .all(|part| part.chars().count() <= 32)
        );
    }
}
