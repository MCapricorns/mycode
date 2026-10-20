//! Transcript line construction: entries, streamed text, the welcome crab,
//! and the small formatting helpers shared by the frame painter.

use mycode_app::{ConversationEntry, EntryKind};
use mycode_frontend::view_model::{WorkspaceState, elapsed_label};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tui_textarea::TextArea;

use super::CRAB;
use crate::reply::push_reply;

/// Applies the composer's fixed styling to a fresh textarea.
pub(super) fn new_composer(mut composer: TextArea<'static>) -> TextArea<'static> {
    composer.set_cursor_line_style(Style::default());
    composer.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
    composer.set_placeholder_text("Ask the agent");
    composer
}

/// The crab greeting on an empty transcript — "hi ferris".
pub(super) fn welcome_lines(version: &str) -> Vec<Line<'static>> {
    let crab = Style::default().fg(CRAB);
    let dim = Style::default().fg(Color::DarkGray);
    vec![
        Line::from(Span::styled("    ▄  ▄▄▄▄▄▄▄  ▄", crab)),
        Line::from(vec![
            Span::styled("  ▄▄█ █ o o █ █▄▄", crab),
            Span::styled(
                "   hi ferris",
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("   ▀▀ ▀▀▀▀▀▀▀▀ ▀▀", crab),
            Span::styled(format!("   MYCode Cli {version}"), dim),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  type below and press Enter · / for commands · /settings picks a model · @ for files",
            dim,
        )),
        Line::from(""),
    ]
}

pub(super) fn render_entry(entry: &ConversationEntry, lines: &mut Vec<Line<'_>>, collapsed: bool) {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    match entry.kind {
        // A compaction summary rides a user message but is not the user's
        // words; one dim line marks where earlier context was folded.
        EntryKind::UserMessage if mycode_app::is_compaction_summary(&entry.text) => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  ≡ earlier context compacted into a summary",
                Style::default().fg(Color::DarkGray),
            )));
        }
        EntryKind::UserMessage => {
            lines.push(Line::from(""));
            push_wrapped(
                lines,
                "❯ ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                &entry.text,
                bold,
            );
        }
        EntryKind::AssistantMessage => {
            let thinking = Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::ITALIC);
            if !entry.thinking.is_empty() {
                if collapsed {
                    let tail = thinking_tail(&entry.thinking);
                    let headline = if tail.is_empty() {
                        "thinking ▸".to_owned()
                    } else {
                        format!("thinking · {tail} ▸")
                    };
                    push_wrapped(lines, "  ✻ ", thinking, &headline, thinking);
                } else {
                    push_wrapped(lines, "  ✻ ", thinking, &entry.thinking, thinking);
                }
            }
            // Interruption lines the agent committed stay visible but read
            // as errors, not as answer text. A quoted `[error]` build line
            // is not one of them.
            let marker = Style::default().fg(CRAB);
            let error = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
            let mut normal: Vec<&str> = Vec::new();
            for line in entry.text.lines() {
                if line.starts_with(mycode_app::INTERRUPTED_RESPONSE_PREFIX) {
                    if !normal.is_empty() {
                        let joined = normal.join("\n");
                        push_reply(lines, "⏺ ", marker, &joined);
                        normal.clear();
                    }
                    push_wrapped(lines, "✖ ", error, line, error);
                } else {
                    normal.push(line);
                }
            }
            if !normal.is_empty() {
                let joined = normal.join("\n");
                push_reply(lines, "⏺ ", marker, &joined);
            }
        }
        EntryKind::ToolCall => {
            push_wrapped(
                lines,
                "⚒ ",
                Style::default().fg(Color::Cyan),
                first_line(&entry.text),
                Style::default().fg(Color::Cyan),
            );
        }
        EntryKind::ToolResult => {
            push_wrapped(
                lines,
                "  ⎿ ",
                Style::default().fg(Color::DarkGray),
                first_line(&entry.text),
                Style::default().fg(Color::DarkGray),
            );
        }
    }
}

/// `marker` plus a wrapped body: first line carries the marker in
/// `marker_style`, continuation lines indent under the text in
/// `text_style`.
pub(super) fn push_wrapped(
    lines: &mut Vec<Line<'_>>,
    marker: &str,
    marker_style: Style,
    text: &str,
    text_style: Style,
) {
    let mut iter = text.lines();
    let indent = " ".repeat(marker.chars().count());
    match iter.next() {
        Some(first) => lines.push(Line::from(vec![
            Span::styled(marker.to_owned(), marker_style),
            Span::styled(first.to_owned(), text_style),
        ])),
        None => {
            lines.push(Line::from(Span::styled(marker.to_owned(), marker_style)));
            return;
        }
    }
    for line in iter {
        lines.push(Line::from(Span::styled(
            format!("{indent}{line}"),
            text_style,
        )));
    }
}

/// A copy of `line` whose spans borrow its text, for measuring wrapped
/// height without copying every string.
pub(super) fn borrowed_line<'a>(line: &'a Line<'_>) -> Line<'a> {
    Line {
        spans: line
            .spans
            .iter()
            .map(|span| Span::styled(span.content.as_ref(), span.style))
            .collect(),
        style: line.style,
        alignment: line.alignment,
    }
}

pub(super) fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// ` · 42s` while a turn runs; empty at rest or before the clock starts.
pub(super) fn elapsed_suffix(vm: &WorkspaceState) -> String {
    let ms = vm.turn_elapsed_ms();
    if ms == 0 {
        String::new()
    } else {
        format!(" · {}", elapsed_label(ms))
    }
}

/// Newest non-empty thinking line, cut from the front so a line still
/// streaming shows its newest words — same rule as the desktop headline.
pub(super) fn thinking_tail(text: &str) -> String {
    const MAX: usize = 72;
    let line = text
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("");
    let count = line.chars().count();
    if count <= MAX {
        return line.to_owned();
    }
    let tail: String = line.chars().skip(count - (MAX - 1)).collect();
    format!("…{tail}")
}

/// `~/…` display for the status bar's directory segment.
pub(super) fn shorten_home(path: &str) -> String {
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        let home = home.to_string_lossy().replace('\\', "/");
        let home = home.trim_end_matches('/');
        let path_unix = path.replace('\\', "/");
        // Only a whole path component matches: `/home/al` is not `~` of
        // `/home/alice`.
        if let Some(rest) = path_unix.strip_prefix(home)
            && !home.is_empty()
            && (rest.is_empty() || rest.starts_with('/'))
        {
            return format!("~{rest}");
        }
    }
    path.to_owned()
}
