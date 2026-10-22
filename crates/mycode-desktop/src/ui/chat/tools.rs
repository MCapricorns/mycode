//! Tool cards: one icon and a status dot per call, diff stats only for
//! edits, and a one-line summary when several calls land in a row.

use gpui_kit::assets::IconName;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    StyledText, div, px,
};
use mycode_frontend::highlight::highlight;

use crate::i18n::{is_chinese, t};
use crate::ui::skin;
use crate::ui::syntax;
use crate::ui::{desk::Desk, ellipsis};
use crate::view_model::ConversationEntry;
use crate::workspace::Workspace;

/// Display-only flags for one tool row, kept separate from the call and
/// result entries the row renders.
pub(super) struct ToolRowFlags {
    /// The result body is shown under the summary line.
    pub expanded: bool,
    /// The subagent behind an `agent` call is still running and can be
    /// stopped from the row's `x`.
    pub cancellable: bool,
    /// The expanded body does not animate in.
    pub reduce_motion: bool,
    /// Runtime of an `agent` call, in milliseconds. `Some` while it runs
    /// and after it finishes — the line ticks on the once-a-second repaint.
    pub elapsed_ms: Option<u64>,
    /// What the subagent behind a running `agent` call is doing now, so the
    /// transcript row says it without opening the panel.
    pub activity: Option<String>,
}

/// One tool row. Clicking it reveals the result without changing the call.
///
/// A `cancellable` row is an `agent` call whose subagent still runs: it ends
/// in an `x` that stops that child alone, and a click on the row opens the
/// subagent window, since there is no result to reveal yet.
pub(super) fn render_tool_block(
    call: &ConversationEntry,
    result: Option<&ConversationEntry>,
    turn_running: bool,
    flags: ToolRowFlags,
    cx: &Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let desk = Desk::of(theme);
    let failed = result.is_some_and(|entry| entry.text.starts_with("failed:"));
    let unfinished = result.is_none();
    // A call that failed or died with its turn paints red, the same alert
    // an `agent` failure gets.
    let alert = failed || (unfinished && !turn_running);
    let tool = tool_name(call.text.as_ref());
    let preparing = crate::view_model::is_preparing_row(call);
    let (verb, target) =
        tool_call_parts(call.text.as_ref(), result.is_some(), turn_running, failed);
    // A placeholder for a call the model is still writing has not started.
    let verb = if preparing {
        t("Preparing agent", "正在准备子代理").to_owned()
    } else {
        verb
    };
    let status = if preparing {
        String::new()
    } else {
        tool_status(tool, result, turn_running)
    };
    let event_id = call.event_id.clone();
    let elapsed = flags.elapsed_ms.map(crate::view_model::elapsed_label);
    let live_call = flags
        .cancellable
        .then(|| call.call_id.clone())
        .flatten()
        .unwrap_or_default();
    let cancel = flags
        .cancellable
        .then(|| subagent_cancel_button(&call.event_id, live_call.clone(), cx));
    let mut row = div()
        .id(format!("tool-{}", call.event_id))
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .child(
            div()
                .id(format!("tool-summary-{}", call.event_id))
                .flex()
                .flex_row()
                .items_center()
                .gap(skin::space(2))
                .w_full()
                .min_w_0()
                .min_h(px(28.))
                .px(skin::space(2))
                .py(skin::space(1))
                .rounded(skin::radius_sm())
                .cursor_pointer()
                .hover(|row| row.bg(theme.secondary_hover.opacity(0.55)))
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    if live_call.is_empty() {
                        workspace.on_toggle_tool_row(&event_id, cx);
                    } else {
                        workspace.on_open_subagent(&live_call, cx);
                    }
                }))
                .child(status_dot(failed, unfinished, turn_running, theme))
                .child(
                    Icon::new(tool_icon(tool))
                        .xsmall()
                        .flex_shrink_0()
                        .text_color(theme.muted_foreground),
                )
                // Verb, then the short status (failed, +3 -1, in progress),
                // then the target filling the rest and truncating. Nothing
                // is pinned to the far edge of a wide window.
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(skin::space(2))
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .font_weight(crate::ui::skin::WEIGHT_STRONG)
                                .text_color(if alert {
                                    theme.danger
                                } else {
                                    theme.foreground
                                })
                                .child(verb),
                        )
                        .when(!status.is_empty(), |label| {
                            label.child(tool_status_node(&status, alert, theme))
                        })
                        .when(!target.is_empty(), |label| {
                            label.child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .overflow_hidden()
                                    .truncate()
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(theme.muted_foreground)
                                    .child(target.clone()),
                            )
                        }),
                )
                .children(cancel),
        );
    // A running time and the child's current step get their own line under
    // an `agent` row, so a ticking value never squeezes the target text.
    let detail = match (elapsed, flags.activity.as_deref()) {
        (Some(elapsed), Some(activity)) => {
            Some(format!("{} {elapsed} · {activity}", t("for", "耗时")))
        }
        (Some(elapsed), None) => Some(format!("{} {elapsed}", t("for", "耗时"))),
        (None, Some(activity)) => Some(activity.to_owned()),
        (None, None) => None,
    };
    if let Some(detail) = detail {
        row = row.child(
            div()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .truncate()
                .px(skin::space(2))
                .pb(px(3.))
                .pl(px(46.))
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.muted_foreground)
                .child(detail),
        );
    }
    if flags.expanded
        && let Some(result) = result
    {
        let body = div()
            .w_full()
            .min_w_0()
            .pt(skin::space(1))
            .pl(skin::space(4))
            .child(result_body(tool, &target, result, theme, &desk));
        row = row.child(crate::ui::motion::fade_in(
            format!("tool-body-{}", call.event_id),
            flags.reduce_motion,
            body,
        ));
    }
    row.into_any_element()
}

/// The `x` on a running `agent` row. It stops that subagent only; the
/// parent turn and its other children keep going.
fn subagent_cancel_button(
    event_id: &str,
    call_id: String,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id(format!("tool-cancel-{event_id}"))
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .rounded(skin::radius_xs())
        .cursor_pointer()
        .text_color(theme.muted_foreground)
        .hover(|this| this.bg(skin::frost_hover(theme)).text_color(theme.danger))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            cx.stop_propagation();
            workspace.on_cancel_subagent(&call_id, cx);
        }))
        .child(Icon::new(IconName::X).xsmall())
}

/// One line for a run of tool calls. Clicking it reveals the cards.
///
/// The collapsed row carries the per-kind counts plus every call's target —
/// "edited 3 · a.rs, b.rs, c.rs" — so a folded batch still names the files
/// it touched. The target list truncates at the row edge.
pub(super) fn render_tool_batch(
    id: &str,
    labels: &[&str],
    open: bool,
    has_failed: bool,
    has_unfinished: bool,
    turn_running: bool,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let batch_id = id.to_owned();
    let marker = if open { "▾" } else { "▸" };
    let targets = tool_batch_targets(labels);
    div()
        .id(format!("tool-batch-{id}"))
        .flex()
        .flex_row()
        .items_center()
        .gap(skin::space(2))
        .w_full()
        .min_w_0()
        .min_h(px(28.))
        .px(skin::space(2))
        .rounded(skin::radius_sm())
        .cursor_pointer()
        .hover(|row| row.bg(theme.secondary_hover.opacity(0.55)))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.on_toggle_tool_batch(&batch_id, cx);
        }))
        .child(status_dot(has_failed, has_unfinished, turn_running, theme))
        // The count phrase is content-sized so the target list takes the
        // remaining width and truncates instead of the other way around.
        .child(
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(theme.foreground)
                .child(tool_batch_summary(labels)),
        )
        .when(!targets.is_empty(), |row| {
            row.child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .truncate()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .child(targets),
            )
        })
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(marker),
        )
}

fn status_dot(
    failed: bool,
    unfinished: bool,
    turn_running: bool,
    theme: &Theme,
) -> impl IntoElement {
    let color = tool_dot_color(failed, unfinished, turn_running, theme);
    div()
        .size(px(7.))
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
        .border_1()
        .border_color(color.opacity(0.35))
}

pub(super) fn tool_dot_color(
    failed: bool,
    unfinished: bool,
    turn_running: bool,
    theme: &Theme,
) -> gpui_kit::Hsla {
    if failed || (unfinished && !turn_running) {
        // A call that died unfinished — failed outright or interrupted with
        // its turn — gets the same red an error deserves.
        theme.red
    } else if unfinished {
        theme.primary
    } else {
        theme.green
    }
}

/// Splits a raw tool call label (`edit  src/lib.rs`) into a verb-first UI
/// label and a monospace target argument. `failed` marks a call whose
/// result carries the `failed:` marker.
pub(super) fn tool_call_parts(
    label: &str,
    completed: bool,
    turn_running: bool,
    failed: bool,
) -> (String, String) {
    let trimmed = label.trim();
    let tool = tool_name(trimmed);
    let target = tool_label_target(trimmed).to_owned();
    let active = !completed && turn_running;
    let verb = match tool {
        "read" => {
            if active {
                t("Reading", "正在读取")
            } else {
                t("Read", "读取")
            }
        }
        "edit" => {
            if active {
                t("Editing", "正在编辑")
            } else {
                t("Edited", "已编辑")
            }
        }
        "write" => {
            if active {
                t("Writing", "正在写入")
            } else {
                t("Wrote", "已写入")
            }
        }
        "grep" | "search" => {
            if active {
                t("Searching", "正在搜索")
            } else {
                t("Searched", "已搜索")
            }
        }
        "find" => {
            if active {
                t("Finding", "正在查找")
            } else {
                t("Found", "已查找")
            }
        }
        tool if is_shell_tool(tool) => {
            if active {
                t("Running", "正在运行")
            } else {
                t("Ran", "已运行")
            }
        }
        "web_search" => {
            if active {
                t("Searching web", "正在搜索网页")
            } else {
                t("Searched web", "已搜索网页")
            }
        }
        "fetch_content" => {
            if active {
                t("Fetching", "正在抓取")
            } else {
                t("Fetched", "已抓取")
            }
        }
        "agent" => {
            if active {
                t("Running agent", "正在运行子代理")
            } else if failed {
                t("Agent failed", "子代理失败")
            } else if !completed {
                t("Agent interrupted", "子代理中断")
            } else {
                t("Ran agent", "已运行子代理")
            }
        }
        "ask" | "ask_user" => {
            if active {
                t("Asking", "正在提问")
            } else {
                t("Asked", "已提问")
            }
        }
        other => other,
    };
    (verb.to_owned(), target)
}

fn is_shell_tool(tool: &str) -> bool {
    tool == "shell" || mycode_tools::ShellKind::is_tool_name(tool)
}

fn tool_icon(tool: &str) -> IconName {
    match tool {
        "read" => IconName::FileText,
        "edit" | "write" => IconName::FilePen,
        "grep" | "search" => IconName::Search,
        "find" => IconName::FolderSearch,
        tool if is_shell_tool(tool) => IconName::Terminal,
        "web_search" | "fetch_content" => IconName::Globe,
        "agent" => IconName::Bot,
        "ask" | "ask_user" => IconName::MessageSquare,
        _ => IconName::Wrench,
    }
}

/// Summary of consecutive tool labels, in first-seen order.
///
/// `read  a`, `read  b`, `read  c`, `edit  d` four times becomes
/// `read 3 files, edited 4`.
#[must_use]
pub(super) fn tool_batch_summary(labels: &[&str]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for label in labels {
        let kind = tool_kind(tool_name(label));
        if let Some((_, count)) = counts.iter_mut().find(|(name, _)| *name == kind) {
            *count += 1;
        } else {
            counts.push((kind, 1));
        }
    }
    counts
        .into_iter()
        .map(|(kind, count)| kind_phrase(kind, count))
        .collect::<Vec<_>>()
        .join(", ")
}

fn tool_kind(tool: &str) -> &'static str {
    match tool {
        "read" => "read",
        "edit" | "write" => "edit",
        "grep" | "find" | "search" => "search",
        tool if is_shell_tool(tool) => "shell",
        "web_search" | "fetch_content" => "web",
        "agent" => "agent",
        "ask" | "ask_user" => "ask",
        _ => "other",
    }
}

fn kind_phrase(kind: &str, count: usize) -> String {
    let (english, chinese) = match kind {
        "read" if count == 1 => (format!("read {count} file"), format!("读取 {count} 个文件")),
        "read" => (
            format!("read {count} files"),
            format!("读取 {count} 个文件"),
        ),
        "edit" => (format!("edited {count}"), format!("编辑了 {count} 次")),
        "search" if count == 1 => (
            format!("searched {count} time"),
            format!("搜索了 {count} 次"),
        ),
        "search" => (
            format!("searched {count} times"),
            format!("搜索了 {count} 次"),
        ),
        "shell" if count == 1 => (
            format!("ran {count} command"),
            format!("运行了 {count} 条命令"),
        ),
        "shell" => (
            format!("ran {count} commands"),
            format!("运行了 {count} 条命令"),
        ),
        "web" if count == 1 => (
            format!("fetched {count} page"),
            format!("抓取了 {count} 个页面"),
        ),
        "web" => (
            format!("fetched {count} pages"),
            format!("抓取了 {count} 个页面"),
        ),
        "agent" if count == 1 => (
            format!("ran {count} agent"),
            format!("运行了 {count} 个子代理"),
        ),
        "agent" => (
            format!("ran {count} agents"),
            format!("运行了 {count} 个子代理"),
        ),
        "ask" => (format!("asked {count}"), format!("提问了 {count} 次")),
        _ if count == 1 => (
            format!("{count} other call"),
            format!("另外 {count} 次调用"),
        ),
        _ => (
            format!("{count} other calls"),
            format!("另外 {count} 次调用"),
        ),
    };
    if is_chinese() { chinese } else { english }
}

pub(super) fn tool_status(
    tool: &str,
    result: Option<&ConversationEntry>,
    turn_running: bool,
) -> String {
    let Some(result) = result else {
        return if turn_running {
            t("in progress", "进行中").to_owned()
        } else {
            t("interrupted", "已中断").to_owned()
        };
    };
    let text = result.text.as_ref();
    if text.starts_with("failed:") {
        return t("failed", "失败").to_owned();
    }
    if tool_paints_diff(tool) {
        let (added, removed) = count_diff_changes(text.lines());
        if added + removed > 0 {
            return format!("+{added} -{removed}");
        }
    }
    // A finished call shows no excerpt: the green dot already says it is
    // done, and the output is one click away. A first-line preview at the
    // row's end only repeated the expanded body.
    String::new()
}

/// Only edit and write previews use `+ ` / `- ` as a diff. Markdown from an
/// agent or scout starts list items the same way, and counting those lines
/// painted a finished scout as `+0 -21`.
mod preview;
pub(crate) use self::preview::parse_diff_hunk_header;
use self::preview::*;
mod shell_read;
#[cfg(test)]
mod tests;
