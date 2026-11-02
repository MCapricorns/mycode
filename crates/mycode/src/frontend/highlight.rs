//! Lexical syntax coloring for fenced code, shared by every frontend.
//!
//! Not a parser: one pass over the bytes that recognizes comments, strings,
//! numbers, keywords, and a few per-language shapes (Rust attributes and
//! lifetimes, shell variables, JSON keys, diff lines, markup tags). Each
//! frontend maps a [`TokenKind`] to its own colors. It stands in for the
//! tree-sitter grammars that were dropped to halve the desktop binary. A
//! fence whose language is missing or unknown stays plain, so command output
//! is never painted as code.
use std::ops::Range;

#[cfg(test)]
mod tests;

/// What a colored span is. Bytes outside every span keep the body color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Keyword,
    /// Type names, builtin types, and TOML section headers.
    Type,
    /// A name in call or definition position.
    Function,
    String,
    /// Numbers, `true` / `false` / `null`, and `ALL_CAPS` constants.
    Number,
    Comment,
    /// Object keys, config keys, command-line flags, and markup attributes.
    Property,
    /// Rust macros and attributes, decorators, annotations, and
    /// preprocessor directives.
    Macro,
    /// Shell, PowerShell, and PHP `$variables`; Rust lifetimes.
    Variable,
    /// Markup tag names and brackets.
    Tag,
    /// Diff `+` lines.
    Inserted,
    /// Diff `-` lines.
    Deleted,
    /// Diff file headers and `@@` hunk lines.
    Meta,
}

/// One colored span: a byte range of the code and what it is.
pub type Token = (Range<usize>, TokenKind);

/// Colored spans for `code` under the fence info string `lang`, in order and
/// non-overlapping, each on UTF-8 boundaries. A missing or unknown language
/// returns none.
#[must_use]
pub fn highlight(code: &str, lang: Option<&str>) -> Vec<Token> {
    match lang.and_then(syntax_for) {
        Some(Syntax::Code(spec)) => Lexer::new(code, spec).run(),
        Some(Syntax::Diff) => diff(code),
        Some(Syntax::Markup) => markup(code),
        None => Vec::new(),
    }
}

/// The language of a fence info string: its first word, lowercased, with a
/// leading `.` or `{` (Pandoc attributes) dropped. `None` for plain fences.
#[must_use]
pub fn fence_language(info: &str) -> Option<String> {
    let word = info
        .trim()
        .trim_start_matches(['{', '.'])
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '{' | '}' | '='))
        .next()
        .unwrap_or("");
    (!word.is_empty()).then(|| word.to_ascii_lowercase())
}

enum Syntax {
    Code(&'static Spec),
    Diff,
    Markup,
}

fn syntax_for(lang: &str) -> Option<Syntax> {
    let lang = fence_language(lang)?;
    let spec = match lang.as_str() {
        "rust" | "rs" => &RUST,
        "python" | "py" | "python3" | "py3" | "pyi" | "ipython" => &PYTHON,
        "javascript" | "js" | "jsx" | "mjs" | "cjs" | "typescript" | "ts" | "tsx" | "mts"
        | "cts" | "node" | "vue" | "svelte" => &JAVASCRIPT,
        "go" | "golang" => &GO,
        "c" | "h" | "cpp" | "c++" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "objc" | "cuda" => &C,
        "java" | "kotlin" | "kt" | "kts" | "scala" | "groovy" | "gradle" | "dart" => &JVM,
        "csharp" | "cs" | "c#" => &CSHARP,
        "swift" => &SWIFT,
        "php" => &PHP,
        "ruby" | "rb" => &RUBY,
        "lua" => &LUA,
        "sh" | "bash" | "zsh" | "shell" | "shellscript" | "console" | "fish" | "ksh"
        | "dockerfile" | "docker" | "makefile" | "make" => &SHELL,
        "powershell" | "ps1" | "ps" | "pwsh" | "psm1" => &POWERSHELL,
        "sql" | "mysql" | "postgres" | "postgresql" | "psql" | "sqlite" | "plsql" | "tsql" => &SQL,
        "json" | "jsonc" | "json5" | "jsonl" | "geojson" => &JSON,
        "yaml" | "yml" => &YAML,
        "toml" | "ini" | "cfg" | "conf" | "env" | "dotenv" | "properties" | "editorconfig"
        | "gitconfig" => &TOML,
        "css" | "scss" | "sass" | "less" => &CSS,
        "diff" | "patch" | "udiff" => return Some(Syntax::Diff),
        "html" | "htm" | "xhtml" | "xml" | "svg" | "xaml" | "plist" | "csproj" | "vbproj" => {
            return Some(Syntax::Markup);
        }
        _ => return None,
    };
    Some(Syntax::Code(spec))
}

mod specs;

use specs::*;
struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    spec: &'static Spec,
    pos: usize,
    tokens: Vec<Token>,
    /// What the next word is, after `fn`, `class`, and the like.
    pending: Option<TokenKind>,
    /// Only whitespace since the last newline.
    line_start: bool,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, spec: &'static Spec) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            spec,
            pos: 0,
            tokens: Vec::new(),
            pending: None,
            line_start: true,
        }
    }

    fn run(mut self) -> Vec<Token> {
        while self.pos < self.bytes.len() {
            let byte = self.bytes[self.pos];
            match byte {
                b'\n' => {
                    self.line_start = true;
                    self.pos += 1;
                    continue;
                }
                b' ' | b'\t' | b'\r' => {
                    self.pos += 1;
                    continue;
                }
                _ => {}
            }
            let line_start = std::mem::replace(&mut self.line_start, false);
            if !byte.is_ascii() {
                self.pos += char_len(self.src, self.pos);
                continue;
            }
            if self.comment()
                || self.string(self.pos)
                || (line_start && self.line_key())
                || self.special(byte, line_start)
            {
                self.pending = None;
                continue;
            }
            if byte.is_ascii_digit() {
                self.number();
                self.pending = None;
            } else if is_ident_start(byte) {
                self.word();
            } else {
                self.pos += 1;
                self.pending = None;
            }
        }
        self.tokens
    }

    fn push(&mut self, range: Range<usize>, kind: TokenKind) {
        if range.start < range.end {
            self.tokens.push((range, kind));
        }
    }

    fn comment(&mut self) -> bool {
        let rest = &self.src[self.pos..];
        if let Some((open, close)) = self.spec.block_comment
            && rest.starts_with(open)
        {
            let end = rest[open.len()..].find(close).map_or(self.src.len(), |at| {
                self.pos + open.len() + at + close.len()
            });
            self.push(self.pos..end, TokenKind::Comment);
            self.pos = end;
            return true;
        }
        let at_word_start = self.pos == 0 || self.bytes[self.pos - 1].is_ascii_whitespace();
        let opens = self
            .spec
            .line_comments
            .iter()
            .any(|marker| rest.starts_with(marker) && (*marker != "#" || at_word_start));
        if !opens {
            return false;
        }
        let end = line_end(self.src, self.pos);
        self.push(self.pos..end, TokenKind::Comment);
        self.pos = end;
        true
    }

    /// A string whose opening quote is at `self.pos`; the span starts at
    /// `start`, which is earlier when a prefix (`f`, `b`) leads it.
    fn string(&mut self, start: usize) -> bool {
        let rest = &self.src[self.pos..];
        if self.spec.triple_quotes
            && let Some(delim) = ["\"\"\"", "'''"]
                .into_iter()
                .find(|delim| rest.starts_with(delim))
        {
            let end = rest[3..]
                .find(delim)
                .map_or(self.src.len(), |at| self.pos + 3 + at + 3);
            self.push(start..end, TokenKind::String);
            self.pos = end;
            return true;
        }
        let quote = self.bytes[self.pos];
        if !self.spec.quotes.contains(&quote) {
            return false;
        }
        if quote == b'\'' && self.spec.rust {
            return self.char_or_lifetime();
        }
        let multiline = self.spec.multiline_quotes.contains(&quote);
        let end = scan_quoted(self.bytes, self.pos + 1, quote, multiline, self.spec.escape);
        let kind = if self.spec.json_keys && next_byte(self.bytes, end) == Some(b':') {
            TokenKind::Property
        } else {
            TokenKind::String
        };
        self.push(start..end, kind);
        self.pos = end;
        true
    }

    /// Rust `'x'`, `'\n'`, or a lifetime `'a`.
    fn char_or_lifetime(&mut self) -> bool {
        let start = self.pos;
        let rest = &self.src[start + 1..];
        match rest.chars().next() {
            Some('\\') => {
                let end = scan_quoted(self.bytes, start + 1, b'\'', false, Some(b'\\'));
                self.push(start..end, TokenKind::String);
                self.pos = end;
            }
            Some(c) if rest[c.len_utf8()..].starts_with('\'') => {
                let end = start + 1 + c.len_utf8() + 1;
                self.push(start..end, TokenKind::String);
                self.pos = end;
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let end = ident_end(self.bytes, start + 1, false);
                self.push(start..end, TokenKind::Variable);
                self.pos = end;
            }
            _ => self.pos += 1,
        }
        true
    }

    /// `key:` or `key =` at the start of a line, or a `[section]` header.
    fn line_key(&mut self) -> bool {
        let Some(separator) = self.spec.line_key else {
            return false;
        };
        let line = &self.src[self.pos..line_end(self.src, self.pos)];
        if separator == b'=' && line.starts_with('[') {
            let end = line.rfind(']').map_or(line.len(), |at| at + 1);
            self.push(self.pos..self.pos + end, TokenKind::Type);
            self.pos += end;
            return true;
        }
        let offset = if separator == b':' && line.starts_with("- ") {
            2
        } else {
            0
        };
        let key = line[offset..]
            .bytes()
            .take_while(|byte| {
                !byte.is_ascii_whitespace() && *byte != separator && !b"\"'#".contains(byte)
            })
            .count();
        let after = line[offset + key..].trim_start_matches([' ', '\t']);
        let separated = after.as_bytes().first() == Some(&separator)
            && (separator != b':' || after.as_bytes().get(1).is_none_or(u8::is_ascii_whitespace));
        if key == 0 || !separated {
            return false;
        }
        let start = self.pos + offset;
        self.push(start..start + key, TokenKind::Property);
        self.pos = start + key;
        true
    }

    /// Attributes, directives, decorators, sigils, and flags.
    fn special(&mut self, byte: u8, line_start: bool) -> bool {
        let start = self.pos;
        let next = self.bytes.get(start + 1).copied();
        match byte {
            b'#' if self.spec.rust && matches!(next, Some(b'[' | b'!')) => {
                let end = attribute_end(self.bytes, start);
                self.push(start..end, TokenKind::Macro);
                self.pos = end;
                true
            }
            b'#' if self.spec.preprocessor && line_start => {
                let name = start
                    + 1
                    + self.bytes[start + 1..]
                        .iter()
                        .take_while(|byte| matches!(byte, b' ' | b'\t'))
                        .count();
                let end = ident_end(self.bytes, name, false);
                self.push(start..end, TokenKind::Macro);
                self.pos = end;
                true
            }
            b'@' if self.spec.decorators && next.is_some_and(is_ident_start) => {
                let mut end = ident_end(self.bytes, start + 1, self.spec.dash_idents);
                while self.bytes.get(end) == Some(&b'.')
                    && self.bytes.get(end + 1).copied().is_some_and(is_ident_start)
                {
                    end = ident_end(self.bytes, end + 1, false);
                }
                self.push(start..end, TokenKind::Macro);
                self.pos = end;
                true
            }
            b'$' if self.spec.sigils => {
                let end = match next {
                    Some(b'{') => self.src[start..]
                        .find('}')
                        .filter(|at| !self.src[start..start + at].contains('\n'))
                        .map_or(start + 1, |at| start + at + 1),
                    Some(byte) if is_ident_start(byte) => {
                        let mut end = ident_end(self.bytes, start + 1, false);
                        // PowerShell scopes: `$env:PATH`, `$script:count`.
                        if self.bytes.get(end) == Some(&b':')
                            && self.bytes.get(end + 1).copied().is_some_and(is_ident_start)
                        {
                            end = ident_end(self.bytes, end + 1, false);
                        }
                        end
                    }
                    Some(byte) if byte.is_ascii_digit() || b"#?@!$*-_".contains(&byte) => start + 2,
                    _ => start + 1,
                };
                self.push(start..end, TokenKind::Variable);
                self.pos = end;
                true
            }
            b'-' if self.spec.flags
                && (start == 0 || self.bytes[start - 1].is_ascii_whitespace()) =>
            {
                let name = if next == Some(b'-') {
                    start + 2
                } else {
                    start + 1
                };
                if !self.bytes.get(name).copied().is_some_and(is_ident_start) {
                    return false;
                }
                let end = ident_end(self.bytes, name, true);
                self.push(start..end, TokenKind::Property);
                self.pos = end;
                true
            }
            _ => false,
        }
    }

    fn number(&mut self) {
        let start = self.pos;
        let mut end = start + 1;
        while let Some(&byte) = self.bytes.get(end) {
            let fraction = byte == b'.'
                && self.bytes[end - 1].is_ascii_digit()
                && self.bytes.get(end + 1).is_some_and(u8::is_ascii_digit);
            if byte.is_ascii_alphanumeric() || byte == b'_' || fraction {
                end += 1;
            } else {
                break;
            }
        }
        self.push(start..end, TokenKind::Number);
        self.pos = end;
    }

    fn word(&mut self) {
        let start = self.pos;
        let end = ident_end(self.bytes, start, self.spec.dash_idents);
        let word = &self.src[start..end];
        self.pos = end;
        if self.string_prefix(start, end, word) {
            self.pending = None;
            return;
        }
        let pending = self.pending.take();
        let spec = self.spec;
        let listed = |list: &[&str]| {
            list.iter().any(|item| {
                if spec.ignore_case {
                    item.eq_ignore_ascii_case(word)
                } else {
                    *item == word
                }
            })
        };
        if listed(spec.keywords) {
            let lower = word.to_ascii_lowercase();
            if FUNCTION_DEFINERS.contains(&lower.as_str()) {
                self.pending = Some(TokenKind::Function);
            } else if TYPE_DEFINERS.contains(&lower.as_str()) {
                self.pending = Some(TokenKind::Type);
            }
            self.push(start..end, TokenKind::Keyword);
            return;
        }
        if listed(spec.literals) {
            self.push(start..end, TokenKind::Number);
            return;
        }
        if listed(spec.types) {
            self.push(start..end, TokenKind::Type);
            return;
        }
        if let Some(kind) = pending {
            self.push(start..end, kind);
            return;
        }
        if spec.rust
            && self.bytes.get(end) == Some(&b'!')
            && matches!(self.bytes.get(end + 1), Some(b'(' | b'[' | b'{'))
        {
            self.push(start..end + 1, TokenKind::Macro);
            self.pos = end + 1;
            return;
        }
        if next_byte(self.bytes, end) == Some(b'(') {
            self.push(start..end, TokenKind::Function);
            return;
        }
        if spec.capital_types && word.as_bytes()[0].is_ascii_uppercase() {
            let constant = word.len() > 1
                && word
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
            let kind = if constant {
                TokenKind::Number
            } else {
                TokenKind::Type
            };
            self.push(start..end, kind);
        }
    }

    /// `f"…"`, `b'…'`, and Rust raw strings `r#"…"#`.
    fn string_prefix(&mut self, start: usize, end: usize, word: &str) -> bool {
        let lower = word.to_ascii_lowercase();
        if !self.spec.string_prefixes.contains(&lower.as_str()) {
            return false;
        }
        if self.spec.rust && lower.ends_with('r') {
            let hashes = self.bytes[end..]
                .iter()
                .take_while(|byte| **byte == b'#')
                .count();
            if self.bytes.get(end + hashes) != Some(&b'"') {
                return false;
            }
            let body = end + hashes + 1;
            let close = format!("\"{}", "#".repeat(hashes));
            let stop = self.src[body..]
                .find(&close)
                .map_or(self.src.len(), |at| body + at + close.len());
            self.push(start..stop, TokenKind::String);
            self.pos = stop;
            return true;
        }
        if !self
            .bytes
            .get(end)
            .is_some_and(|byte| self.spec.quotes.contains(byte))
        {
            return false;
        }
        self.pos = end;
        // A Rust `b'x'` is a byte literal, not a lifetime.
        if self.spec.rust && self.bytes[end] == b'\'' {
            let close = scan_quoted(self.bytes, end + 1, b'\'', false, Some(b'\\'));
            self.push(start..close, TokenKind::String);
            self.pos = close;
            return true;
        }
        self.string(start)
    }
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn ident_end(bytes: &[u8], start: usize, dashes: bool) -> usize {
    let mut end = start;
    while let Some(&byte) = bytes.get(end) {
        let dash = dashes
            && byte == b'-'
            && end > start
            && bytes.get(end + 1).is_some_and(u8::is_ascii_alphanumeric);
        if byte.is_ascii_alphanumeric() || byte == b'_' || dash {
            end += 1;
        } else {
            break;
        }
    }
    end
}

/// The first byte at or after `from` that is not a space or tab.
fn next_byte(bytes: &[u8], from: usize) -> Option<u8> {
    bytes[from.min(bytes.len())..]
        .iter()
        .copied()
        .find(|byte| !matches!(byte, b' ' | b'\t'))
}

fn char_len(src: &str, at: usize) -> usize {
    src[at..].chars().next().map_or(1, char::len_utf8)
}

/// The newline ending the line that holds `at`, or the end of `src`.
fn line_end(src: &str, at: usize) -> usize {
    src[at..].find('\n').map_or(src.len(), |offset| at + offset)
}

/// The end of a string whose body starts at `from`: just past the closing
/// `quote`, at the newline when the string cannot span lines, or the end.
fn scan_quoted(
    bytes: &[u8],
    mut from: usize,
    quote: u8,
    multiline: bool,
    escape: Option<u8>,
) -> usize {
    while let Some(&byte) = bytes.get(from) {
        if Some(byte) == escape {
            from += 2;
        } else if byte == quote {
            return from + 1;
        } else if byte == b'\n' && !multiline {
            return from;
        } else {
            from += 1;
        }
    }
    bytes.len()
}

/// The end of a Rust `#[…]` or `#![…]` attribute, nested brackets included.
fn attribute_end(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0usize;
    for (offset, byte) in bytes[start..].iter().enumerate() {
        match byte {
            b'[' => depth += 1,
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return start + offset + 1;
                }
            }
            b'\n' if depth == 0 => return start + offset,
            _ => {}
        }
    }
    bytes.len()
}

/// Whole-line colors for a unified diff.
fn diff(code: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut start = 0;
    for line in code.split('\n') {
        let body = line.trim_end_matches('\r');
        let kind = if ["+++", "---", "@@", "diff ", "index "]
            .iter()
            .any(|prefix| body.starts_with(prefix))
        {
            Some(TokenKind::Meta)
        } else if body.starts_with('+') {
            Some(TokenKind::Inserted)
        } else if body.starts_with('-') {
            Some(TokenKind::Deleted)
        } else {
            None
        };
        if let Some(kind) = kind
            && !body.is_empty()
        {
            tokens.push((start..start + body.len(), kind));
        }
        start += line.len() + 1;
    }
    tokens
}

/// Tags, attributes, attribute values, and comments in HTML and XML.
fn markup(code: &str) -> Vec<Token> {
    let bytes = code.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if code[at..].starts_with("<!--") {
            let end = code[at + 4..]
                .find("-->")
                .map_or(code.len(), |offset| at + 4 + offset + 3);
            tokens.push((at..end, TokenKind::Comment));
            at = end;
            continue;
        }
        let opens_tag = bytes[at] == b'<'
            && bytes
                .get(at + 1)
                .is_some_and(|byte| byte.is_ascii_alphabetic() || b"/!?".contains(byte));
        if !opens_tag {
            at += char_len(code, at);
            continue;
        }
        let name = at + 1 + usize::from(b"/!?".contains(&bytes[at + 1]));
        let end = markup_name_end(bytes, name);
        tokens.push((at..end, TokenKind::Tag));
        at = end;
        while at < bytes.len() && bytes[at] != b'>' {
            let byte = bytes[at];
            if byte == b'"' || byte == b'\'' {
                let end = scan_quoted(bytes, at + 1, byte, true, None);
                tokens.push((at..end, TokenKind::String));
                at = end;
            } else if is_ident_start(byte) {
                let end = markup_name_end(bytes, at);
                tokens.push((at..end, TokenKind::Property));
                at = end;
            } else if byte == b'/' || byte == b'?' {
                tokens.push((at..at + 1, TokenKind::Tag));
                at += 1;
            } else {
                at += char_len(code, at);
            }
        }
        if at < bytes.len() {
            tokens.push((at..at + 1, TokenKind::Tag));
            at += 1;
        }
    }
    tokens
}

fn markup_name_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || b"_:.-@".contains(byte))
    {
        end += 1;
    }
    end
}
