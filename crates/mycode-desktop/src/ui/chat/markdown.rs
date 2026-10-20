//! Assistant markdown: prose is left-aligned with the tool cards, while fenced
//! code and tables use the full column. Tables scroll sideways, stripe their rows,
//! and keep the header pinned above a scrolling body. Bold and inline code inside
//! a cell render as text styles; the markers themselves are not shown.
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::theme::Theme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels, ScrollHandle,
    SharedString, StatefulInteractiveElement, Styled, TestSupportExt as _, Window,
    WindowTextSystem, div, px, rems,
};

use crate::ui::skin;

#[cfg(all(test, feature = "test-support"))]
mod layout_tests;

/// Maximum number of resident entries cached in the renderer's bounded
/// markdown/table cache. Matches the resident conversation window ceiling
/// plus streaming headroom so old entries are evicted in FIFO order.
const MAX_CACHED_MARKDOWN_ENTRIES: usize = 1536;

/// Bounded per-entry cache for parsed markdown blocks and measured table column
/// widths. Owned by the renderer so no global or unbounded cache is retained.
#[derive(Clone, Default)]
pub(crate) struct MarkdownCache {
    inner: Rc<RefCell<MarkdownCacheStore>>,
}

#[derive(Default)]
struct MarkdownCacheStore {
    order: VecDeque<SharedString>,
    entries: HashMap<SharedString, CachedMarkdownEntry>,
}

struct CachedMarkdownEntry {
    source: SharedString,
    blocks: Vec<MarkdownBlock>,
    tables: Vec<Option<CachedTableLayout>>,
    scrolls: Vec<Option<TableScroll>>,
}

/// Per-table scroll handles, kept in the entry cache so pan offsets survive
/// rerenders. `x` pans the whole frame sideways; `y` pans the row body under
/// the pinned header.
#[derive(Clone, Default)]
struct TableScroll {
    x: ScrollHandle,
    y: ScrollHandle,
}

#[derive(Clone)]
struct CachedTableLayout {
    font_family: SharedString,
    mono_font_family: SharedString,
    font_size_bits: u32,
    widths: Vec<f32>,
}

impl MarkdownCache {
    /// Returns the parsed blocks for `id` and `source`, reusing the cached
    /// split (and its `SharedString` allocations) when `source` has not changed.
    fn blocks_for(&self, id: &SharedString, source: &SharedString) -> Vec<MarkdownBlock> {
        let mut store = self.inner.borrow_mut();
        if let Some(entry) = store.entries.get(id)
            && entry.source == *source
        {
            return entry.blocks.clone();
        }
        let blocks = split_markdown(source.as_ref());
        let tables = vec![None; blocks.len()];
        // A streaming reply changes its source on every delta. Keeping each
        // table's scroll handles stops a table the reader panned from
        // snapping back while text below it is still arriving.
        let scrolls = blocks
            .iter()
            .enumerate()
            .map(|(index, block)| match block {
                MarkdownBlock::Table(_) => store
                    .entries
                    .get(id)
                    .and_then(|entry| entry.scrolls.get(index).cloned().flatten()),
                MarkdownBlock::Prose(_) | MarkdownBlock::Code(_) => None,
            })
            .collect();
        if !store.entries.contains_key(id) {
            if store.order.len() >= MAX_CACHED_MARKDOWN_ENTRIES
                && let Some(oldest) = store.order.pop_front()
            {
                store.entries.remove(&oldest);
            }
            store.order.push_back(id.clone());
        }
        store.entries.insert(
            id.clone(),
            CachedMarkdownEntry {
                source: source.clone(),
                blocks: blocks.clone(),
                tables,
                scrolls,
            },
        );
        blocks
    }

    /// Returns cached table column widths when the entry text, font families,
    /// and font size match, or shapes and stores them.
    fn table_widths_for(
        &self,
        id: &SharedString,
        block_index: usize,
        table: &ParsedTableBlock,
        theme: &Theme,
        text_system: &WindowTextSystem,
    ) -> Vec<f32> {
        let font_size_bits = f32::from(theme.font_size).to_bits();
        if let Some(entry) = self.inner.borrow().entries.get(id)
            && let Some(Some(cached)) = entry.tables.get(block_index)
            && cached.font_size_bits == font_size_bits
            && cached.font_family == theme.font_family
            && cached.mono_font_family == theme.mono_font_family
        {
            return cached.widths.clone();
        }
        let widths = column_widths_from_spans(&table.spans, theme, text_system);
        if let Some(entry) = self.inner.borrow_mut().entries.get_mut(id)
            && let Some(slot) = entry.tables.get_mut(block_index)
        {
            *slot = Some(CachedTableLayout {
                font_family: theme.font_family.clone(),
                mono_font_family: theme.mono_font_family.clone(),
                font_size_bits,
                widths: widths.clone(),
            });
        }
        widths
    }

    /// Returns the scroll handles for one table block, creating them on first
    /// use so pan offsets survive rerenders.
    fn table_scroll_for(&self, id: &SharedString, block_index: usize) -> Option<TableScroll> {
        let mut store = self.inner.borrow_mut();
        let slot = store.entries.get_mut(id)?.scrolls.get_mut(block_index)?;
        Some(slot.get_or_insert_with(TableScroll::default).clone())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.borrow().entries.len()
    }
}

/// One parsed table block with both raw cell text and pre-parsed inline spans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ParsedTableBlock {
    pub(super) rows: Vec<Vec<String>>,
    spans: Vec<Vec<Vec<InlineSpan>>>,
}

/// One block after the prose / code / table split.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MarkdownBlock {
    /// Paragraphs, headings, lists, and anything that is not a fence or table.
    Prose(SharedString),
    /// A fenced block, including its opening and closing markers.
    Code(SharedString),
    /// Header row first, then body rows. Cells are trimmed and pre-split into
    /// inline spans.
    Table(Rc<ParsedTableBlock>),
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
            blocks.push(MarkdownBlock::Code(fence.join("\n").into()));
            continue;
        }
        if let Some(table) = table_at(&lines, index) {
            flush_prose(&mut prose, &mut blocks);
            index += table.span;
            let spans = table
                .rows
                .iter()
                .map(|row| row.iter().map(|cell| inline_spans(cell)).collect())
                .collect();
            blocks.push(MarkdownBlock::Table(Rc::new(ParsedTableBlock {
                rows: table.rows,
                spans,
            })));
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
        blocks.push(MarkdownBlock::Prose(prose[start..=end].join("\n").into()));
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
    cache: Option<&MarkdownCache>,
    theme: &Theme,
    stream_fade: bool,
    window: &Window,
) -> impl IntoElement {
    let blocks = match cache {
        Some(cache) => cache.blocks_for(&id, &text),
        None => split_markdown(text.as_ref()),
    };
    render_blocks(id, blocks, cache, theme, stream_fade, window)
}

fn render_blocks(
    id: SharedString,
    blocks: Vec<MarkdownBlock>,
    cache: Option<&MarkdownCache>,
    theme: &Theme,
    stream_fade: bool,
    window: &Window,
) -> impl IntoElement {
    let last = blocks.len().saturating_sub(1);
    let ink = crate::ui::desk::Desk::of(theme).reply;
    div()
        .id(SharedString::from(format!("reply-shell-{id}")))
        .w_full()
        .min_w_0()
        .h_auto()
        .flex_none()
        .flex()
        .flex_col()
        .gap(skin::space(2))
        .text_color(ink)
        .test_support()
        .children(blocks.into_iter().enumerate().map(|(index, block)| {
            let fade = stream_fade && index == last;
            match block {
                MarkdownBlock::Prose(body) => {
                    prose_measure(text_view(format!("{id}-p{index}"), body, ink, fade))
                        .into_any_element()
                }
                MarkdownBlock::Code(body) => div()
                    .w_full()
                    .min_w_0()
                    .child(text_view(format!("{id}-c{index}"), body, ink, fade))
                    .into_any_element(),
                MarkdownBlock::Table(table) => {
                    let widths = match cache {
                        Some(cache) => {
                            cache.table_widths_for(&id, index, &table, theme, window.text_system())
                        }
                        None => column_widths_from_spans(&table.spans, theme, window.text_system()),
                    };
                    let scroll = cache.and_then(|cache| cache.table_scroll_for(&id, index));
                    render_table(format!("{id}-t{index}"), &table, &widths, theme, scroll)
                        .into_any_element()
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

/// `ink` overrides the theme foreground the TextView root paints with, so
/// paragraphs, headings, and code without a token color take the reply tint.
fn text_view(
    id: String,
    text: SharedString,
    ink: gpui_kit::Hsla,
    stream_fade: bool,
) -> impl IntoElement {
    TextView::markdown(SharedString::from(id), text)
        .style(prose_style().clone())
        .text_color(ink)
        .selectable(true)
        .stream_fade(stream_fade)
}

fn prose_style() -> &'static TextViewStyle {
    static STYLE: std::sync::LazyLock<TextViewStyle> = std::sync::LazyLock::new(|| {
        let mut table = gpui_kit::StyleRefinement::default();
        table.overflow.x = Some(gpui_kit::Overflow::Scroll);
        let mut cell = gpui_kit::StyleRefinement::default();
        cell.text.white_space = Some(gpui_kit::WhiteSpace::Nowrap);
        // Headings stay close to body size and rely on semibold weight rather
        // than huge size jumps, matching chat-scale typography.
        TextViewStyle::default()
            .paragraph_gap(rems(0.45))
            .heading_font_size(|level, base| {
                let scale = match level {
                    1 => 1.18,
                    2 => 1.10,
                    3 => 1.04,
                    _ => 1.0,
                };
                px(base.as_f32() * scale)
            })
            .table(table)
            .table_cell(cell)
    });
    &STYLE
}

fn render_table(
    id: String,
    table: &ParsedTableBlock,
    widths: &[f32],
    theme: &Theme,
    scroll: Option<TableScroll>,
) -> impl IntoElement {
    let Some(header_spans) = table.spans.first() else {
        return div().into_any_element();
    };
    let min_width: f32 = widths.iter().sum();
    let body_spans = &table.spans[1..];
    let body_id = format!("{id}-body");
    let scroll = scroll.unwrap_or_default();
    let body_scroll = scroll.y.clone();
    div()
        .w_full()
        .min_w_0()
        .flex_none()
        .relative()
        .child(
            div()
                .id(SharedString::from(id.clone()))
                .w_full()
                .min_w_0()
                .overflow_x_scroll()
                .restrict_scroll_to_axis()
                .track_scroll(&scroll.x)
                .on_scroll_wheel(move |event, window, cx| {
                    let delta = event.delta.pixel_delta(window.line_height());
                    if delta.x != px(0.) {
                        // The frame owns horizontal deltas; left to bubble the
                        // transcript remaps them into a vertical page scroll.
                        cx.stop_propagation();
                        return;
                    }
                    if delta.y == px(0.)
                        || body_scroll.max_offset().y <= px(0.)
                        || !body_scroll.bounds().contains(&event.position)
                    {
                        return;
                    }
                    // The body's own listener already applied this delta.
                    // Chain the gesture to the transcript once the body sits
                    // on its limit instead of scrolling both at once.
                    let offset_y = body_scroll.offset().y;
                    let max_y = body_scroll.max_offset().y;
                    let chained = (delta.y < px(0.) && offset_y <= -max_y)
                        || (delta.y > px(0.) && offset_y >= px(0.));
                    if !chained {
                        cx.stop_propagation();
                    }
                })
                .rounded(px(10.))
                .border_1()
                .border_color(theme.border)
                .child(
                    div()
                        .min_w(px(min_width))
                        .flex()
                        .flex_col()
                        .child(table_row(header_spans, widths, true, false, theme))
                        .child(
                            div()
                                .id(SharedString::from(body_id))
                                .max_h(px(280.))
                                .overflow_y_scroll()
                                .restrict_scroll_to_axis()
                                .track_scroll(&scroll.y)
                                .flex()
                                .flex_col()
                                .children(body_spans.iter().enumerate().map(
                                    |(index, row_spans)| {
                                        table_row(row_spans, widths, false, index % 2 == 1, theme)
                                    },
                                )),
                        ),
                ),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .child(Scrollbar::horizontal(&scroll.x).id(format!("{id}-sb-x")))
                .child(Scrollbar::vertical(&scroll.y).id(format!("{id}-sb-y"))),
        )
        .into_any_element()
}

#[cfg(all(test, feature = "test-support"))]
pub(super) fn column_widths(
    rows: &[Vec<String>],
    theme: &Theme,
    text_system: &WindowTextSystem,
) -> Vec<f32> {
    let parsed_rows: Vec<Vec<Vec<InlineSpan>>> = rows
        .iter()
        .map(|row| row.iter().map(|cell| inline_spans(cell)).collect())
        .collect();
    column_widths_from_spans(&parsed_rows, theme, text_system)
}

fn column_widths_from_spans(
    rows: &[Vec<Vec<InlineSpan>>],
    theme: &Theme,
    text_system: &WindowTextSystem,
) -> Vec<f32> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![88.0_f32; columns];
    for (index, row) in rows.iter().enumerate() {
        let weight = if index == 0 {
            crate::ui::skin::WEIGHT_STRONG
        } else {
            FontWeight::NORMAL
        };
        for (column, spans) in row.iter().enumerate() {
            let width = inline_width(spans, weight, theme, text_system);
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
) -> Pixels {
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
                    text.clone(),
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
    Text(SharedString),
    /// `**...**`. Nested bold is left literal; inline code inside is parsed.
    Bold(Vec<InlineSpan>),
    /// `` `...` ``. The backticks are not part of the text.
    Code(SharedString),
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
                source[index + 1..index + 1 + rel].to_owned().into(),
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
        spans.push(InlineSpan::Text(source[start..end].to_owned().into()));
    }
}

fn render_inline(spans: &[InlineSpan], theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .min_w_0()
        .children(spans.iter().map(|span| render_span(span, theme)))
}

fn render_span(span: &InlineSpan, theme: &Theme) -> AnyElement {
    match span {
        InlineSpan::Text(text) => div().child(text.clone()).into_any_element(),
        InlineSpan::Code(text) => div()
            .font_family(theme.mono_font_family.clone())
            .px(px(3.))
            .rounded(px(3.))
            .bg(theme.secondary)
            .child(text.clone())
            .into_any_element(),
        InlineSpan::Bold(parts) => div()
            .font_weight(FontWeight::BOLD)
            .flex()
            .flex_row()
            .items_center()
            .children(parts.iter().map(|part| render_span(part, theme)))
            .into_any_element(),
    }
}

fn table_row(
    row_spans: &[Vec<InlineSpan>],
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
                .font_weight(crate::ui::skin::WEIGHT_STRONG)
        })
        .when(!header && zebra, |this| this.bg(theme.table_even))
        .when(!header && !zebra, |this| this.bg(theme.table))
        .children(widths.iter().enumerate().map(|(index, width)| {
            let empty = Vec::new();
            let spans = row_spans.get(index).unwrap_or(&empty);
            let last = index + 1 == widths.len();
            div()
                .w(px(*width))
                .flex_shrink_0()
                .px(skin::space(2))
                .py(skin::space(1))
                .text_sm()
                .whitespace_nowrap()
                .when(!last, |this| this.border_r_1().border_color(theme.border))
                .child(render_inline(spans, theme))
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
            Some(MarkdownBlock::Table(table)) => {
                assert_eq!(table.rows.len(), 3);
                assert_eq!(table.rows[0], vec!["a".to_owned(), "b".to_owned()]);
                assert_eq!(table.rows[2], vec!["3".to_owned(), "4".to_owned()]);
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
        let MarkdownBlock::Table(table) = &blocks[0] else {
            panic!("expected a table");
        };
        assert_eq!(table.rows[1], ["`a|b`", "left|right"]);
        assert_eq!(table.rows[2], ["ending|", "slash\\\\"]);
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
        let MarkdownBlock::Table(table) = &blocks[0] else {
            panic!("expected a table");
        };
        assert_eq!(table.rows[1], ["a", "b"]);
        assert_eq!(table.rows[2], ["c", ""]);
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

    #[test]
    fn bounded_markdown_cache_reuses_allocations_and_evicts_oldest() {
        use super::{MAX_CACHED_MARKDOWN_ENTRIES, MarkdownBlock, MarkdownCache};
        use gpui_kit::SharedString;

        let cache = MarkdownCache::default();
        let id = SharedString::from("entry-1");
        let text = SharedString::from(
            "A long paragraph exceeding inline SmolStr storage to verify pointer reuse across frames.",
        );
        let first = cache.blocks_for(&id, &text);
        let second = cache.blocks_for(&id, &text);
        let (Some(MarkdownBlock::Prose(p1)), Some(MarkdownBlock::Prose(p2))) =
            (first.first(), second.first())
        else {
            panic!("expected cached prose blocks");
        };
        assert_eq!(
            p1.as_ptr(),
            p2.as_ptr(),
            "unchanged entry must reuse the same SharedString allocation"
        );

        let updated = SharedString::from(
            "Updated paragraph exceeding inline SmolStr storage to verify cache invalidation on edit.",
        );
        let third = cache.blocks_for(&id, &updated);
        let Some(MarkdownBlock::Prose(p3)) = third.first() else {
            panic!("expected updated prose block");
        };
        assert_eq!(p3.as_str(), updated.as_str());

        for index in 0..(MAX_CACHED_MARKDOWN_ENTRIES + 32) {
            let key = SharedString::from(format!("id-{index}"));
            let body = SharedString::from(format!("row {index}"));
            let _ = cache.blocks_for(&key, &body);
        }
        assert_eq!(cache.len(), MAX_CACHED_MARKDOWN_ENTRIES);
    }
}
