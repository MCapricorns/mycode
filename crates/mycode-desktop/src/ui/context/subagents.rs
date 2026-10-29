//! Session agent inspector: compact run/workflow rows and result-first details.
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use mycode_frontend::builtin::{
    AgentRunSnapshot, AgentRunStatus, AgentWorkflowSnapshot, AgentWorkflowStatus,
};

use super::{SUBAGENT_WINDOW_BOTTOM_CLEAR, subagent_window_bounds};
use crate::i18n::t;
use crate::view_model::{
    FrontendAction, SubagentSession, WorkspaceState, agent_run_elapsed_ms, agent_run_status_label,
    agent_workflow_status_label, elapsed_label,
};
use crate::workspace::Workspace;

#[cfg(test)]
mod controls_tests;
#[cfg(all(test, feature = "test-support"))]
mod layout_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusTone {
    Neutral,
    Active,
    Success,
    Warning,
    Error,
}

fn run_tone(status: AgentRunStatus) -> StatusTone {
    match status {
        AgentRunStatus::Queued | AgentRunStatus::Cancelled => StatusTone::Neutral,
        AgentRunStatus::Running | AgentRunStatus::Settling => StatusTone::Active,
        AgentRunStatus::Completed => StatusTone::Success,
        AgentRunStatus::Failed => StatusTone::Error,
        AgentRunStatus::Blocked => StatusTone::Warning,
    }
}

fn workflow_tone(status: AgentWorkflowStatus) -> StatusTone {
    match status {
        AgentWorkflowStatus::Queued | AgentWorkflowStatus::Cancelled => StatusTone::Neutral,
        AgentWorkflowStatus::Running => StatusTone::Active,
        AgentWorkflowStatus::WaitingIntegration => StatusTone::Warning,
        AgentWorkflowStatus::Completed => StatusTone::Success,
        AgentWorkflowStatus::Failed => StatusTone::Error,
    }
}

fn tone_color(tone: StatusTone, theme: &Theme) -> gpui_kit::Hsla {
    match tone {
        StatusTone::Neutral => theme.muted_foreground,
        StatusTone::Active => theme.foreground,
        StatusTone::Success => crate::ui::desk::Desk::of(theme).green,
        StatusTone::Warning => theme.warning,
        StatusTone::Error => theme.danger,
    }
}

#[derive(Clone)]
struct AgentRow {
    id: String,
    title: String,
    role: String,
    status: String,
    tone: StatusTone,
    elapsed: String,
    activity: String,
    cancel_id: Option<String>,
}

fn can_stop(run: &AgentRunSnapshot) -> bool {
    matches!(run.status, AgentRunStatus::Queued | AgentRunStatus::Running)
        && !run.call_id.is_empty()
}

fn run_row(run: &AgentRunSnapshot) -> AgentRow {
    AgentRow {
        id: run.id.clone(),
        title: if run.description.is_empty() {
            run.role.clone()
        } else {
            run.description.clone()
        },
        role: run.role.clone(),
        status: agent_run_status_label(run.status).to_owned(),
        tone: run_tone(run.status),
        elapsed: elapsed_label(agent_run_elapsed_ms(run)),
        activity: run
            .error
            .as_deref()
            .or(run.result.as_deref())
            .unwrap_or(&run.step)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned(),
        cancel_id: can_stop(run).then(|| run.call_id.clone()),
    }
}

fn workflow_has_current_run(workflow: &AgentWorkflowSnapshot, run: &AgentRunSnapshot) -> bool {
    workflow
        .nodes
        .iter()
        .any(|node| node.run_id.as_deref() == Some(run.id.as_str()))
}

fn previous_workflow_runs<'a>(
    workflow: &AgentWorkflowSnapshot,
    session: &'a SubagentSession,
) -> Vec<&'a AgentRunSnapshot> {
    session
        .runs
        .iter()
        .rev()
        .filter(|run| {
            run.workflow_id.as_deref() == Some(workflow.id.as_str())
                && !workflow_has_current_run(workflow, run)
        })
        .collect()
}

fn workflow_status_text(workflow: &AgentWorkflowSnapshot) -> String {
    let status = agent_workflow_status_label(workflow.status);
    if workflow.revision == 0 {
        status.to_owned()
    } else {
        format!("{status} · {} {}", t("Rework", "返工"), workflow.revision)
    }
}

fn previous_result_button(run: &AgentRunSnapshot, theme: &Theme) -> Button {
    let row = run_row(run);
    Button::new(format!("workflow-previous-{}", run.id))
        .ghost()
        .h_auto()
        .w_full()
        .min_w_0()
        .px_2()
        .py_2()
        .accessibility_label(format!(
            "{}: {}",
            t("View previous result", "查看以往结果"),
            row.title
        ))
        .tooltip(t("View previous run and result", "查看以往运行与结果"))
        .child(row_content(&row, theme))
}

fn rows(state: &WorkspaceState) -> Vec<AgentRow> {
    let mut rows = Vec::new();
    if let Some(session) = state.subagent_session() {
        rows.extend(session.workflows.iter().rev().map(|workflow| {
            let runs: Vec<_> = session
                .runs
                .iter()
                .filter(|run| {
                    run.workflow_id.as_deref() == Some(&workflow.id)
                        && workflow_has_current_run(workflow, run)
                })
                .collect();
            let active = runs.iter().filter(|run| !run.status.is_terminal()).count();
            let latest = runs
                .iter()
                .rev()
                .find(|run| !run.status.is_terminal())
                .or_else(|| runs.last());
            let started = runs
                .iter()
                .map(|run| run.started_ms)
                .filter(|ms| *ms > 0)
                .min();
            let finished = if runs.iter().any(|run| !run.status.is_terminal()) {
                None
            } else {
                runs.iter().filter_map(|run| run.finished_ms).max()
            };
            let elapsed = started
                .map(|started| {
                    elapsed_label(
                        finished
                            .unwrap_or_else(|| crate::unix_now().as_millis() as u64)
                            .saturating_sub(started),
                    )
                })
                .unwrap_or_else(|| "—".to_owned());
            AgentRow {
                id: workflow.id.clone(),
                title: workflow.name.clone(),
                role: t("Workflow", "工作流").to_owned(),
                status: workflow_status_text(workflow),
                tone: workflow_tone(workflow.status),
                elapsed,
                activity: if workflow.status == AgentWorkflowStatus::WaitingIntegration {
                    t(
                        "Integrate worktrees, then request acceptance",
                        "整合 worktree 后再请求验收",
                    )
                    .to_owned()
                } else if active > 0 {
                    latest.map(|run| run.step.clone()).unwrap_or_default()
                } else {
                    workflow
                        .result
                        .as_deref()
                        .or(workflow.acceptance.as_deref())
                        .unwrap_or("")
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                },
                cancel_id: None,
            }
        }));
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
                status: if job.capped || job.step == "queued" {
                    t("Queued", "排队中")
                } else {
                    t("Running", "运行中")
                }
                .to_owned(),
                tone: if job.capped {
                    StatusTone::Warning
                } else {
                    StatusTone::Active
                },
                elapsed: elapsed_label(job.elapsed_ms()),
                activity: job.step_label(),
                cancel_id: (!job.call_id.is_empty()).then(|| job.call_id.clone()),
            }),
    );
    rows
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
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .truncate()
                        .text_color(theme.foreground)
                        .child(row.title.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child(row.elapsed.clone()),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(row.role.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(tone_color(row.tone, theme))
                        .child(row.status.clone()),
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
    let count = rows.len();
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
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(count.to_string()),
                ),
        )
        .child(
            div()
                .text_xs()
                .whitespace_normal()
                .text_color(theme.muted_foreground)
                .child(t(
                    "Recent runs in this session · this app process",
                    "此会话的近期运行 · 当前应用进程",
                )),
        )
        .child(
            div()
                .id("subagents-list")
                .max_h(px(360.))
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_2()
                .children(rows.into_iter().enumerate().map(|(index, row)| {
                    let id = row.id.clone();
                    let is_selected = selected == Some(id.as_str());
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .flex_shrink_0()
                        .gap_1()
                        .child(
                            Button::new(format!("subagent-open-{index}"))
                                .ghost()
                                .h_auto()
                                .w_full()
                                .min_w_0()
                                .px_2()
                                .py_2()
                                .selected(is_selected)
                                .accessibility_label(format!(
                                    "{}: {}",
                                    t("Open details", "打开详情"),
                                    row.title
                                ))
                                .tooltip(t("Open run or workflow details", "打开运行或工作流详情"))
                                .child(row_content(&row, theme))
                                .on_click(cx.listener(move |workspace, _, _, cx| {
                                    workspace.on_open_subagent(&id, cx);
                                })),
                        )
                        .when_some(row.cancel_id, |this, cancel_id| {
                            this.child(
                                Button::new(format!("subagent-stop-{index}"))
                                    .label(t("Stop", "停止"))
                                    .small()
                                    .ghost()
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
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    let row = run_row(run);
    let log = workspace
        .vm()
        .subagent_session()
        .and_then(|session| session.activity.iter().find(|(id, _)| id == &run.id))
        .map(|(_, lines)| {
            lines.join(
                "
",
            )
        })
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
                .child(
                    div()
                        .text_sm()
                        .text_color(tone_color(row.tone, theme))
                        .child(row.status),
                )
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
            this.child(detail_section(
                "agent-result",
                t("Result", "结果"),
                &result,
                theme,
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
        .when_some(run.workflow_id.clone(), |this, workflow_id| {
            this.child(
                Button::new("agent-parent-workflow")
                    .label(t("View workflow", "查看工作流"))
                    .small()
                    .ghost()
                    .tooltip(t(
                        "Show dependencies and independent acceptance",
                        "查看依赖关系与独立验收",
                    ))
                    .on_click(cx.listener(move |workspace, _, _, cx| {
                        workspace.on_open_subagent(&workflow_id, cx);
                    })),
            )
        })
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
            &format!(
                "{}
{}",
                run.id, run.call_id
            ),
            workspace,
            cx,
        ))
        .into_any_element()
}

fn acceptance_text(workflow: &AgentWorkflowSnapshot) -> String {
    if workflow.status == AgentWorkflowStatus::WaitingIntegration {
        return t("Not accepted. Integrate the retained worktrees into the main workspace, then explicitly request the integration gate and independent acceptance.", "尚未验收。请先将保留的 worktree 整合到主工作区，再明确请求运行集成检查与独立验收。").to_owned();
    }
    let node = workflow
        .nodes
        .iter()
        .find(|node| node.id == "workflow-acceptance");
    match (workflow.acceptance.as_deref(), node) {
        (Some(verdict), Some(node)) => {
            format!("{} · {verdict}", agent_run_status_label(node.status))
        }
        (Some(verdict), None) => verdict.to_owned(),
        (None, Some(node)) => agent_run_status_label(node.status).to_owned(),
        (None, None) => t(
            "No independent acceptance result yet.",
            "暂无独立验收结果。",
        )
        .to_owned(),
    }
}

fn workflow_detail(
    workflow: &AgentWorkflowSnapshot,
    workspace: &Workspace,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    let previous = workspace
        .vm()
        .subagent_session()
        .map(|session| previous_workflow_runs(workflow, session))
        .unwrap_or_default();
    div()
        .flex()
        .flex_col()
        .gap_4()
        .min_w_0()
        .flex_shrink_0()
        .child(
            div()
                .text_sm()
                .text_color(tone_color(workflow_tone(workflow.status), theme))
                .child(workflow_status_text(workflow)),
        )
        .child(
            detail_section(
                "workflow-acceptance",
                t("Independent acceptance", "独立验收"),
                &acceptance_text(workflow),
                theme,
            )
            .when(
                workflow.status == AgentWorkflowStatus::WaitingIntegration,
                |this| this.text_color(theme.warning),
            ),
        )
        .when_some(workflow.result.clone(), |this, result| {
            this.child(detail_section(
                "workflow-result",
                t("Result / handoff", "结果 / 交接"),
                &result,
                theme,
            ))
            .child(copy_button(
                "workflow-copy-result",
                t("Copy result", "复制结果"),
                result,
                cx,
            ))
        })
        .child(
            div()
                .text_sm()
                .font_weight(crate::ui::skin::WEIGHT_STRONG)
                .child(t("Nodes and dependencies", "节点与依赖")),
        )
        .children(workflow.nodes.iter().map(|node| {
            let run = node.run_id.as_ref().and_then(|id| {
                workspace
                    .vm()
                    .subagent_session()?
                    .runs
                    .iter()
                    .find(|run| &run.id == id)
            });
            let deps = if node.dependencies.is_empty() {
                t("No dependencies", "无依赖").to_owned()
            } else {
                format!("{}: {}", t("After", "依赖"), node.dependencies.join(", "))
            };
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .gap_2()
                .flex_shrink_0()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .items_center()
                        .child(div().text_sm().child(node.id.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(node.role.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(tone_color(run_tone(node.status), theme))
                                .child(agent_run_status_label(node.status)),
                        ),
                )
                .child(detail_text(
                    &format!("node-{}-description", node.id),
                    &node.description,
                ))
                .child(
                    detail_text(&format!("node-{}-dependencies", node.id), &deps)
                        .text_color(theme.muted_foreground),
                )
                .when_some(run, |this, run| {
                    let run_id = run.id.clone();
                    this.child(
                        Button::new(format!("node-{}-details", node.id))
                            .label(if node.id == "workflow-acceptance" {
                                t("View acceptance run", "查看验收运行")
                            } else {
                                t("View run and result", "查看运行与结果")
                            })
                            .small()
                            .ghost()
                            .tooltip(run.description.clone())
                            .on_click(cx.listener(move |workspace, _, _, cx| {
                                workspace.on_open_subagent(&run_id, cx);
                            })),
                    )
                })
        }))
        .when(!previous.is_empty(), |this| {
            this.child(
                div()
                    .id("workflow-previous-results")
                    .flex()
                    .flex_col()
                    .gap_2()
                    .min_w_0()
                    .flex_shrink_0()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(crate::ui::skin::WEIGHT_STRONG)
                            .child(t("Previous results", "以往结果")),
                    )
                    .children(previous.into_iter().map(|run| {
                        let run_id = run.id.clone();
                        previous_result_button(run, theme).on_click(cx.listener(
                            move |workspace, _, _, cx| {
                                workspace.on_open_subagent(&run_id, cx);
                            },
                        ))
                    })),
            )
        })
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
    let run = session.and_then(|session| session.run(&selected)).cloned();
    let workflow = session
        .and_then(|session| {
            session
                .workflows
                .iter()
                .find(|workflow| workflow.id == selected)
        })
        .cloned();
    let legacy = workspace
        .vm()
        .live_jobs
        .iter()
        .find(|job| job.call_id == selected)
        .cloned();
    let title = run
        .as_ref()
        .map(|run| run.description.clone())
        .or_else(|| workflow.as_ref().map(|workflow| workflow.name.clone()))
        .or_else(|| legacy.as_ref().map(|job| job.title()))
        .unwrap_or_else(|| t("Subagent", "子代理").to_owned());
    let content = if let Some(run) = &run {
        run_detail(run, workspace, cx)
    } else if let Some(workflow) = &workflow {
        workflow_detail(workflow, workspace, cx)
    } else if let Some(job) = &legacy {
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
            status: AgentRunStatus::Running,
            step: job.step_label(),
            path: job.path.clone(),
            started_ms: job.started_ms,
            finished_ms: None,
            result: None,
            error: None,
            artifact_path: None,
            workflow_id: None,
            node_id: None,
        };
        run_detail(&run, workspace, cx)
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
        .border_color(crate::ui::skin::border_strong(theme))
        .bg(crate::ui::skin::popover(theme))
        .shadow(crate::ui::skin::elevation_shadow(theme))
        .overflow_hidden()
        .child(
            div()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(theme.border)
                .child(div().flex_1().min_w_0().text_sm().truncate().child(title))
                .child(
                    Button::new("subagent-window-close")
                        .label(t("Close", "关闭"))
                        .small()
                        .ghost()
                        .tooltip(t("Close subagent details", "关闭子代理详情"))
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_open_subagent("", cx);
                        })),
                ),
        )
        .child(
            div()
                .id("subagent-window-body")
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .p_3()
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
    fn failures_and_integration_handoffs_have_distinct_semantics() {
        assert_eq!(run_tone(AgentRunStatus::Failed), StatusTone::Error);
        assert_eq!(run_tone(AgentRunStatus::Blocked), StatusTone::Warning);
        assert_eq!(
            workflow_tone(AgentWorkflowStatus::WaitingIntegration),
            StatusTone::Warning
        );
        let workflow = AgentWorkflowSnapshot {
            id: "w".into(),
            name: "migration".into(),
            revision: 0,
            status: AgentWorkflowStatus::WaitingIntegration,
            nodes: Vec::new(),
            acceptance: Some("PASS".into()),
            result: None,
        };
        assert_ne!(acceptance_text(&workflow), "PASS");
        assert!(!crate::view_model::workflow_terminal(workflow.status));
    }

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
