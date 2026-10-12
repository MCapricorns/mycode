//! Assistant markdown: prose is left-aligned with the tool cards, while fenced
//! code and tables use the full column. Tables scroll sideways, stripe their rows,
//! and keep the header pinned above a scrolling body.
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::theme::Theme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt as _, div, px, rems,
};

use crate::ui::skin;

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
        if is_fence(line) {
            flush_prose(&mut prose, &mut blocks);
            let marker = fence_marker(line);
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
    while prose.first().is_some_and(|line| line.trim().is_empty()) {
        prose.remove(0);
    }
    while prose.last().is_some_and(|line| line.trim().is_empty()) {
        prose.pop();
    }
    if prose.is_empty() {
        return;
    }
    let text = prose.join("\n");
    prose.clear();
    blocks.push(MarkdownBlock::Prose(text));
}

fn is_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

fn fence_marker(line: &str) -> &str {
    let trimmed = line.trim_start();
    if trimmed.starts_with("~~~") {
        "~~~"
    } else {
        "```"
    }
}

fn is_closing_fence(line: &str, marker: &str) -> bool {
    let trimmed = line.trim();
    trimmed == marker || (trimmed.starts_with(marker) && trimmed.len() == marker.len())
}

struct ParsedTable {
    rows: Vec<Vec<String>>,
    span: usize,
}

fn table_at(lines: &[&str], start: usize) -> Option<ParsedTable> {
    if start + 1 >= lines.len() {
        return None;
    }
    if !looks_like_table_row(lines[start]) || !is_separator_row(lines[start + 1]) {
        return None;
    }
    let mut rows = vec![split_row(lines[start])];
    let mut index = start + 2;
    while index < lines.len()
        && looks_like_table_row(lines[index])
        && !is_separator_row(lines[index])
    {
        rows.push(split_row(lines[index]));
        index += 1;
    }
    Some(ParsedTable {
        rows,
        span: index - start,
    })
}

fn looks_like_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && trimmed.contains('|')
}

fn is_separator_row(line: &str) -> bool {
    let trimmed = line.trim().trim_matches('|');
    if !trimmed.contains('-') {
        return false;
    }
    trimmed.split('|').all(|cell| {
        let cell = cell.trim();
        !cell.is_empty() && cell.chars().all(|ch| matches!(ch, '-' | ':'))
    })
}

fn split_row(line: &str) -> Vec<String> {
    let mut body = line.trim();
    if let Some(rest) = body.strip_prefix('|') {
        body = rest;
    }
    if let Some(rest) = body.strip_suffix('|') {
        body = rest;
    }
    body.split('|').map(|cell| cell.trim().to_owned()).collect()
}

/// Renders assistant markdown. Prose is left-aligned on the content column.
/// Code and tables keep the full column width.
pub(super) fn render_markdown(
    id: SharedString,
    text: SharedString,
    theme: &Theme,
    stream_fade: bool,
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
                    prose_measure(text_view(format!("{id}-p{index}"), body, theme, fade))
                        .into_any_element()
                }
                MarkdownBlock::Code(body) => div()
                    .w_full()
                    .min_w_0()
                    .child(text_view(format!("{id}-c{index}"), body, theme, fade))
                    .into_any_element(),
                MarkdownBlock::Table(rows) => {
                    render_table(format!("{id}-t{index}"), &rows, theme).into_any_element()
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

fn text_view(id: String, text: String, _theme: &Theme, stream_fade: bool) -> impl IntoElement {
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

fn render_table(id: String, rows: &[Vec<String>], theme: &Theme) -> impl IntoElement {
    let Some(header) = rows.first() else {
        return div().into_any_element();
    };
    let widths = column_widths(rows);
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

fn column_widths(rows: &[Vec<String>]) -> Vec<f32> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    (0..columns)
        .map(|column| {
            let chars = rows
                .iter()
                .map(|row| {
                    row.get(column)
                        .map(|cell| cell.chars().count())
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(1);
            (chars as f32).mul_add(7.4, 28.).max(88.)
        })
        .collect()
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
                .child(text)
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
}
