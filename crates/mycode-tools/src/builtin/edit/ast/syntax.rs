//! Syntax-error validation for tree-sitter edit results.
use std::ops::ControlFlow;
use tokio_util::sync::CancellationToken;

use tree_sitter::{ParseOptions, Parser};

use super::{
    AST_SYNTAX_CANCEL_INTERVAL, AstLanguage, AstSnapshot, MAX_AST_SYNTAX_ERRORS,
    MAX_AST_SYNTAX_NODES,
};
use crate::builtin::edit::engine::{Planned, check_cancel};
use crate::tool::ToolError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct SyntaxError {
    start: usize,
    end: usize,
    kind_id: u16,
    missing: bool,
}

#[derive(Clone, Copy, Debug)]
struct CoordinateEdit {
    old_start: usize,
    old_end: usize,
    new_start: usize,
    new_end: usize,
}

/// Reparses `after` and rejects syntax errors absent from the cached snapshot.
pub(in crate::builtin::edit) fn reject_new_syntax_errors(
    parsed: Option<&AstSnapshot>,
    before: &str,
    after: &str,
    planned: &[Planned],
    cancel: &CancellationToken,
) -> Result<(), ToolError> {
    let Some(parsed) = parsed else {
        return Ok(());
    };
    let before_body = super::strip_bom(before);
    let after_body = super::strip_bom(after);
    let before_bom = before.len() - before_body.len();
    let after_bom = after.len() - after_body.len();
    let after_tree = parse_body(parsed.language, after_body, cancel)?;
    let mut after_errors = Vec::new();
    collect_syntax_errors(
        after_tree.root_node(),
        &mut after_errors,
        cancel,
        MAX_AST_SYNTAX_NODES,
        MAX_AST_SYNTAX_ERRORS,
    )?;
    if after_errors.is_empty() {
        return Ok(());
    }
    let edits = coordinate_edits(before, planned, before_bom, after_bom)?;
    if syntax_errors_are_preexisting(&parsed.errors, &after_errors, &edits) {
        Ok(())
    } else {
        Err(ToolError::Execution(
            "ast edit introduced new syntax errors; file was not published".to_owned(),
        ))
    }
}

pub(in crate::builtin::edit) fn parse_body(
    language: AstLanguage,
    body: &str,
    cancel: &CancellationToken,
) -> Result<tree_sitter::Tree, ToolError> {
    check_cancel(cancel)?;
    let mut parser = Parser::new();
    parser.set_language(&language.grammar()?).map_err(|error| {
        ToolError::Execution(format!("tree-sitter language failed to load: {error}"))
    })?;
    let bytes = body.as_bytes();
    let mut input =
        |offset: usize, _position: tree_sitter::Point| bytes.get(offset..).unwrap_or_default();
    let mut progress = |_state: &tree_sitter::ParseState| {
        if cancel.is_cancelled() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    let tree = parser.parse_with_options(&mut input, None, Some(options));
    check_cancel(cancel)?;
    tree.ok_or_else(|| ToolError::Execution("tree-sitter parse failed".to_owned()))
}

pub(super) fn collect_syntax_errors(
    root: tree_sitter::Node<'_>,
    out: &mut Vec<SyntaxError>,
    cancel: &CancellationToken,
    node_limit: usize,
    error_limit: usize,
) -> Result<(), ToolError> {
    check_cancel(cancel)?;
    let mut cursor = root.walk();
    let mut visited = 0usize;
    loop {
        visited = visited.saturating_add(1);
        if visited > node_limit {
            return Err(ToolError::InvalidArgs(format!(
                "ast syntax validation exceeded {node_limit} inspected nodes"
            )));
        }
        if visited.is_multiple_of(AST_SYNTAX_CANCEL_INTERVAL) {
            check_cancel(cancel)?;
        }

        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            if out.len() >= error_limit {
                return Err(ToolError::InvalidArgs(format!(
                    "ast syntax validation exceeded {error_limit} errors"
                )));
            }
            out.push(SyntaxError {
                start: node.start_byte(),
                end: node.end_byte(),
                kind_id: node.kind_id(),
                missing: node.is_missing(),
            });
        }
        if node.has_error() && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                check_cancel(cancel)?;
                return Ok(());
            }
        }
    }
}

fn coordinate_edits(
    before: &str,
    planned: &[Planned],
    before_bom: usize,
    after_bom: usize,
) -> Result<Vec<CoordinateEdit>, ToolError> {
    let mut ordered: Vec<&Planned> = planned
        .iter()
        .filter(|item| before.get(item.start..item.end) != Some(item.replacement.as_str()))
        .collect();
    ordered.sort_by_key(|item| (item.start, item.end));
    let mut edits = Vec::with_capacity(ordered.len());
    let mut old_full_cursor = 0usize;
    let mut new_full_cursor = 0usize;
    for item in ordered {
        let old_start = item.start.checked_sub(before_bom).ok_or_else(|| {
            ToolError::Execution("ast edit unexpectedly overlaps the UTF-8 BOM".to_owned())
        })?;
        let old_end = item.end.checked_sub(before_bom).ok_or_else(|| {
            ToolError::Execution("ast edit unexpectedly overlaps the UTF-8 BOM".to_owned())
        })?;
        let unchanged = item.start.checked_sub(old_full_cursor).ok_or_else(|| {
            ToolError::Execution("ast edit coordinates overlap during validation".to_owned())
        })?;
        let new_start_full = new_full_cursor.checked_add(unchanged).ok_or_else(|| {
            ToolError::Execution("ast edit coordinate overflow during validation".to_owned())
        })?;
        let new_end_full = new_start_full
            .checked_add(item.replacement.len())
            .ok_or_else(|| {
                ToolError::Execution("ast edit coordinate overflow during validation".to_owned())
            })?;
        let new_start = new_start_full.saturating_sub(after_bom);
        let new_end = new_end_full.checked_sub(after_bom).ok_or_else(|| {
            ToolError::Execution(
                "ast edit unexpectedly overlaps the resulting UTF-8 BOM".to_owned(),
            )
        })?;
        edits.push(CoordinateEdit {
            old_start,
            old_end,
            new_start,
            new_end,
        });
        old_full_cursor = item.end;
        new_full_cursor = new_end_full;
    }
    Ok(edits)
}

fn syntax_errors_are_preexisting(
    before: &[SyntaxError],
    after: &[SyntaxError],
    edits: &[CoordinateEdit],
) -> bool {
    let mut existing = before.to_vec();
    existing.sort_unstable();
    let mut mapped = Vec::with_capacity(after.len());
    for error in after {
        let Some(error) = map_syntax_error(*error, edits) else {
            return false;
        };
        mapped.push(error);
    }
    mapped.sort_unstable();
    let mut existing_index = 0usize;
    for error in mapped {
        while existing
            .get(existing_index)
            .is_some_and(|item| *item < error)
        {
            existing_index += 1;
        }
        if existing.get(existing_index) != Some(&error) {
            return false;
        }
        existing_index += 1;
    }
    true
}

fn map_syntax_error(error: SyntaxError, edits: &[CoordinateEdit]) -> Option<SyntaxError> {
    if edits
        .iter()
        .any(|edit| error_intersects_replacement(error, *edit))
    {
        return None;
    }
    let start = map_after_position(error.start, edits, Boundary::Start)?;
    let end = map_after_position(error.end, edits, Boundary::End)?;
    (start <= end).then_some(SyntaxError {
        start,
        end,
        ..error
    })
}

fn error_intersects_replacement(error: SyntaxError, edit: CoordinateEdit) -> bool {
    if edit.new_start == edit.new_end {
        return false;
    }
    if error.start == error.end {
        error.start > edit.new_start && error.start < edit.new_end
    } else {
        error.start < edit.new_end && error.end > edit.new_start
    }
}

#[derive(Clone, Copy)]
enum Boundary {
    Start,
    End,
}

fn map_after_position(pos: usize, edits: &[CoordinateEdit], boundary: Boundary) -> Option<usize> {
    let mut old_cursor = 0usize;
    let mut new_cursor = 0usize;
    for edit in edits {
        if pos < edit.new_start {
            return old_cursor.checked_add(pos.checked_sub(new_cursor)?);
        }
        if pos == edit.new_start {
            return Some(if edit.new_start == edit.new_end {
                match boundary {
                    Boundary::Start => edit.old_end,
                    Boundary::End => edit.old_start,
                }
            } else {
                edit.old_start
            });
        }
        if pos < edit.new_end {
            return None;
        }
        if pos == edit.new_end {
            return Some(edit.old_end);
        }
        old_cursor = edit.old_end;
        new_cursor = edit.new_end;
    }
    old_cursor.checked_add(pos.checked_sub(new_cursor)?)
}
