//! Assistant markdown: prose is left-aligned with the tool cards, while fenced
//! code and tables use the full column. Tables scroll sideways, stripe their rows,
//! and keep the header pinned above a scrolling body. Bold and inline code inside
//! a cell render as text styles; the markers themselves are not shown.
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::theme::Theme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt as _, Window, WindowTextSystem, div, px,
    rems,
};

use crate::ui::skin;

#[cfg(all(test, feature = "test-support"))]
mod layout_tests;

/// One block after the prose / code / table split.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MarkdownBlock {
    /// Paragraphs, headings, lists, and anything that is not a fence or table.
    Prose(String),
    /// A fenced block, including its opening and closing markers.
    Code(String),
    /// Header row first, then body rows. Cells are trimmed.
    Table(Vec<Vec<String>>),
}

/// Splits markdown so prose can be measured separately from wide blocks.
///
/// An unclosed fence consumes the rest of the source. A table needs a
/// separator row; a header without one stays prose so a streaming line is
/// not claimed early.
#[must_use]
pub(super) fn split_markdown(source: &str) -> Vec<MarkdownBlock> {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut blocks = Vec::new();
    let mut prose: Vec<&str> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if let Some(marker) = opening_fence(line) {
            flush_prose(&mut prose, &mut blocks);
            let mut fence = vec![line];
            index += 1;
            while index < lines.len() {
                fence.push(lines[index]);
                if is_closing_fence(lines[index], marker) {
                    index += 1;
                    break;
                }
                index += 1;
            }
            blocks.push(MarkdownBlock::Code(fence.join("\n")));
            continue;
        }
        if let Some(table) = table_at(&lines, index) {
            flush_prose(&mut prose, &mut blocks);
            index += table.span;
            blocks.push(MarkdownBlock::Table(table.rows));
            continue;
        }
        prose.push(line);
        index += 1;
    }
    flush_prose(&mut prose, &mut blocks);
    blocks
}

fn flush_prose(prose: &mut Vec<&str>, blocks: &mut Vec<MarkdownBlock>) {
    if let Some(start) = prose.iter().position(|line| !line.trim().is_empty()) {
        let end = prose
            .iter()
            .rposition(|line| !line.trim().is_empty())
            .unwrap();
        blocks.push(MarkdownBlock::Prose(prose[start..=end].join("\n")));
    }
    prose.clear();
}

#[derive(Clone, Copy)]
struct Fence {
    marker: u8,
    length: usize,
}

fn fence_line(line: &str) -> Option<&str> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    (indent <= 3).then_some(&line[indent..])
}

fn opening_fence(line: &str) -> Option<Fence> {
    let line = fence_line(line)?;
    let marker = *line.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = line.bytes().take_while(|byte| *byte == marker).count();
    if length < 3 || (marker == b'`' && line[length..].contains('`')) {
        return None;
    }
    Some(Fence { marker, length })
}

fn is_closing_fence(line: &str, fence: Fence) -> bool {
    let Some(line) = fence_line(line) else {
        return false;
    };
    let length = line
        .bytes()
        .take_while(|byte| *byte == fence.marker)
        .count();
    length >= fence.length && line[length..].trim().is_empty()
}

struct ParsedTable {
    rows: Vec<Vec<String>>,
    span: usize,
}

fn table_at(lines: &[&str], start: usize) -> Option<ParsedTable> {
    if start + 1 >= lines.len()
        || !looks_like_table_row(lines[start])
        || !is_separator_row(lines[start + 1])
    {
        return None;
    }
    let header = split_row(lines[start]);
    let columns = header.len();
    if columns != split_row(lines[start + 1]).len() {
        return None;
    }
    let mut rows = vec![header];
    let mut index = start + 2;
    while index < lines.len()
        && looks_like_table_row(lines[index])
        && !is_separator_row(lines[index])
    {
        let mut row = split_row(lines[index]);
        row.resize(columns, String::new());
        rows.push(row);
        index += 1;
    }
    Some(ParsedTable {
        rows,
        span: index - start,
    })
}

fn looks_like_table_row(line: &str) -> bool {
    let mut escaped = false;
    for byte in line.bytes() {
        if byte == b'|' && !escaped {
            return true;
        }
        escaped = byte == b'\\' && !escaped;
    }
    false
}

fn is_separator_row(line: &str) -> bool {
    let cells = split_row(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let cell = cell.strip_prefix(':').unwrap_or(cell);
            let cell = cell.strip_suffix(':').unwrap_or(cell);
            !cell.is_empty() && cell.bytes().all(|byte| byte == b'-')
        })
}

fn split_row(line: &str) -> Vec<String> {
    let line = line.trim();
    let mut cells = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (index, byte) in line.bytes().enumerate() {
        if byte == b'|' && !escaped {
            cells.push(&line[start..index]);
            start = index + 1;
        }
        escaped = byte == b'\\' && !escaped;
    }
    cells.push(&line[start..]);
    let first = usize::from(line.starts_with('|'));
    let end = cells.len() - usize::from(start == line.len());
    cells[first..end.max(first)]
        .iter()
        .map(|cell| cell.trim().replace("\\|", "|"))
        .collect()
}

/// Renders assistant markdown. Prose is left-aligned on the content column.
/// Code and tables keep the full column width.
pub(super) fn render_markdown(
    id: SharedString,
    text: SharedString,
    theme: &Theme,
    stream_fade: bool,
    window: &Window,
) -> impl IntoElement {
    let blocks = split_markdown(text.as_ref());
    let last = blocks.len().saturating_sub(1);
    div()
        .id(SharedString::from(format!("reply-shell-{id}")))
        .w_full()
        .min_w_0()
        .h_auto()
        .flex_none()
        .flex()
        .flex_col()
        .gap(skin::space(2))
        .test_support()
        .children(blocks.into_iter().enumerate().map(|(index, block)| {
            let fade = stream_fade && index == last;
            match block {
                MarkdownBlock::Prose(body) => {
                    prose_measure(text_view(format!("{id}-p{index}"), body, fade))
                        .into_any_element()
                }
                MarkdownBlock::Code(body) => div()
                    .w_full()
                    .min_w_0()
                    .child(text_view(format!("{id}-c{index}"), body, fade))
                    .into_any_element(),
                MarkdownBlock::Table(rows) => {
                    render_table(format!("{id}-t{index}"), &rows, theme, window).into_any_element()
                }
            }
        }))
}

/// Left-aligns a block on the same edge as the tool cards.
///
/// `max_w` only caps an ultrawide pane. The block is not centered, so a
/// normal chat column matches the tool-card width.
pub(super) fn prose_measure(child: impl IntoElement) -> impl IntoElement {
    div()
        .w_full()
        .max_w(skin::prose_max())
        .min_w_0()
        .child(child)
}

fn text_view(id: String, text: String, stream_fade: bool) -> impl IntoElement {
    TextView::markdown(SharedString::from(id), SharedString::from(text))
        .style(prose_style())
        .selectable(true)
        .stream_fade(stream_fade)
}

fn prose_style() -> TextViewStyle {
    let mut table = gpui_kit::StyleRefinement::default();
    table.overflow.x = Some(gpui_kit::Overflow::Scroll);
    let mut cell = gpui_kit::StyleRefinement::default();
    cell.text.white_space = Some(gpui_kit::WhiteSpace::Nowrap);
    // Headings stay close to body size. The kit default is 2rem / 1.5rem,
    // which reads as a title page inside a chat turn.
    TextViewStyle::default()
        .paragraph_gap(rems(0.45))
        .heading_font_size(|level, base| {
            let scale = match level {
                1 => 1.28,
                2 => 1.16,
                3 => 1.08,
                _ => 1.0,
            };
            px(base.as_f32() * scale)
        })
        .table(table)
        .table_cell(cell)
}

fn render_table(
    id: String,
    rows: &[Vec<String>],
    theme: &Theme,
    window: &Window,
) -> impl IntoElement {
    let Some(header) = rows.first() else {
        return div().into_any_element();
    };
    let widths = column_widths(rows, theme, window.text_system());
    let min_width: f32 = widths.iter().sum();
    let body = &rows[1..];
    let body_id = format!("{id}-body");
    div()
        .id(SharedString::from(id))
        .w_full()
        .min_w_0()
        .overflow_x_scroll()
        .rounded(skin::radius_sm())
        .border_1()
        .border_color(theme.border)
        .child(
            div()
                .min_w(px(min_width))
                .flex()
                .flex_col()
                .child(table_row(header, &widths, true, false, theme))
                .child(
                    div()
                        .id(SharedString::from(body_id))
                        .max_h(px(280.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .children(body.iter().enumerate().map(|(index, row)| {
                            table_row(row, &widths, false, index % 2 == 1, theme)
                        })),
                ),
        )
        .into_any_element()
}

fn column_widths(rows: &[Vec<String>], theme: &Theme, text_system: &WindowTextSystem) -> Vec<f32> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![88.0_f32; columns];
    for (index, row) in rows.iter().enumerate() {
        let weight = if index == 0 {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        };
        for (column, cell) in row.iter().enumerate() {
            let width = inline_width(&inline_spans(cell), weight, theme, text_system);
            widths[column] = widths[column].max((f32::from(width) + 28.).ceil());
        }
    }
    widths
}

/// Use the same font, weight, size, and inline-code padding as the painted
/// spans. A fixed Latin character estimate clipped CJK and overlapped cells
/// after changing the interface font or choosing the larger text sizes.
fn inline_width(
    spans: &[InlineSpan],
    weight: FontWeight,
    theme: &Theme,
    text_system: &WindowTextSystem,
) -> gpui_kit::Pixels {
    spans
        .iter()
        .map(|span| {
            let (text, family, padding) = match span {
                InlineSpan::Bold(inner) => {
                    return inline_width(inner, FontWeight::BOLD, theme, text_system);
                }
                InlineSpan::Text(text) => (text, theme.font_family.clone(), px(0.)),
                InlineSpan::Code(text) => (text, theme.mono_font_family.clone(), px(6.)),
            };
            let mut font = gpui_kit::font(family);
            font.weight = weight;
            text_system
                .shape_line(
                    SharedString::from(text.clone()),
                    theme.font_size * 0.875,
                    &[gpui_kit::TextRun {
                        len: text.len(),
                        font,
                        ..Default::default()
                    }],
                    None,
                )
                .width()
                + padding
        })
        .sum()
}

/// One inline run inside a table cell.
#[derive(Clone, Debug, PartialEq, Eq)]
enum InlineSpan {
    Text(String),
    /// `**...**`. Nested bold is left literal; inline code inside is parsed.
    Bold(Vec<InlineSpan>),
    /// `` `...` ``. The backticks are not part of the text.
    Code(String),
}

/// Splits a cell into bold, inline code, and plain text.
///
/// An unclosed `**` or backtick stays literal so a streaming cell is not
/// eaten early. Markers need a non-empty interior.
fn inline_spans(source: &str) -> Vec<InlineSpan> {
    parse_inline(source, true)
}

fn parse_inline(source: &str, allow_bold: bool) -> Vec<InlineSpan> {
    let bytes = source.as_bytes();
    let mut spans = Vec::new();
    let mut text_start = 0;
    let mut index = 0;
    while index < bytes.len() {
        let bold = allow_bold && bytes[index..].starts_with(b"**");
        if bold
            && let Some(rel) = source[index + 2..].find("**")
            && rel > 0
        {
            push_text(&mut spans, source, text_start, index);
            let inner = &source[index + 2..index + 2 + rel];
            spans.push(InlineSpan::Bold(parse_inline(inner, false)));
            index += 2 + rel + 2;
            text_start = index;
            continue;
        }
        if bytes[index] == b'`'
            && let Some(rel) = source[index + 1..].find('`')
            && rel > 0
        {
            push_text(&mut spans, source, text_start, index);
            spans.push(InlineSpan::Code(
                source[index + 1..index + 1 + rel].to_owned(),
            ));
            index += 1 + rel + 1;
            text_start = index;
            continue;
        }
        let step = source[index..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
        index += step;
    }
    push_text(&mut spans, source, text_start, source.len());
    spans
}

fn push_text(spans: &mut Vec<InlineSpan>, source: &str, start: usize, end: usize) {
    if start < end {
        spans.push(InlineSpan::Text(source[start..end].to_owned()));
    }
}

fn render_inline(source: &str, theme: &Theme) -> impl IntoElement {
    div().flex().flex_row().items_center().min_w_0().children(
        inline_spans(source)
            .into_iter()
            .map(|span| render_span(span, theme)),
    )
}

fn render_span(span: InlineSpan, theme: &Theme) -> AnyElement {
    match span {
        InlineSpan::Text(text) => div().child(text).into_any_element(),
        InlineSpan::Code(text) => div()
            .font_family(theme.mono_font_family.clone())
            .px(px(3.))
            .rounded(px(3.))
            .bg(theme.secondary)
            .child(text)
            .into_any_element(),
        InlineSpan::Bold(parts) => div()
            .font_weight(FontWeight::BOLD)
            .flex()
            .flex_row()
            .items_center()
            .children(parts.into_iter().map(|part| render_span(part, theme)))
            .into_any_element(),
    }
}

fn table_row(
    cells: &[String],
    widths: &[f32],
    header: bool,
    zebra: bool,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .w_full()
        .flex_none()
        .border_b_1()
        .border_color(theme.border)
        .when(header, |this| {
            this.bg(theme.table_head)
                .text_color(theme.table_head_foreground)
                .font_weight(FontWeight::MEDIUM)
        })
        .when(!header && zebra, |this| this.bg(theme.table_even))
        .when(!header && !zebra, |this| this.bg(theme.table))
        .children(widths.iter().enumerate().map(|(index, width)| {
            let text = cells.get(index).cloned().unwrap_or_default();
            let last = index + 1 == widths.len();
            div()
                .w(px(*width))
                .flex_shrink_0()
                .px(skin::space(2))
                .py(skin::space(1))
                .text_sm()
                .whitespace_nowrap()
                .when(!last, |this| this.border_r_1().border_color(theme.border))
                .child(render_inline(&text, theme))
        }))
}

#[cfg(test)]
mod tests {
    use super::{MarkdownBlock, split_markdown};

    #[test]
    fn prose_code_and_tables_split_onto_their_own_blocks() {
        let source = "Hello\n\n# Title\n\n```rust\nfn main() {}\n```\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |\n\nAfter";
        let blocks = split_markdown(source);
        assert!(
            matches!(blocks.first(), Some(MarkdownBlock::Prose(text)) if text.contains("# Title"))
        );
        assert!(
            matches!(blocks.get(1), Some(MarkdownBlock::Code(text)) if text.contains("fn main"))
        );
        match blocks.get(2) {
            Some(MarkdownBlock::Table(rows)) => {
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[0], vec!["a".to_owned(), "b".to_owned()]);
                assert_eq!(rows[2], vec!["3".to_owned(), "4".to_owned()]);
            }
            other => panic!("expected a table, got {other:?}"),
        }
        assert!(matches!(blocks.get(3), Some(MarkdownBlock::Prose(text)) if text == "After"));
    }

    #[test]
    fn an_open_fence_and_a_header_without_a_separator_stay_intact() {
        let streaming = "words\n```\nlet x = 1\n| not | a table |";
        let blocks = split_markdown(streaming);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[1], MarkdownBlock::Code(text) if text.contains("let x")));
        let header = "intro\n| col | other |\n";
        let blocks = split_markdown(header);
        assert!(matches!(blocks.first(), Some(MarkdownBlock::Prose(_))));
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn longer_fences_keep_shorter_markers_and_tables_inside_code() {
        let source = "````markdown\n```rust\nlet value = 1;\n```\n| code | only |\n| --- | --- |\n`````\n\nAfter";
        let blocks = split_markdown(source);
        assert_eq!(blocks.len(), 2);
        assert!(
            matches!(&blocks[0], MarkdownBlock::Code(text) if text.contains("| code | only |") && text.ends_with("`````"))
        );
        assert_eq!(blocks[1], MarkdownBlock::Prose("After".into()));
        let blocks = split_markdown("~~~~text\n~~~\nbody\n~~~~\nAfter");
        assert_eq!(blocks.len(), 2);
        assert!(
            matches!(&blocks[0], MarkdownBlock::Code(text) if text.contains("body") && text.ends_with("~~~~"))
        );
    }

    #[test]
    fn escaped_table_pipes_stay_in_their_cells() {
        let source = "| expression | meaning |\n| --- | --- |\n| `a\\|b` | left\\|right |\n| ending\\| | slash\\\\|";
        let blocks = split_markdown(source);
        assert_eq!(blocks.len(), 1);
        let MarkdownBlock::Table(rows) = &blocks[0] else {
            panic!("expected a table");
        };
        assert_eq!(rows[1], ["`a|b`", "left|right"]);
        assert_eq!(rows[2], ["ending|", "slash\\\\"]);
    }

    #[test]
    fn tables_require_matching_header_columns_and_normalize_body_rows() {
        let mismatched = "| one | two |\n| --- |\nnot a table";
        assert_eq!(
            split_markdown(mismatched),
            [MarkdownBlock::Prose(mismatched.into())]
        );
        let invalid = "| one | two |\n| -:- | --- |";
        assert_eq!(
            split_markdown(invalid),
            [MarkdownBlock::Prose(invalid.into())]
        );
        let blocks = split_markdown("| one | two |\n| --- | :---: |\n| a | b | ignored |\n| c |");
        let MarkdownBlock::Table(rows) = &blocks[0] else {
            panic!("expected a table");
        };
        assert_eq!(rows[1], ["a", "b"]);
        assert_eq!(rows[2], ["c", ""]);
    }

    #[test]
    fn long_blank_prefixes_do_not_become_empty_blocks() {
        let source = format!("{}body\n\n", "\n".repeat(4096));
        assert_eq!(
            split_markdown(&source),
            [MarkdownBlock::Prose("body".into())]
        );
    }

    #[test]
    fn table_cells_render_bold_and_inline_code_without_markers() {
        use super::{InlineSpan, inline_spans};

        assert_eq!(
            inline_spans("**Tokio**"),
            vec![InlineSpan::Bold(vec![InlineSpan::Text("Tokio".into())])]
        );
        assert_eq!(inline_spans("`mio`"), vec![InlineSpan::Code("mio".into())]);
        assert_eq!(
            inline_spans("uses `mio` and **Tokio**"),
            vec![
                InlineSpan::Text("uses ".into()),
                InlineSpan::Code("mio".into()),
                InlineSpan::Text(" and ".into()),
                InlineSpan::Bold(vec![InlineSpan::Text("Tokio".into())]),
            ]
        );
        assert_eq!(
            inline_spans("**bold `code`**"),
            vec![InlineSpan::Bold(vec![
                InlineSpan::Text("bold ".into()),
                InlineSpan::Code("code".into()),
            ])]
        );
        assert_eq!(
            inline_spans("**not closed"),
            vec![InlineSpan::Text("**not closed".into())]
        );
        assert_eq!(
            inline_spans("`not closed"),
            vec![InlineSpan::Text("`not closed".into())]
        );
        assert_eq!(
            inline_spans("运行时 `mio`"),
            vec![
                InlineSpan::Text("运行时 ".into()),
                InlineSpan::Code("mio".into()),
            ]
        );
    }
}
