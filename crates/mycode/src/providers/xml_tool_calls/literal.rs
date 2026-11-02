//! Tracks literal Markdown without interpreting examples as tool calls.
//!
//! Delimiter runs are counted across chunks rather than buffered, so a long
//! backtick run cannot cause repeated rescanning or unbounded held text.

use super::{CallTag, OPEN_MINIMAX, OPEN_TOOL, holds_partial_open};

pub(super) struct TextScan {
    /// Prefix consumed as ordinary text, before a call or an incomplete tag.
    pub safe: usize,
    pub call: Option<CallTag>,
}

#[derive(Clone, Copy)]
struct Fence {
    marker: char,
    length: usize,
    container_indent: usize,
}

struct MarkerRun {
    marker: char,
    length: usize,
    line_start: bool,
    container_indent: usize,
}

#[derive(Clone, Copy)]
enum ListMarker {
    Bullet,
    Digits(u8),
    OrderedEnd,
}

pub(super) struct LiteralContext {
    fence: Option<Fence>,
    inline_ticks: Option<usize>,
    run: Option<MarkerRun>,
    /// A closing fence is valid only if the rest of its line is whitespace.
    closing_fence: bool,
    /// Leading columns; None after the first non-space.
    indent: Option<usize>,
    column: usize,
    /// Column immediately after a list prefix on the current line.
    container_indent: usize,
    /// Explicit blockquotes and indented code remain literal until newline.
    literal_line: bool,
    escaped: bool,
    /// A list marker may precede a fenced block, even across chunks.
    list_marker: Option<ListMarker>,
}

impl Default for LiteralContext {
    fn default() -> Self {
        Self {
            fence: None,
            inline_ticks: None,
            run: None,
            closing_fence: false,
            indent: Some(0),
            column: 0,
            container_indent: 0,
            literal_line: false,
            escaped: false,
            list_marker: None,
        }
    }
}

impl LiteralContext {
    pub(super) fn scan(&mut self, text: &str) -> TextScan {
        self.scan_text(text, true)
    }

    /// Rejected XML is visible text. Track its Markdown as well, without
    /// turning any nested markup in that rejected body into another call.
    pub(super) fn record_text(&mut self, text: &str) {
        self.scan_text(text, false);
    }

    fn scan_text(&mut self, text: &str, extract_calls: bool) -> TextScan {
        let mut cursor = 0;
        while cursor < text.len() {
            let tail = &text[cursor..];
            let ch = tail.chars().next().expect("cursor is before the end");
            if let Some(run) = &mut self.run {
                if ch == run.marker {
                    run.length = run.length.saturating_add(1);
                    self.advance(ch);
                    cursor += ch.len_utf8();
                    continue;
                }
                self.finish_run();
            }
            if self.closing_fence {
                if matches!(ch, '\n' | '\r') {
                    self.fence = None;
                    self.closing_fence = false;
                } else if !matches!(ch, ' ' | '\t') {
                    self.closing_fence = false;
                }
            }
            if self.consume_list_marker(ch) {
                cursor += ch.len_utf8();
                continue;
            }

            let allowed_indent = self
                .fence
                .map_or(3, |fence| fence.container_indent.saturating_add(3));
            let line_start = self.indent.is_some_and(|columns| columns <= allowed_indent);
            if let Some(fence) = self.fence {
                if line_start && ch == fence.marker {
                    self.start_run(ch, true);
                }
            } else if self.inline_ticks.is_some() {
                if ch == '`' {
                    self.start_run(ch, false);
                }
            } else if self.literal_line {
                // Even valid XML in a quote or indented code is only text.
            } else if self.escaped {
                self.escaped = false;
            } else if ch == '\\' {
                self.escaped = true;
            } else if line_start && ch == '>' {
                self.literal_line = true;
            } else if line_start && matches!(ch, '-' | '+' | '*') {
                self.list_marker = Some(ListMarker::Bullet);
            } else if line_start && ch.is_ascii_digit() {
                self.list_marker = Some(ListMarker::Digits(1));
            } else if ch == '`' || (line_start && ch == '~') {
                self.start_run(ch, line_start);
            } else if extract_calls && ch == '<' {
                let call = if tail.starts_with(OPEN_TOOL) {
                    Some(CallTag::Tool)
                } else if tail.starts_with(OPEN_MINIMAX) {
                    Some(CallTag::Minimax)
                } else {
                    None
                };
                if call.is_some() || holds_partial_open(tail) {
                    return TextScan { safe: cursor, call };
                }
            }
            self.advance(ch);
            cursor += ch.len_utf8();
        }
        TextScan {
            safe: cursor,
            call: None,
        }
    }

    fn start_run(&mut self, marker: char, line_start: bool) {
        self.run = Some(MarkerRun {
            marker,
            length: 1,
            line_start,
            container_indent: self.container_indent,
        });
    }

    /// After `- ` or `1. `, Markdown may start a nested fence. Keep this
    /// small prefix state instead of buffering or rescanning whole lines.
    fn consume_list_marker(&mut self, ch: char) -> bool {
        match (self.list_marker.take(), ch) {
            (Some(ListMarker::Bullet | ListMarker::OrderedEnd), ' ' | '\t') => {
                self.advance(ch);
                self.indent = Some(0);
                self.container_indent = self.column;
                true
            }
            (Some(ListMarker::Digits(count)), '0'..='9') if count < 9 => {
                self.list_marker = Some(ListMarker::Digits(count + 1));
                self.advance(ch);
                true
            }
            (Some(ListMarker::Digits(_)), '.' | ')') => {
                self.list_marker = Some(ListMarker::OrderedEnd);
                self.advance(ch);
                true
            }
            _ => false,
        }
    }

    fn finish_run(&mut self) {
        let run = self.run.take().expect("a marker run is pending");
        if let Some(fence) = self.fence {
            self.closing_fence =
                run.line_start && run.marker == fence.marker && run.length >= fence.length;
        } else if let Some(ticks) = self.inline_ticks {
            if run.length == ticks {
                self.inline_ticks = None;
            }
        } else if run.line_start && run.length >= 3 {
            self.fence = Some(Fence {
                marker: run.marker,
                length: run.length,
                container_indent: run.container_indent,
            });
        } else if run.marker == '`' {
            self.inline_ticks = Some(run.length);
        }
    }

    fn advance(&mut self, ch: char) {
        match ch {
            '\n' | '\r' => {
                self.indent = Some(0);
                self.column = 0;
                self.container_indent = 0;
                self.literal_line = false;
                self.escaped = false;
                self.list_marker = None;
            }
            _ => {
                let width = if ch == '\t' { 4 - self.column % 4 } else { 1 };
                self.column = self.column.saturating_add(width);
                if matches!(ch, ' ' | '\t') && self.indent.is_some() {
                    let columns = self.indent.unwrap_or(0).saturating_add(width);
                    self.indent = Some(columns);
                    self.literal_line |= columns >= 4;
                } else {
                    self.indent = None;
                }
            }
        }
    }

    /// XML arguments are protocol data, not Markdown. A consumed closing tag
    /// ends in `>`, so subsequent text is no longer at the start of its line.
    pub(super) fn end_xml(&mut self) {
        self.indent = None;
        self.escaped = false;
        self.list_marker = None;
    }
}
