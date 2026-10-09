//! Tree-sitter capture replace for [`super::EditOp::Ast`].
//!
//! One snapshot parse is shared by all bounded queries in a call. Only named
//! capture ranges are replaced; the file is never pretty-printed. After apply,
//! the result is reparsed and rejected when the edit introduces syntax errors
//! that were not present before.
use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::Path;

use gpui_kit::component::highlighter::{Language, LanguageRegistry};
use tokio_util::sync::CancellationToken;
use tree_sitter::{Query, QueryCursor, QueryCursorOptions, StreamingIterator};

use super::engine::{Planned, PreparedOp, check_cancel, reserve_planned};
use super::{MAX_CAPTURE_BYTES, MAX_MATCHES, UTF8_BOM};
use crate::builtin::fs_search::MAX_PATTERN_BYTES;
use crate::tool::ToolError;

mod syntax;
pub(super) use syntax::{parse_body, reject_new_syntax_errors};

/// Query source cap. 64 KiB is far above real tree-sitter queries;
/// compiling larger S-expressions is not useful and would expand match work.
pub(super) const MAX_AST_QUERY_BYTES: usize = 64 * 1024;
/// Total query matches or captures inspected by one AST operation.
///
/// Unlike tree-sitter's in-progress match limit, this caps completed query
/// work even when no capture is selected for replacement.
pub(super) const MAX_AST_SCANNED: usize = 8_192;
/// Maximum syntax-tree nodes inspected while locating parse errors.
///
/// An erroneous root can force inspection of otherwise valid siblings. This
/// bound prevents malformed maximum-size files from turning validation into an
/// unbounded tree walk.
const MAX_AST_SYNTAX_NODES: usize = 262_144;
/// Maximum syntax errors retained from one parse.
const MAX_AST_SYNTAX_ERRORS: usize = 8_192;
/// Maximum `language` field bytes on an `ast` operation.
///
/// Supported grammar names are short (`javascript`, `typescript`). Applied
/// before parser selection and error formatting so an oversized field cannot
/// inflate diagnostics. Raising it only increases rejected-input memory.
pub(super) const MAX_AST_LANGUAGE_BYTES: usize = 32;
/// Maximum selected `capture` field bytes on an `ast` operation.
///
/// Real tree-sitter capture names are short (`function.method`). The query
/// is capped separately; this bounds the independent field before clone or
/// interpolation. Raising it grows `PreparedOp` clone size.
pub(super) const MAX_AST_CAPTURE_BYTES: usize = 256;
/// Captures or template segments processed between cancellation checks.
const AST_CAPTURE_CANCEL_INTERVAL: usize = 256;
/// Syntax-tree nodes visited between cancellation checks.
const AST_SYNTAX_CANCEL_INTERVAL: usize = 256;

/// Grammar name registered by gpui-kit (`"rust"`, `"tsx"`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AstLanguage(&'static str);

impl AstLanguage {
    fn grammar(self) -> Result<tree_sitter::Language, ToolError> {
        LanguageRegistry::singleton()
            .language(self.0)
            .and_then(|config| config.language)
            .ok_or_else(|| {
                ToolError::Execution(format!(
                    "tree-sitter grammar for {} is not available",
                    self.0
                ))
            })
    }
}

pub(super) fn prepare(
    language: Option<&str>,
    path: &str,
    query: &str,
    replacement: &str,
    capture: Option<&str>,
) -> Result<PreparedOp, ToolError> {
    if let Some(name) = language
        && name.len() > MAX_AST_LANGUAGE_BYTES
    {
        return Err(ToolError::InvalidArgs(format!(
            "ast language exceeds {MAX_AST_LANGUAGE_BYTES} bytes"
        )));
    }
    if let Some(name) = capture
        && name.len() > MAX_AST_CAPTURE_BYTES
    {
        return Err(ToolError::InvalidArgs(format!(
            "ast capture exceeds {MAX_AST_CAPTURE_BYTES} bytes"
        )));
    }
    if query.is_empty() {
        return Err(ToolError::InvalidArgs(
            "ast query must not be empty".to_owned(),
        ));
    }
    if query.len() > MAX_AST_QUERY_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "ast query exceeds {MAX_AST_QUERY_BYTES} bytes"
        )));
    }
    if replacement.len() > MAX_PATTERN_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "ast replacement exceeds {MAX_PATTERN_BYTES} bytes"
        )));
    }
    if let Some(name) = capture {
        validate_portable_capture_name(name)?;
    }
    let language = resolve_language(language, path)?;
    let compiled = compile_query(language, query)?;
    if compiled.capture_names().is_empty() {
        return Err(ToolError::InvalidArgs(
            "ast query must contain at least one named capture".to_owned(),
        ));
    }
    for name in compiled.capture_names() {
        validate_portable_capture_name(name)?;
    }
    if let Some(name) = capture
        && !compiled.capture_names().contains(&name)
    {
        return Err(ToolError::InvalidArgs(
            "ast capture is not present in the query".to_owned(),
        ));
    }
    Ok(PreparedOp::Ast {
        language,
        query: compiled,
        replacement: replacement.to_owned(),
        capture: capture.map(str::to_owned),
    })
}

pub(super) fn reject_mixed_languages(ops: &[PreparedOp]) -> Result<(), ToolError> {
    let mut seen: Option<AstLanguage> = None;
    for op in ops {
        if let PreparedOp::Ast { language, .. } = op {
            match seen {
                None => seen = Some(*language),
                Some(existing) if existing == *language => {}
                Some(_) => {
                    return Err(ToolError::InvalidArgs(
                        "ast operations in one call must use a single language".to_owned(),
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn resolve_language(named: Option<&str>, path: &str) -> Result<AstLanguage, ToolError> {
    if let Some(name) = named {
        if name.len() > MAX_AST_LANGUAGE_BYTES {
            return Err(ToolError::InvalidArgs(format!(
                "ast language exceeds {MAX_AST_LANGUAGE_BYTES} bytes"
            )));
        }
        return parse_language_name(name)
            .ok_or_else(|| ToolError::InvalidArgs("unsupported ast language".to_owned()));
    }
    infer_from_path(path).ok_or_else(|| {
        ToolError::InvalidArgs(format!(
            "unsupported ast file extension for '{path}'; set language explicitly"
        ))
    })
}

/// Maps a caller token onto a gpui-kit grammar name.
///
/// gpui already aliases `rs`/`ts`/`py` and the rest of its registry.
/// Extensions it does not alias (`h`, `jsx`, …) are folded here first.
fn parse_language_name(name: &str) -> Option<AstLanguage> {
    lookup_grammar(match name {
        "c#" => "csharp",
        "cxx" => "cpp",
        other => other,
    })
}

fn infer_from_path(path: &str) -> Option<AstLanguage> {
    let path = Path::new(path);
    if let Some(file_name) = path.file_name().and_then(|name| name.to_str()) {
        if file_name.eq_ignore_ascii_case("makefile") {
            return lookup_grammar("make");
        }
        if file_name.eq_ignore_ascii_case("cmakelists.txt") {
            return lookup_grammar("cmake");
        }
    }
    let ext = path.extension()?.to_str()?;
    lookup_grammar(match ext {
        "h" => "c",
        "hpp" | "hh" | "hxx" | "cc" | "cxx" => "cpp",
        "jsx" | "mjs" | "cjs" => "javascript",
        other => other,
    })
}

fn lookup_grammar(token: &str) -> Option<AstLanguage> {
    let language = Language::from_str(token);
    if matches!(language, Language::Plain) {
        return None;
    }
    let name = language.name();
    LanguageRegistry::singleton()
        .language(name)
        .and_then(|config| config.language)
        .is_some()
        .then_some(AstLanguage(name))
}

fn compile_query(language: AstLanguage, source: &str) -> Result<Query, ToolError> {
    Query::new(&language.grammar()?, source).map_err(|error| {
        ToolError::InvalidArgs(format!(
            "invalid tree-sitter query: {:?} at {}:{}",
            error.kind,
            error.row + 1,
            error.column + 1
        ))
    })
}

/// Parsed AST state shared by all AST operations in one edit call.
pub(super) struct AstSnapshot {
    pub(super) language: AstLanguage,
    pub(super) tree: tree_sitter::Tree,
    errors: Vec<syntax::SyntaxError>,
}

pub(super) fn parse_snapshot(
    ops: &[PreparedOp],
    snapshot: &str,
    cancel: &CancellationToken,
) -> Result<Option<AstSnapshot>, ToolError> {
    let Some(language) = ast_language(ops) else {
        return Ok(None);
    };
    let body = strip_bom(snapshot);
    let tree = parse_body(language, body, cancel)?;
    let mut errors = Vec::new();
    syntax::collect_syntax_errors(
        tree.root_node(),
        &mut errors,
        cancel,
        MAX_AST_SYNTAX_NODES,
        MAX_AST_SYNTAX_ERRORS,
    )?;
    Ok(Some(AstSnapshot {
        language,
        tree,
        errors,
    }))
}

pub(super) struct AstPlan<'a> {
    pub query: &'a Query,
    pub replacement: &'a str,
    pub capture: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct CaptureValue<'a> {
    start: usize,
    end: usize,
    name: &'a str,
    text: &'a str,
}

pub(super) fn plan_ast(
    body: &str,
    tree: &tree_sitter::Tree,
    op: AstPlan<'_>,
    bom_len: usize,
    planned: &mut Vec<Planned>,
    replacement_bytes: &mut usize,
    cancel: &CancellationToken,
) -> Result<(), ToolError> {
    let AstPlan {
        query,
        replacement,
        capture,
    } = op;
    let template = ParsedTemplate::parse(replacement);
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    // This tree-sitter limit bounds simultaneously in-progress matches only.
    cursor.set_match_limit(MAX_MATCHES.min(65_536) as u32);
    let mut progress = |_state: &tree_sitter::QueryCursorState| {
        if cancel.is_cancelled() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = QueryCursorOptions::new().progress_callback(&mut progress);
    let mut selected = 0usize;
    let mut scanned_matches = 0usize;
    let mut scanned_captures = 0usize;
    {
        let mut matches =
            cursor.matches_with_options(query, tree.root_node(), body.as_bytes(), options);
        while let Some(matched) = matches.next() {
            scanned_matches = scanned_matches.saturating_add(1);
            scanned_captures = scanned_captures.saturating_add(matched.captures.len());
            if scanned_matches > MAX_AST_SCANNED || scanned_captures > MAX_AST_SCANNED {
                return Err(ToolError::InvalidArgs(format!(
                    "ast query exceeded {MAX_AST_SCANNED} scanned matches or captures"
                )));
            }
            let mut values = Vec::with_capacity(matched.captures.len());
            for (capture_index, cap) in matched.captures.iter().enumerate() {
                if capture_index.is_multiple_of(AST_CAPTURE_CANCEL_INTERVAL) {
                    check_cancel(cancel)?;
                }
                let index = cap.index as usize;
                let name = names.get(index).copied().unwrap_or("");
                let start = cap.node.start_byte();
                let end = cap.node.end_byte();
                if start == end {
                    return Err(ToolError::InvalidArgs(
                        "ast captures must not be zero-width".to_owned(),
                    ));
                }
                let text = body.get(start..end).ok_or_else(|| {
                    ToolError::Execution(
                        "ast capture is not on a UTF-8 character boundary".to_owned(),
                    )
                })?;
                values.push(CaptureValue {
                    start,
                    end,
                    name,
                    text,
                });
            }
            let captures = index_captures(&values);
            for (target_index, target) in values.iter().enumerate() {
                if target_index.is_multiple_of(AST_CAPTURE_CANCEL_INTERVAL) {
                    check_cancel(cancel)?;
                }
                if capture.is_some_and(|wanted| target.name != wanted) {
                    continue;
                }
                let expanded = template.expand(target, &captures, cancel)?;
                reserve_planned(
                    planned,
                    replacement_bytes,
                    target.start + bom_len,
                    target.end + bom_len,
                    &expanded,
                )?;
                selected += 1;
            }
        }
    }
    check_cancel(cancel)?;
    if cursor.did_exceed_match_limit() {
        return Err(ToolError::InvalidArgs(format!(
            "ast query exceeded the {MAX_MATCHES} in-progress match limit"
        )));
    }
    if selected == 0 {
        return Err(ToolError::Execution(
            "ast query matched no selected captures; re-read the file and adjust the query"
                .to_owned(),
        ));
    }
    Ok(())
}

fn ast_language(ops: &[PreparedOp]) -> Option<AstLanguage> {
    ops.iter().find_map(|op| match op {
        PreparedOp::Ast { language, .. } => Some(*language),
        _ => None,
    })
}

fn strip_bom(text: &str) -> &str {
    text.strip_prefix(UTF8_BOM).unwrap_or(text)
}

#[derive(Clone, Copy)]
enum TemplatePart<'a> {
    Text(&'a str),
    LiteralAt,
    Capture(&'a str),
}

struct ParsedTemplate<'a> {
    parts: Vec<TemplatePart<'a>>,
}

impl<'a> ParsedTemplate<'a> {
    fn parse(template: &'a str) -> Self {
        let bytes = template.as_bytes();
        let mut parts = Vec::new();
        let mut index = 0usize;
        while index < bytes.len() {
            if bytes[index] != b'@' {
                let start = index;
                while index < bytes.len() && bytes[index] != b'@' {
                    index += 1;
                }
                parts.push(TemplatePart::Text(&template[start..index]));
                continue;
            }
            if index + 1 < bytes.len() && bytes[index + 1] == b'@' {
                parts.push(TemplatePart::LiteralAt);
                index += 2;
                continue;
            }
            let name_start = index + 1;
            if name_start >= bytes.len() || !portable_capture_name_start(bytes[name_start]) {
                parts.push(TemplatePart::LiteralAt);
                index += 1;
                continue;
            }
            let mut end = name_start + 1;
            while end < bytes.len() && portable_capture_name_continue(bytes[end]) {
                end += 1;
            }
            parts.push(TemplatePart::Capture(&template[name_start..end]));
            index = end;
        }
        Self { parts }
    }

    fn expand<'text>(
        &self,
        target: &CaptureValue<'text>,
        captures: &BTreeMap<&'text str, IndexedCapture<'text>>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let mut out = String::new();
        for (index, part) in self.parts.iter().enumerate() {
            if index.is_multiple_of(AST_CAPTURE_CANCEL_INTERVAL) {
                check_cancel(cancel)?;
            }
            match part {
                TemplatePart::Text(text) => push_bounded(&mut out, text)?,
                TemplatePart::LiteralAt => push_bounded(&mut out, "@")?,
                TemplatePart::Capture(name) => {
                    let text = resolve_template_capture(name, target, captures)?;
                    push_bounded(&mut out, text)?;
                }
            }
        }
        check_cancel(cancel)?;
        Ok(out)
    }
}

#[derive(Clone, Copy)]
enum IndexedCapture<'a> {
    Unique(&'a str),
    Ambiguous,
}

fn index_captures<'a>(values: &[CaptureValue<'a>]) -> BTreeMap<&'a str, IndexedCapture<'a>> {
    let mut captures = BTreeMap::new();
    for value in values {
        captures
            .entry(value.name)
            .and_modify(|entry| *entry = IndexedCapture::Ambiguous)
            .or_insert(IndexedCapture::Unique(value.text));
    }
    captures
}

fn validate_portable_capture_name(name: &str) -> Result<(), ToolError> {
    if portable_capture_name(name) {
        Ok(())
    } else {
        Err(ToolError::InvalidArgs(
            "ast capture name is not portable; use ASCII alphanumeric, '_', or '-' initially and '.', '?', or '!' only after the first character".to_owned(),
        ))
    }
}

fn portable_capture_name(name: &str) -> bool {
    let Some((first, rest)) = name.as_bytes().split_first() else {
        return false;
    };
    portable_capture_name_start(*first) && rest.iter().copied().all(portable_capture_name_continue)
}

fn portable_capture_name_start(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn portable_capture_name_continue(byte: u8) -> bool {
    portable_capture_name_start(byte) || matches!(byte, b'.' | b'?' | b'!')
}

fn resolve_template_capture<'a>(
    name: &str,
    target: &CaptureValue<'a>,
    captures: &BTreeMap<&'a str, IndexedCapture<'a>>,
) -> Result<&'a str, ToolError> {
    if name == target.name {
        return Ok(target.text);
    }
    match captures.get(name) {
        Some(IndexedCapture::Unique(text)) => Ok(text),
        Some(IndexedCapture::Ambiguous) => Err(ToolError::InvalidArgs(
            "ast replacement capture is ambiguous in this match".to_owned(),
        )),
        None => Err(ToolError::InvalidArgs(
            "ast replacement references an unknown capture in this match".to_owned(),
        )),
    }
}

fn push_bounded(out: &mut String, chunk: &str) -> Result<(), ToolError> {
    if out.len().saturating_add(chunk.len()) > MAX_CAPTURE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "ast replacement exceeds {MAX_CAPTURE_BYTES} bytes"
        )));
    }
    out.push_str(chunk);
    Ok(())
}
