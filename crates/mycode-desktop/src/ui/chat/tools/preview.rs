//! Result previews: diff numbering, search excerpts, and the empty-state
//! captions a tool row renders under its status line.

use super::*;
pub(super) fn tool_paints_diff(tool: &str) -> bool {
    matches!(tool, "edit" | "write")
}

pub(super) fn shows_diff_preview(tool: &str, text: &str) -> bool {
    tool_paints_diff(tool)
        && text.lines().any(|line| {
            diff_added_line(line) || diff_removed_line(line) || line.starts_with("[diff truncated]")
        })
}

/// The first token of a tool label is the tool name. The rest is the target.
pub(super) fn tool_name(label: &str) -> &str {
    label.split_whitespace().next().unwrap_or(label)
}

/// Everything after the tool name: the path, query, or command.
pub(super) fn tool_label_target(label: &str) -> &str {
    label
        .trim()
        .strip_prefix(tool_name(label.trim()))
        .map(str::trim)
        .unwrap_or("")
}

/// Distinct call targets of one batch in first-seen order, joined for the
/// collapsed row. Empty targets (a bare tool name) are skipped.
#[must_use]
pub(super) fn tool_batch_targets(labels: &[&str]) -> String {
    let mut targets: Vec<&str> = Vec::new();
    for label in labels {
        let target = tool_label_target(label);
        if !target.is_empty() && !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets.join(" · ")
}

pub(super) fn diff_added_line(line: &str) -> bool {
    (line.starts_with("++ ") || line.starts_with("+ ")) && !line.starts_with("+++")
}

pub(super) fn diff_removed_line(line: &str) -> bool {
    (line.starts_with("-- ") || line.starts_with("- ")) && !line.starts_with("---")
}

pub(super) fn count_diff_changes<'a>(lines: impl IntoIterator<Item = &'a str>) -> (usize, usize) {
    let mut added = 0usize;
    let mut removed = 0usize;
    for line in lines {
        if diff_added_line(line) {
            added += 1;
        } else if diff_removed_line(line) {
            removed += 1;
        }
    }
    (added, removed)
}

/// `+3 -1` from [`tool_status`]. Only digit counts match, so a status line
/// that merely contains " -" stays plain text.
pub(super) fn diff_counts(status: &str) -> Option<(&str, &str)> {
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
pub(super) fn diff_line_text(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("++ ") {
        format!("+ {rest}")
    } else if let Some(rest) = line.strip_prefix("-- ") {
        format!("- {rest}")
    } else {
        line.to_owned()
    }
}

pub(super) fn tool_status_node(status: &str, alert: bool, theme: &Theme) -> gpui_kit::AnyElement {
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
        .text_color(if alert {
            crate::ui::desk::diff_ink(theme, false)
        } else {
            theme.muted_foreground
        })
        .child(status.to_owned())
        .into_any_element()
}

/// The path inside a `read` target.
///
/// [`mycode_core::tool_target`] appends a line window (`path L520+620` or
/// `path +200`); the path itself may contain spaces, so only a trailing
/// window is stripped.
pub(super) fn read_path(target: &str) -> &str {
    let target = target.trim();
    for marker in [" L", " +"] {
        if let Some(at) = target.rfind(marker) {
            let window = &target[at + marker.len()..];
            if window.chars().all(|ch| ch.is_ascii_digit() || ch == '+') && !window.is_empty() {
                return target[..at].trim().trim_matches('"');
            }
        }
    }
    target.trim_matches('"')
}

/// A highlighted body for file text printed by `read` or a read-only shell
/// command.
///
/// The file extension in `path` is the only language hint available, and
/// the text arrives bare with no fence. An unknown extension or a body
/// without recognized tokens keeps the plain rendering, so prose and
/// command output are never painted as code.
pub(super) fn file_preview(
    path: &str,
    text: &str,
    theme: &Theme,
    desk: &Desk,
) -> Option<gpui_kit::AnyElement> {
    let lang = std::path::Path::new(path).extension()?.to_str()?;
    let code = ellipsis(text, 2000);
    let tokens = highlight(&code, Some(lang));
    if tokens.is_empty() {
        return None;
    }
    let light = !theme.mode.is_dark();
    Some(
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
            .text_color(desk.screen_dim)
            .child(
                StyledText::new(code).with_highlights(
                    tokens
                        .into_iter()
                        .map(|(range, kind)| (range, syntax::style(kind, light))),
                ),
            )
            .into_any_element(),
    )
}

pub(super) fn result_body(
    tool: &str,
    target: &str,
    result: &ConversationEntry,
    theme: &Theme,
    desk: &Desk,
) -> impl IntoElement {
    let raw: &str = &result.text;
    let failed = raw.starts_with("failed:");
    // The row already says 失败, so the projection's `failed: ` marker is not
    // repeated in front of the first output line (`failed: True` read like a
    // command output of its own). Keep it only to drive the status.
    let text = raw.strip_prefix("failed: ").unwrap_or(raw);
    let lines: Vec<&str> = text.lines().collect();
    if shows_diff_preview(tool, text) {
        diff_preview(&lines, theme).into_any_element()
    } else if matches!(tool, "grep" | "find" | "search") && !failed {
        search_preview(tool, &lines, theme, desk).into_any_element()
    } else if tool == "read"
        && !failed
        && let Some(preview) = file_preview(read_path(target), text, theme, desk)
    {
        preview
    } else if is_shell_tool(tool)
        && !failed
        && let Some(path) = shell_read::shell_read_path(target)
        && let Some(preview) = file_preview(&path, text, theme, desk)
    {
        preview
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
            .child(ellipsis(text, 2000))
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
            .child(ellipsis(text, 600))
            .into_any_element()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DiffKind {
    Add,
    Remove,
    Context,
    Meta,
}

pub(super) struct NumberedLine {
    pub(super) old: Option<u32>,
    pub(super) new: Option<u32>,
    pub(super) text: String,
    pub(super) kind: DiffKind,
}

pub(super) fn number_diff(lines: &[&str]) -> Vec<NumberedLine> {
    let mut old_n = 1u32;
    let mut new_n = 1u32;
    let mut started = false;
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        if let Some((old, new)) = parse_diff_hunk_header(line) {
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

pub(crate) fn parse_diff_hunk_header(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@ ")?;
    let (old, rest) = rest.split_once(' ')?;
    let new = rest.split_once(' ').map(|(head, _)| head).unwrap_or(rest);
    let old = old.strip_prefix('-')?.split(',').next()?;
    let new = new.strip_prefix('+')?.split(',').next()?;
    Some((old.parse().ok()?, new.parse().ok()?))
}

pub(super) fn diff_preview(lines: &[&str], theme: &Theme) -> impl IntoElement {
    let (added, removed) = count_diff_changes(lines.iter().copied());
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
        .restrict_scroll_to_axis()
        .on_scroll_wheel(|event, window, cx| {
            // The frame owns horizontal deltas; left to bubble the transcript
            // remaps them into a vertical page scroll.
            if event.delta.pixel_delta(window.line_height()).x != px(0.) {
                cx.stop_propagation();
            }
        })
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

pub(super) fn line_number(number: Option<u32>, theme: &Theme) -> impl IntoElement {
    div()
        .w(px(36.))
        .flex_shrink_0()
        .text_right()
        .pr(skin::space(1))
        .text_color(theme.muted_foreground.opacity(0.85))
        .child(number.map(|value| value.to_string()).unwrap_or_default())
}

pub(super) fn search_preview(tool: &str, lines: &[&str], theme: &Theme, desk: &Desk) -> impl IntoElement {
    let hits = lines.iter().filter(|line| !is_search_notice(line)).count();
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
            let notice = is_search_notice(line);
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

/// A line of a grep or find report that is not a result: a bracketed
/// notice, the empty-report line, or the skipped-path explanation.
pub(super) fn is_search_notice(line: &str) -> bool {
    line.starts_with('[')
        || line.starts_with("No matching ")
        || line.contains(" is hidden or gitignored, so ")
}

pub(super) fn preview_caption(label: &str, theme: &Theme) -> impl IntoElement {
    div()
        .px(skin::space(2))
        .py(px(2.))
        .text_color(theme.muted_foreground)
        .child(label.to_owned())
        .font_family(theme.mono_font_family.clone())
}

