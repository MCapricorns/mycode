//! Tool cards: one icon and a status dot per call, diff stats only for
//! edits, and a one-line summary when several calls land in a row.
use gpui_kit::assets::IconName;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px,
};

use crate::i18n::{is_chinese, t};
use crate::ui::skin;
use crate::ui::{desk::Desk, ellipsis};
use crate::view_model::ConversationEntry;
use crate::workspace::Workspace;

/// One tool row. Clicking it reveals the result without changing the call.
pub(super) fn render_tool_block(
    call: &ConversationEntry,
    result: Option<&ConversationEntry>,
    expanded: bool,
    reduce_motion: bool,
    cx: &Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let desk = Desk::of(theme);
    let failed = result.is_some_and(|entry| entry.text.starts_with("failed:"));
    let waiting = result.is_none();
    let tool = tool_name(call.text.as_ref());
    let status = tool_status(tool, result);
    let event_id = call.event_id.clone();
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
                    workspace.on_toggle_tool_row(&event_id, cx);
                }))
                .child(status_dot(failed, waiting, theme))
                .child(
                    Icon::new(tool_icon(tool))
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.foreground)
                        .child(tool_title(tool, call.text.as_ref()).to_string()),
                )
                .when(!status.is_empty(), |row| {
                    row.child(tool_status_node(&status, failed, theme))
                }),
        );
    if expanded && let Some(result) = result {
        let body = div()
            .w_full()
            .min_w_0()
            .pt(skin::space(1))
            .pl(skin::space(4))
            .child(result_body(tool, result, theme, &desk));
        row = row.child(crate::ui::motion::fade_in(
            format!("tool-body-{}", call.event_id),
            reduce_motion,
            body,
        ));
    }
    row.into_any_element()
}

/// One line for a run of tool calls. Clicking it reveals the cards.
pub(super) fn render_tool_batch(
    id: &str,
    summary: String,
    open: bool,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let batch_id = id.to_owned();
    let marker = if open { "▾" } else { "▸" };
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
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(marker),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_xs()
                .text_color(theme.foreground)
                .child(summary),
        )
}

fn status_dot(failed: bool, waiting: bool, theme: &Theme) -> impl IntoElement {
    let color = if failed {
        theme.red
    } else if waiting {
        theme.primary
    } else {
        theme.green
    };
    div()
        .size(skin::space(2))
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
        .border_1()
        .border_color(color.opacity(0.35))
}

fn tool_icon(tool: &str) -> IconName {
    match tool {
        "read" => IconName::FileText,
        "edit" | "write" => IconName::FilePen,
        "grep" | "search" => IconName::Search,
        "find" => IconName::FolderSearch,
        "bash" | "zsh" | "sh" | "shell" | "pwsh" | "powershell" | "cmd" => IconName::Terminal,
        "web_search" | "fetch_content" => IconName::Globe,
        "agent" => IconName::Bot,
        "ask" | "ask_user" => IconName::MessageSquare,
        "run_code" => IconName::Code,
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
        "bash" | "zsh" | "sh" | "shell" | "pwsh" | "powershell" | "cmd" => "shell",
        "web_search" | "fetch_content" => "web",
        "agent" => "agent",
        "ask" | "ask_user" => "ask",
        "run_code" => "run",
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
        "edit" => (format!("edited {count}"), format!("编辑了 {count}")),
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
        "ask" => (format!("asked {count}"), format!("提问了 {count}")),
        "run" if count == 1 => (
            format!("ran {count} program"),
            format!("运行了 {count} 个程序"),
        ),
        "run" => (
            format!("ran {count} programs"),
            format!("运行了 {count} 个程序"),
        ),
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

pub(super) fn tool_status(tool: &str, result: Option<&ConversationEntry>) -> String {
    let Some(result) = result else {
        return t("in progress", "进行中").to_owned();
    };
    let text = result.text.as_ref();
    if text.starts_with("failed:") {
        return t("failed", "失败").to_owned();
    }
    if tool_paints_diff(tool) {
        let added = text.lines().filter(|line| diff_added_line(line)).count();
        let removed = text.lines().filter(|line| diff_removed_line(line)).count();
        if added + removed > 0 {
            return format!("+{added} -{removed}");
        }
    }
    if text.trim().is_empty() {
        return t("done", "完成").to_owned();
    }
    status_snippet(text)
}

/// First line that reads as output, or nothing when every line is decoration
/// or a raw byte window such as `|347270:347292`.
fn status_snippet(text: &str) -> String {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || decoration_line(line) || raw_span_line(line) {
            continue;
        }
        // Drop a markdown heading marker and the rule characters that frame
        // ASCII art (`....Pelican....`, `# Tokio`). Digits and brackets stay.
        let line = line.trim_matches(|c: char| c.is_whitespace() || is_rule_char(c));
        if line.is_empty() || decoration_line(line) || raw_span_line(line) {
            continue;
        }
        return ellipsis(line, 48);
    }
    String::new()
}

/// A line is noise only when it is rules and box drawing, plus optional
/// whitespace (`=====`, `-------`, `────`). Digits and brackets are output:
/// `42`, `3.14`, and `[1, 2, 3]` are summaries. A banner that still contains
/// a word (`===== tokio =====`) is noise when rule glyphs dominate it. Dots
/// are not counted in that check, so an ASCII-art row keeps the lettering
/// after the edges are stripped.
fn decoration_line(line: &str) -> bool {
    let mut saw_ink = false;
    for c in line.chars() {
        if c.is_whitespace() {
            continue;
        }
        saw_ink = true;
        if !is_rule_char(c) {
            saw_ink = false;
            break;
        }
    }
    if saw_ink {
        return true;
    }
    let chars: Vec<char> = line.chars().collect();
    let rules = chars.iter().filter(|c| is_banner_rule(**c)).count();
    rules >= 6 && rules.saturating_mul(2) >= chars.len()
}

/// Rule and box-drawing glyphs. Whitespace is handled by the caller.
fn is_rule_char(c: char) -> bool {
    matches!(c, '=' | '-' | '_' | '*' | '~' | '#' | '.' | '|' | '+')
        || ('\u{2500}'..='\u{257F}').contains(&c)
}

/// Glyphs that mark a text banner. `.` is omitted so a pelican row of dots
/// is not discarded before its lettering is trimmed.
fn is_banner_rule(c: char) -> bool {
    matches!(c, '=' | '-' | '_' | '*' | '#' | '|' | '+' | '~')
}

/// A digit-heavy `|start:end` window from a raw output dump.
fn raw_span_line(line: &str) -> bool {
    let sample: String = line.chars().take(48).collect();
    let len = sample.chars().count();
    if len == 0 {
        return false;
    }
    let digits = sample.chars().filter(|c| c.is_ascii_digit()).count();
    if digits.saturating_mul(2) < len {
        return false;
    }
    let core = sample
        .trim()
        .trim_matches(|c: char| matches!(c, '|' | '…' | '.' | ' '));
    let Some((left, right)) = core.split_once(':') else {
        return false;
    };
    let right = right.trim_end_matches(['…', '.']);
    !left.is_empty()
        && !right.is_empty()
        && left.chars().all(|c| c.is_ascii_digit())
        && right.chars().all(|c| c.is_ascii_digit())
}

/// Only edit and write previews use `+ ` / `- ` as a diff. Markdown from an
/// agent or scout starts list items the same way, and counting those lines
/// painted a finished scout as `+0 -21`.
fn tool_paints_diff(tool: &str) -> bool {
    matches!(tool, "edit" | "write")
}

fn shows_diff_preview(tool: &str, text: &str) -> bool {
    tool_paints_diff(tool)
        && text.lines().any(|line| {
            diff_added_line(line) || diff_removed_line(line) || line.starts_with("[diff truncated]")
        })
}

/// The first token of a tool label is the tool name. The rest is the target.
pub(super) fn tool_name(label: &str) -> &str {
    label.split_whitespace().next().unwrap_or(label)
}

/// `run_code` cards are titled by the program description. Other cards keep
/// the `name  target` label.
fn tool_title<'a>(tool: &str, label: &'a str) -> &'a str {
    if tool == "run_code" {
        label
            .split_once("  ")
            .map(|(_, rest)| rest.trim())
            .filter(|rest| !rest.is_empty())
            .unwrap_or(label)
    } else {
        label
    }
}

fn diff_added_line(line: &str) -> bool {
    (line.starts_with("++ ") || line.starts_with("+ ")) && !line.starts_with("+++")
}

fn diff_removed_line(line: &str) -> bool {
    (line.starts_with("-- ") || line.starts_with("- ")) && !line.starts_with("---")
}

/// `+3 -1` from [`tool_status`]. Only digit counts match, so a status line
/// that merely contains " -" stays plain text.
fn diff_counts(status: &str) -> Option<(&str, &str)> {
    let (added, removed) = status.split_once(" -")?;
    let added = added.strip_prefix('+')?;
    if added.is_empty()
        || removed.is_empty()
        || !added.bytes().all(|byte| byte.is_ascii_digit())
        || !removed.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some((added, removed))
}

/// One `+` or `-` on screen. Older previews stored a doubled sign.
fn diff_line_text(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("++ ") {
        format!("+ {rest}")
    } else if let Some(rest) = line.strip_prefix("-- ") {
        format!("- {rest}")
    } else {
        line.to_owned()
    }
}

fn tool_status_node(status: &str, failed: bool, theme: &Theme) -> gpui_kit::AnyElement {
    if let Some((added, removed)) = diff_counts(status) {
        return div()
            .flex()
            .flex_row()
            .items_center()
            .gap(skin::space(2))
            .flex_shrink_0()
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .child(
                div()
                    .text_color(crate::ui::desk::diff_ink(theme, true))
                    .child(format!("+{added}")),
            )
            .child(
                div()
                    .text_color(crate::ui::desk::diff_ink(theme, false))
                    .child(format!("-{removed}")),
            )
            .into_any_element();
    }
    div()
        .flex_shrink_0()
        .max_w(px(220.))
        .truncate()
        .text_xs()
        .text_color(if failed {
            crate::ui::desk::diff_ink(theme, false)
        } else {
            theme.muted_foreground
        })
        .child(status.to_owned())
        .into_any_element()
}

fn result_body(
    tool: &str,
    result: &ConversationEntry,
    theme: &Theme,
    desk: &Desk,
) -> impl IntoElement {
    let text = result.text.to_string();
    let failed = text.starts_with("failed:");
    let lines: Vec<&str> = text.lines().collect();
    if shows_diff_preview(tool, &text) {
        diff_preview(&lines, theme).into_any_element()
    } else if matches!(tool, "grep" | "find" | "search") && !failed {
        search_preview(tool, &lines, theme, desk).into_any_element()
    } else if failed || text.len() > 300 || lines.len() > 6 {
        div()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .px(skin::space(2))
            .py(skin::space(1))
            .rounded(skin::radius_xs())
            .text_xs()
            .whitespace_normal()
            .font_family(theme.mono_font_family.clone())
            .bg(desk.screen.opacity(0.45))
            .text_color(if failed { desk.red } else { desk.screen_dim })
            .child(ellipsis(&text, 2000))
            .into_any_element()
    } else {
        div()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .whitespace_normal()
            .px(skin::space(2))
            .py(skin::space(1))
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.muted_foreground)
            .child(ellipsis(&text, 600))
            .into_any_element()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffKind {
    Add,
    Remove,
    Context,
    Meta,
}

struct NumberedLine {
    old: Option<u32>,
    new: Option<u32>,
    text: String,
    kind: DiffKind,
}

fn number_diff(lines: &[&str]) -> Vec<NumberedLine> {
    let mut old_n = 1u32;
    let mut new_n = 1u32;
    let mut started = false;
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        if let Some((old, new)) = hunk_start(line) {
            old_n = old;
            new_n = new;
            started = true;
            out.push(NumberedLine {
                old: None,
                new: None,
                text: (*line).to_owned(),
                kind: DiffKind::Meta,
            });
            continue;
        }
        if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("[diff truncated]")
        {
            out.push(NumberedLine {
                old: None,
                new: None,
                text: (*line).to_owned(),
                kind: DiffKind::Meta,
            });
            continue;
        }
        let added = diff_added_line(line);
        let removed = diff_removed_line(line);
        if !started && !added && !removed {
            out.push(NumberedLine {
                old: None,
                new: None,
                text: (*line).to_owned(),
                kind: DiffKind::Meta,
            });
            continue;
        }
        started = true;
        if added {
            out.push(NumberedLine {
                old: None,
                new: Some(new_n),
                text: diff_line_text(line),
                kind: DiffKind::Add,
            });
            new_n += 1;
        } else if removed {
            out.push(NumberedLine {
                old: Some(old_n),
                new: None,
                text: diff_line_text(line),
                kind: DiffKind::Remove,
            });
            old_n += 1;
        } else {
            out.push(NumberedLine {
                old: Some(old_n),
                new: Some(new_n),
                text: diff_line_text(line),
                kind: DiffKind::Context,
            });
            old_n += 1;
            new_n += 1;
        }
    }
    out
}

fn hunk_start(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@ ")?;
    let (old, rest) = rest.split_once(' ')?;
    let new = rest.split_once(' ')?.0;
    let old = old.strip_prefix('-')?.split(',').next()?;
    let new = new.strip_prefix('+')?.split(',').next()?;
    Some((old.parse().ok()?, new.parse().ok()?))
}

fn diff_preview(lines: &[&str], theme: &Theme) -> impl IntoElement {
    let added = lines.iter().filter(|line| diff_added_line(line)).count();
    let removed = lines.iter().filter(|line| diff_removed_line(line)).count();
    let numbered = number_diff(lines);
    let shown = numbered.len().min(80);
    let add_ink = crate::ui::desk::diff_ink(theme, true);
    let remove_ink = crate::ui::desk::diff_ink(theme, false);
    div()
        .id(format!(
            "tool-diff-{added}-{removed}-{}",
            lines.first().map(|line| line.len()).unwrap_or(0)
        ))
        .w_full()
        .min_w_0()
        .overflow_x_scroll()
        .flex()
        .flex_col()
        .py(skin::space(1))
        .rounded(skin::radius_xs())
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .child(
            div()
                .px(skin::space(2))
                .py(px(2.))
                .flex()
                .flex_row()
                .gap(skin::space(2))
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(t("DIFF", "差异")),
                )
                .child(div().text_color(add_ink).child(format!("+{added}")))
                .child(div().text_color(remove_ink).child(format!("-{removed}"))),
        )
        .children(numbered.iter().take(shown).map(|line| {
            let (color, bg) = match line.kind {
                DiffKind::Add => (add_ink, crate::ui::desk::diff_wash(add_ink)),
                DiffKind::Remove => (remove_ink, crate::ui::desk::diff_wash(remove_ink)),
                DiffKind::Context | DiffKind::Meta => (theme.muted_foreground, theme.transparent),
            };
            div()
                .flex()
                .flex_row()
                .w_full()
                .min_w_0()
                .text_color(color)
                .bg(bg)
                .child(line_number(line.old, theme))
                .child(line_number(line.new, theme))
                .child(
                    div()
                        .min_w_0()
                        .px(skin::space(2))
                        .whitespace_nowrap()
                        .child(line.text.clone()),
                )
        }))
        .when(numbered.len() > shown, |this| {
            this.child(preview_caption(
                t("… more changes omitted", "… 更多改动已省略"),
                theme,
            ))
        })
}

fn line_number(number: Option<u32>, theme: &Theme) -> impl IntoElement {
    div()
        .w(px(36.))
        .flex_shrink_0()
        .text_right()
        .pr(skin::space(1))
        .text_color(theme.muted_foreground.opacity(0.85))
        .child(number.map(|value| value.to_string()).unwrap_or_default())
}

fn search_preview(tool: &str, lines: &[&str], theme: &Theme, desk: &Desk) -> impl IntoElement {
    let hits = lines.iter().filter(|line| !line.starts_with('[')).count();
    let label = if tool == "find" {
        t("PATHS", "路径")
    } else {
        t("MATCHES", "匹配")
    };
    let shown = lines.len().min(40);
    div()
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .py(skin::space(1))
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .child(preview_caption(&format!("{label}  {hits}"), theme))
        .children(lines.iter().take(shown).enumerate().map(|(index, line)| {
            let notice = line.starts_with('[');
            div()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .whitespace_normal()
                .px(skin::space(2))
                .flex()
                .flex_row()
                .gap(skin::space(2))
                .text_color(if notice {
                    theme.muted_foreground
                } else {
                    theme.foreground
                })
                .bg(if notice {
                    theme.transparent
                } else if index % 2 == 1 {
                    desk.green.opacity(0.06)
                } else {
                    theme.transparent
                })
                .child(
                    div()
                        .w(px(28.))
                        .flex_shrink_0()
                        .text_color(theme.muted_foreground)
                        .child((index + 1).to_string()),
                )
                .child(div().min_w_0().child((*line).to_owned()))
        }))
        .when(lines.len() > shown, |this| {
            this.child(preview_caption(
                t("… more results omitted", "… 更多结果已省略"),
                theme,
            ))
        })
}

fn preview_caption(label: &str, theme: &Theme) -> impl IntoElement {
    div()
        .px(skin::space(2))
        .py(px(2.))
        .text_color(theme.muted_foreground)
        .child(label.to_owned())
        .font_family(theme.mono_font_family.clone())
}

#[cfg(test)]
mod tests {
    use super::{diff_counts, diff_line_text, shows_diff_preview, tool_batch_summary, tool_status};
    use crate::view_model::{ConversationEntry, EntryKind};

    fn result_entry(text: &str) -> ConversationEntry {
        ConversationEntry {
            event_id: "evt".into(),
            kind: EntryKind::ToolResult,
            text: text.into(),
            call_id: Some("call".into()),
            thinking: String::new(),
            parent_call_id: None,
        }
    }

    /// Eleven markdown list lines are what QA saw counted as deletions.
    fn eleven_list_lines(header: &str) -> String {
        let mut page = format!("{header}\n");
        for n in 1..=11 {
            page.push_str(&format!("- line {n}\n"));
        }
        page
    }

    #[test]
    fn fetch_content_list_lines_are_not_a_diff() {
        let page = eleven_list_lines("[https://example.com/a]");
        let status = tool_status("fetch_content", Some(&result_entry(&page)));
        assert_eq!(status, "[https://example.com/a]");
        assert_ne!(status, "+0 -11");
        assert!(!shows_diff_preview("fetch_content", &page));
    }

    #[test]
    fn web_search_snippets_are_not_a_diff() {
        let text = "[https://example.com] Docs\n- alpha\n- beta\n";
        let status = tool_status("web_search", Some(&result_entry(text)));
        assert_eq!(status, "[https://example.com] Docs");
        assert!(!shows_diff_preview("web_search", text));
    }

    #[test]
    fn agent_and_scout_markdown_is_not_a_diff() {
        let mut text = String::from("scout notes\n");
        for n in 1..=21 {
            text.push_str(&format!("- line {n}\n"));
        }
        let status = tool_status("agent", Some(&result_entry(&text)));
        assert_eq!(status, "scout notes");
        assert_ne!(status, "+0 -21");
        assert!(!shows_diff_preview("agent", &text));
        assert!(!shows_diff_preview("scout", &text));
    }

    #[test]
    fn edit_removals_still_summarize_as_a_diff() {
        let mut text = String::from("edited src/lib.rs\n");
        for n in 1..=11 {
            text.push_str(&format!("- line {n}\n"));
        }
        assert_eq!(tool_status("edit", Some(&result_entry(&text))), "+0 -11");
        assert_eq!(diff_counts("+0 -11"), Some(("0", "11")));
        assert!(shows_diff_preview("edit", &text));
    }

    #[test]
    fn write_additions_summarize_as_a_diff() {
        let text = "wrote src/new.rs\n+ alpha\n+ beta\n";
        assert_eq!(tool_status("write", Some(&result_entry(text))), "+2 -0");
        assert!(shows_diff_preview("write", text));
    }

    #[test]
    fn raw_output_banners_are_not_the_summary() {
        let banner = "===== tokio =====\n|347270:347292…\n-------\n";
        assert_eq!(tool_status("bash", Some(&result_entry(banner))), "");
        let with_text = "===== tokio =====\n-------\ncompiled 3 files\n";
        assert_eq!(
            tool_status("bash", Some(&result_entry(with_text))),
            "compiled 3 files"
        );
        assert_eq!(tool_status("bash", Some(&result_entry("   \n"))), "done");
    }

    #[test]
    fn numbers_and_brackets_are_the_summary() {
        assert_eq!(tool_status("bash", Some(&result_entry("42\n"))), "42");
        assert_eq!(tool_status("bash", Some(&result_entry("3.14\n"))), "3.14");
        assert_eq!(
            tool_status("bash", Some(&result_entry("[1, 2, 3]\n"))),
            "[1, 2, 3]"
        );
    }

    #[test]
    fn ascii_art_dots_and_markdown_headings_are_trimmed() {
        let pelican = format!("{}Pelican", ".".repeat(60));
        assert_eq!(
            tool_status("bash", Some(&result_entry(&pelican))),
            "Pelican"
        );
        assert_eq!(
            tool_status(
                "bash",
                Some(&result_entry("# Tokio Runtime — Fact Sheet\n"))
            ),
            "Tokio Runtime — Fact Sheet"
        );
        assert_eq!(tool_status("bash", Some(&result_entry("...\n"))), "");
    }

    #[test]
    fn legacy_doubled_signs_still_count_and_display_as_one() {
        let mut text = String::from("edited src/lib.rs\n");
        text.push_str("++ kept\n");
        text.push_str("-- gone\n");
        assert_eq!(tool_status("edit", Some(&result_entry(&text))), "+1 -1");
        assert_eq!(diff_line_text("++ kept"), "+ kept");
        assert_eq!(diff_line_text("-- gone"), "- gone");
        assert_eq!(diff_line_text("+ kept"), "+ kept");
        assert_eq!(diff_line_text("+++ b/file.rs"), "+++ b/file.rs");
        assert_eq!(diff_line_text("--- a/file.rs"), "--- a/file.rs");
    }

    #[test]
    fn a_run_of_reads_and_edits_collapses_to_one_line() {
        let labels = [
            "read  a.rs",
            "read  b.rs",
            "read  c.rs",
            "edit  d.rs",
            "edit  e.rs",
            "edit  f.rs",
            "edit  g.rs",
        ];
        assert_eq!(tool_batch_summary(&labels), "read 3 files, edited 4");
        assert_eq!(
            tool_batch_summary(&["bash  cargo test", "grep  fn main"]),
            "ran 1 command, searched 1 time"
        );
    }

    #[test]
    fn diff_lines_are_numbered_from_the_first_change() {
        let lines = ["edited src/lib.rs", "+ alpha", "- beta", "  kept"];
        let numbered = super::number_diff(&lines);
        assert_eq!(numbered[0].old, None);
        assert_eq!(numbered[1].new, Some(1));
        assert_eq!(numbered[1].old, None);
        assert_eq!(numbered[2].old, Some(1));
        assert_eq!(numbered[2].new, None);
        assert_eq!(numbered[3].old, Some(2));
        assert_eq!(numbered[3].new, Some(2));
    }
}
