//! Error classification shared by search resolution and reporting.
use std::io;

use crate::tool::ToolError;

/// Distinctive error so callers skip a now-hidden entry without recording I/O.
pub(crate) fn hidden_entry_error() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "hidden entry")
}

/// Returns whether `error` is the silent hidden-skip marker.
pub(crate) fn is_hidden_skip(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound && error.to_string().contains("hidden entry")
}

/// Model-visible notice when per-path I/O made the report a lower bound.
pub(crate) fn io_incomplete_notice(io_count: u64) -> Option<String> {
    if io_count == 0 {
        None
    } else {
        Some(format!(
            "[search incomplete: {io_count} path(s) could not be read; matching results are a lower bound]"
        ))
    }
}

pub(crate) fn wrap_ignore_error(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::Interrupted || is_ignore_load_error(&error) {
        error
    } else {
        io::Error::other(format!("search ignore files cannot be loaded: {error}"))
    }
}

pub(crate) fn map_target_or_ignore_error(
    raw: &str,
    cwd_target: bool,
    error: io::Error,
) -> ToolError {
    if error.kind() == io::ErrorKind::Interrupted || is_ignore_load_error(&error) {
        let message = error.to_string();
        if message.contains("search ignore files cannot be loaded") {
            return ToolError::Execution(message);
        }
        return ToolError::Execution(format!("search ignore files cannot be loaded: {error}"));
    }
    if cwd_target {
        ToolError::Execution(format!("session cwd handle cannot be retained: {error}"))
    } else {
        map_target_open_error(raw, error)
    }
}

pub(crate) fn is_ignore_load_error(error: &io::Error) -> bool {
    let message = error.to_string();
    message.contains("ignore file exceeds size limit")
        || message.contains("ignore file is not valid UTF-8")
        || message.contains("search ignore files cannot be loaded")
}

/// Shared target-open classification for the search and file kernels.
///
/// `type_word` completes the "not a regular …" argument message;
/// `missing_msg` / `other_msg` prefix the execution messages for
/// not-found versus other failures.
pub(crate) fn map_target_kind_error(
    raw: &str,
    error: io::Error,
    type_word: &str,
    missing_msg: &str,
    other_msg: &str,
) -> ToolError {
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData
    ) {
        ToolError::InvalidArgs(format!(
            "path escapes the session cwd, crosses a link, or is not a {type_word}: {raw}"
        ))
    } else if error.kind() == io::ErrorKind::NotFound {
        ToolError::Execution(format!("{missing_msg}: {raw}: {error}"))
    } else {
        ToolError::Execution(format!("{other_msg}: {raw}: {error}"))
    }
}

pub(crate) fn map_target_open_error(raw: &str, error: io::Error) -> ToolError {
    map_target_kind_error(
        raw,
        error,
        "regular file/directory",
        "search path does not exist or is inaccessible",
        "search path does not exist or is inaccessible",
    )
}
