//! Capability preparation: resolve, walk, and bind retained file handles.
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, ErrorKind};
use std::path::{Component, Path, PathBuf};

use tokio_util::sync::CancellationToken;

use crate::builtin::blocking::run_blocking;
use crate::builtin::fs_search::{
    SEARCH_TIME_LIMIT, map_target_kind_error, normalize_session_cwd, posix_relative_key,
    resolve_relative_argument, validate_component_name,
};
use crate::tool::ToolError;

use super::*;

pub(super) fn check_cancel(cancel: &CancellationToken) -> io::Result<()> {
    if cancel.is_cancelled() {
        Err(io::Error::new(
            ErrorKind::Interrupted,
            "file operation cancelled before completion",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn map_not_found(error: io::Error) -> io::Error {
    if error.kind() == ErrorKind::NotFound {
        error
    } else if error.raw_os_error() == Some(2) {
        io::Error::new(ErrorKind::NotFound, error)
    } else {
        error
    }
}

pub(super) fn map_open_error(raw: &str, error: io::Error) -> ToolError {
    if error.kind() == ErrorKind::Interrupted {
        return ToolError::Execution(error.to_string());
    }
    map_target_kind_error(
        raw,
        error,
        "regular file",
        "file does not exist or is inaccessible",
        "file path is inaccessible",
    )
}

/// Resolves `cwd` and `path` once for retained-capability execution.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] or [`ToolError::InvalidArgs`] when the
/// target cannot be bound, including missing read targets, cancellation, and
/// symlink/reparse/device/ADS/escape failures.
pub fn prepare_file(
    cwd: &Path,
    path: &str,
    cancel: &CancellationToken,
    access: FileAccess,
) -> Result<PreparedFile, ToolError> {
    check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
    let absolute = normalize_session_cwd(cwd)?;
    let allowed = sys::open_allowed_root(&absolute)
        .map_err(|error| ToolError::Execution(format!("session cwd is not accessible: {error}")))?;
    let relative = resolve_relative_argument(&absolute, None, path)
        .map_err(|()| ToolError::InvalidArgs(format!("path escapes the session cwd: {path}")))?;
    let components: Vec<OsString> = relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name.to_os_string()),
            _ => Err(ToolError::InvalidArgs(format!(
                "path escapes the session cwd: {path}"
            ))),
        })
        .collect::<Result<_, _>>()?;
    if components.len() > MAX_WALK_DEPTH {
        return Err(ToolError::InvalidArgs(
            "path exceeds the maximum component depth".to_owned(),
        ));
    }
    if components.is_empty() {
        return Err(ToolError::InvalidArgs(
            "path must name a file inside the session cwd".to_owned(),
        ));
    }
    walk_prepare(allowed, components, path, cancel, access)
}

pub(super) fn walk_prepare(
    allowed: File,
    components: Vec<OsString>,
    raw: &str,
    cancel: &CancellationToken,
    access: FileAccess,
) -> Result<PreparedFile, ToolError> {
    let mut parent = allowed;
    let mut proven = PathBuf::new();
    let last = components.len() - 1;
    for (index, name) in components.iter().enumerate() {
        check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
        validate_component_name(name).map_err(|error| map_open_error(raw, error))?;
        let is_last = index == last;
        let how = if is_last {
            ChildOpen::Probe
        } else {
            ChildOpen::Directory
        };
        match sys::open_child(&parent, name, how) {
            Ok(opened) => {
                if !is_last {
                    if opened.meta.kind != FileKind::Directory {
                        return Err(ToolError::InvalidArgs(format!(
                            "path component is not a directory: {raw}"
                        )));
                    }
                    let exact = sys::unique_component_name(&parent, opened.meta.identity, cancel)
                        .map_err(|error| map_open_error(raw, error))?;
                    proven.push(exact);
                    parent = opened.file;
                    continue;
                }
                if opened.meta.kind != FileKind::File {
                    return Err(ToolError::InvalidArgs(format!(
                        "path is not a regular file: {raw}"
                    )));
                }
                let exact = sys::unique_component_name(&parent, opened.meta.identity, cancel)
                    .map_err(|error| map_open_error(raw, error))?;
                proven.push(&exact);
                let key = posix_relative_key(&proven);
                return Ok(PreparedFile {
                    key: key.clone(),
                    access,
                    inner: Mutex::new(Some(PreparedInner::Existing {
                        parent,
                        file: opened.file,
                        name: exact,
                        meta: opened.meta,
                        key,
                    })),
                });
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                if access != FileAccess::ExistingOrMissing {
                    return Err(map_open_error(raw, error));
                }
                let remaining = components[index..].to_vec();
                let mut key_path = proven;
                for part in &remaining {
                    key_path.push(part);
                }
                let parent_meta =
                    sys::current_meta(&parent).map_err(|error| map_open_error(raw, error))?;
                let key = posix_relative_key(&key_path);
                return Ok(PreparedFile {
                    key: key.clone(),
                    access,
                    inner: Mutex::new(Some(PreparedInner::Missing {
                        parent,
                        remaining,
                        key,
                        parent_identity: parent_meta.identity,
                    })),
                });
            }
            Err(error) => return Err(map_open_error(raw, error)),
        }
    }
    Err(ToolError::InvalidArgs(format!(
        "path must name a file inside the session cwd: {raw}"
    )))
}

/// [`prepare_file`] on the cancellable supervisor thread.
///
/// # Errors
///
/// Same as [`prepare_file`], plus cancellation and deadline expiry.
pub async fn prepare_file_async(
    cwd: PathBuf,
    path: String,
    cancel: CancellationToken,
    access: FileAccess,
) -> Result<PreparedFile, ToolError> {
    run_blocking(
        "file dispatch preflight",
        &cancel,
        SEARCH_TIME_LIMIT,
        move |worker_cancel| prepare_file(&cwd, &path, &worker_cancel, access),
    )
    .await
}

pub(super) fn bind_prepared(
    prepared: Option<&PreparedFile>,
    cwd: &Path,
    path: &str,
    cancel: &CancellationToken,
    access: FileAccess,
) -> Result<PreparedInner, ToolError> {
    if let Some(prepared) = prepared {
        if prepared.access() != access {
            return Err(ToolError::Execution(format!(
                "prepared file access mismatch: prepared {:?}, requested {:?}",
                prepared.access(),
                access
            )));
        }
        let Some(inner) = prepared.take_inner() else {
            return Err(ToolError::Execution(
                "prepared file capability is missing or was already consumed".to_owned(),
            ));
        };
        let key = match &inner {
            PreparedInner::Existing { key, .. } | PreparedInner::Missing { key, .. } => {
                key.as_str()
            }
        };
        if key != prepared.key() {
            return Err(ToolError::Execution(
                "prepared file capability does not match its path key".to_owned(),
            ));
        }
        return Ok(inner);
    }
    let prepared = prepare_file(cwd, path, cancel, access)?;
    prepared.take_inner().ok_or_else(|| {
        ToolError::Execution(
            "prepared file capability is missing or was already consumed".to_owned(),
        )
    })
}
