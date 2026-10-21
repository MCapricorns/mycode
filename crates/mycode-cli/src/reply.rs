//! Assistant reply lines with fenced code colored by the shared lexer.
//!
//! Prose keeps the terminal's own ink. Code tokens use the named ANSI
//! colors, so they follow whatever palette the terminal is set to, light or
//! dark. Fence marker lines are dimmed.
use mycode_frontend::highlight::{TokenKind, fence_language, highlight};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::theme::Palette;

/// `marker` plus the reply body: the first line carries the marker in
/// `marker_style`, later lines indent under the text.
pub(crate) fn push_reply(
    lines: &mut Vec<Line<'_>>,
    marker: &str,
    marker_style: Style,
    text: &str,
    palette: Palette,
) {
    let indent = " ".repeat(marker.chars().count());
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut source = text.lines().peekable();
    while let Some(line) = source.next() {
        let Some(fence) = Fence::open(line) else {
            rows.push(vec![Span::raw(line.to_owned())]);
            continue;
        };
        let dim = Style::default().fg(palette.dim);
        rows.push(vec![Span::styled(line.to_owned(), dim)]);
        let mut code = Vec::new();
        let mut closing = None;
        for line in source.by_ref() {
            if fence.closes(line) {
                closing = Some(line);
                break;
            }
            code.push(line);
        }
        if !code.is_empty() {
            rows.extend(code_rows(&code.join("\n"), fence.lang.as_deref(), palette));
        }
        if let Some(line) = closing {
            rows.push(vec![Span::styled(line.to_owned(), dim)]);
        }
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    for (index, mut spans) in rows.into_iter().enumerate() {
        let lead = if index == 0 {
            Span::styled(marker.to_owned(), marker_style)
        } else {
            Span::raw(indent.clone())
        };
        spans.insert(0, lead);
        lines.push(Line::from(spans));
    }
}

/// An opening fence: up to three spaces, then three or more backticks or
/// tildes and an info string.
struct Fence {
    marker: char,
    length: usize,
    lang: Option<String>,
}

impl Fence {
    fn open(line: &str) -> Option<Self> {
        let body = fence_body(line)?;
        let marker = body.chars().next().filter(|c| matches!(c, '`' | '~'))?;
        let length = body.chars().take_while(|c| *c == marker).count();
        let info = &body[length..];
        if length < 3 || (marker == '`' && info.contains('`')) {
            return None;
        }
        Some(Self {
            marker,
            length,
            lang: fence_language(info),
        })
    }

    fn closes(&self, line: &str) -> bool {
        fence_body(line).is_some_and(|body| {
            let length = body.chars().take_while(|c| *c == self.marker).count();
            length >= self.length && body[length..].trim().is_empty()
        })
    }
}

fn fence_body(line: &str) -> Option<&str> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    (indent <= 3).then_some(&line[indent..])
}

/// One row of spans per code line, split at the lexer's token edges.
fn code_rows(code: &str, lang: Option<&str>, palette: Palette) -> Vec<Vec<Span<'static>>> {
    let tokens = highlight(code, lang);
    let mut rows = Vec::new();
    let mut first = 0;
    let mut start = 0;
    for line in code.split('\n') {
        let end = start + line.len();
        while tokens
            .get(first)
            .is_some_and(|(range, _)| range.end <= start)
        {
            first += 1;
        }
        let mut spans = Vec::new();
        let mut at = start;
        for (range, kind) in tokens[first..]
            .iter()
            .take_while(|(range, _)| range.start < end)
        {
            let from = range.start.max(start);
            let to = range.end.min(end);
            if from > at {
                spans.push(Span::raw(code[at..from].to_owned()));
            }
            spans.push(Span::styled(
                code[from..to].to_owned(),
                style(*kind, palette),
            ));
            at = to;
        }
        if at < end {
            spans.push(Span::raw(code[at..end].to_owned()));
        }
        rows.push(spans);
        start = end + 1;
    }
    rows
}

fn style(kind: TokenKind, palette: Palette) -> Style {
    let color = match kind {
        TokenKind::Keyword => Color::Magenta,
        TokenKind::Type | TokenKind::Variable => Color::Cyan,
        TokenKind::Function | TokenKind::Meta => Color::Blue,
        TokenKind::String | TokenKind::Inserted => Color::Green,
        TokenKind::Number => Color::Yellow,
        TokenKind::Comment => palette.dim,
        TokenKind::Property => Color::LightBlue,
        TokenKind::Macro | TokenKind::Tag => Color::LightRed,
        TokenKind::Deleted => Color::Red,
    };
    let style = Style::default().fg(color);
    if kind == TokenKind::Comment {
        style.add_modifier(Modifier::ITALIC)
    } else {
        style
    }
}

#[cfg(test)]
mod tests {
    use super::push_reply;
    use crate::app::theme::Palette;
    use ratatui::style::{Color, Style};
    use ratatui::text::Line;

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn fenced_code_is_colored_and_prose_keeps_its_text() {
        let mut lines = Vec::new();
        let reply = "Run this:\n```rust\nfn main() {} // go\n```\nDone";
        push_reply(&mut lines, "⏺ ", Style::default(), reply, Palette::DARK);
        let rendered: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(
            rendered,
            [
                "⏺ Run this:",
                "  ```rust",
                "  fn main() {} // go",
                "  ```",
                "  Done"
            ]
        );
        let code = &lines[2];
        let keyword = code.spans.iter().find(|span| span.content == "fn").unwrap();
        assert_eq!(keyword.style.fg, Some(Color::Magenta));
        let comment = code
            .spans
            .iter()
            .find(|span| span.content == "// go")
            .unwrap();
        assert_eq!(comment.style.fg, Some(Color::DarkGray));
        assert_eq!(lines[1].spans[1].style.fg, Some(Color::DarkGray));
        assert_eq!(lines[4].spans[1].style.fg, None);
    }

    #[test]
    fn an_open_fence_still_colors_the_streamed_code() {
        let mut lines = Vec::new();
        push_reply(
            &mut lines,
            "⏺ ",
            Style::default(),
            "```py\nx = \"s\nmore",
            Palette::DARK,
        );
        assert_eq!(lines.len(), 3);
        assert_eq!(text(&lines[2]), "  more");
        let string = lines[1]
            .spans
            .iter()
            .find(|span| span.content == "\"s")
            .unwrap();
        assert_eq!(string.style.fg, Some(Color::Green));
    }

    #[test]
    fn an_empty_reply_still_shows_its_marker() {
        let mut lines = Vec::new();
        push_reply(&mut lines, "⏺ ", Style::default(), "", Palette::DARK);
        assert_eq!(lines.len(), 1);
        assert_eq!(text(&lines[0]), "⏺ ");
    }
}
