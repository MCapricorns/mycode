//! Search-root resolution and user-typed alias opens.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::tool::ToolError;

use super::*;

/// Resolves `cwd` and `path_arg` to a handle-backed search root.
///
/// Relative `cwd` is made absolute against the process cwd; an already
/// absolute `cwd` does not consult the process cwd. An omitted
/// path or any argument that lexically normalizes to `cwd` denotes the allowed
/// root. Relative arguments are normalized with an anchored component stack,
/// so a leading parent can never leave and later re-enter the root. Unix and
/// Windows target traversal is handle-relative and no-follow. Windows also
/// validates final Unicode paths and stores on-disk component spelling after
/// an alias open so anchored ignore matching sees `Visible`, not `visible`.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArgs`] for lexical escapes, symlink/reparse
/// targets, or handle-proven containment failures. Missing or inaccessible
/// roots, cancelled or overdue ignore reads, and oversized ignore files
/// return [`ToolError::Execution`].
/// Resolves a search root while honouring `cancel` and `limits`.
///
/// Ignore files loaded during resolution use the same cancel token and
/// [`WalkLimiter`] the walker will share, so a timeout cannot keep reading
/// and ignore/handle budgets cannot be spent twice.
///
/// # Errors
///
/// Same as [`resolve_search_root`], plus cancellation and deadline expiry
/// while reading ignore files.
/// Resolves a search root with an explicit content/metadata capability.
pub(crate) fn resolve_search_root_with_access(
    cwd: &Path,
    path_arg: Option<&str>,
    cancel: &CancellationToken,
    limits: &Limits,
    access: SearchAccess,
) -> Result<ResolvedRoot, ToolError> {
    let absolute_cwd = normalize_session_cwd(cwd)?;

    let allowed = open_allowed_root(&absolute_cwd)
        .map_err(|error| ToolError::Execution(format!("session cwd is not accessible: {error}")))?;
    if allowed.kind != FsEntryKind::Directory {
        return Err(ToolError::Execution(
            "session cwd is not a directory".to_owned(),
        ));
    }

    #[cfg(windows)]
    let allowed_path = allowed.final_path.clone();
    #[cfg(not(windows))]
    let allowed_path = absolute_cwd.clone();

    // Windows: the session-given cwd spelling can differ from the
    // handle-proven final path through an 8.3 alias (`RUNNER~1` vs
    // `runneradmin`); both spell the tree the retained handle anchors.
    #[cfg(windows)]
    let session_spelling: Option<&Path> = Some(absolute_cwd.as_path());
    #[cfg(not(windows))]
    let session_spelling: Option<&Path> = None;

    let relative = match path_arg {
        Some(raw) => resolve_relative_argument(&allowed_path, session_spelling, raw)
            .map_err(|()| ToolError::InvalidArgs(format!("path escapes the session cwd: {raw}")))?,
        None => PathBuf::new(),
    };
    let raw = path_arg.unwrap_or("");
    let limiter = Arc::new(WalkLimiter::new(limits));
    if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
        let reason = limiter.stopped_reason().unwrap_or("search stopped");
        return Err(ToolError::Execution(format!(
            "search ignore files cannot be loaded: {reason}"
        )));
    }
    let allowed_lease = limiter
        .lease()
        .map_err(|error| ToolError::Execution(format!("search handle budget: {error}")))?;
    let (target, root_path, ignores, target_relative, hidden) =
        open_target_with_ignores(&allowed, &allowed_path, &relative, &limiter, cancel, access)
            .map_err(|error| {
                map_target_or_ignore_error(raw, relative.as_os_str().is_empty(), error)
            })?;
    let target_lease = limiter
        .lease()
        .map_err(|error| ToolError::Execution(format!("search handle budget: {error}")))?;

    // Keep both identities live for the full operation. `allowed` remains
    // separate even when `target` is a duplicated handle to the same object.
    let final_allowed_identity = identity_and_kind(&allowed.file)
        .map_err(|error| ToolError::Execution(format!("allowed-root validation failed: {error}")))?
        .0;
    if final_allowed_identity != allowed.identity {
        return Err(ToolError::Execution(
            "allowed-root identity changed during resolution".to_owned(),
        ));
    }

    Ok(ResolvedRoot {
        root: root_path,
        cwd: allowed_path,
        target_relative,
        allowed,
        target,
        ignores,
        limiter,
        hidden,
        _allowed_lease: allowed_lease,
        _target_lease: target_lease,
    })
}

fn open_target_with_ignores(
    allowed: &StableHandle,
    allowed_path: &Path,
    relative: &Path,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
    access: SearchAccess,
) -> io::Result<(StableHandle, PathBuf, walk::IgnoreStack, PathBuf, bool)> {
    let components: Vec<_> = relative.components().collect();
    if components.len() > limiter.max_walk_depth() {
        limiter.stop("walk depth limit reached");
        return Err(io::Error::other("walk depth limit reached"));
    }
    let mut ignores = walk::IgnoreStack::default();
    ignores
        .seed_git_boundary(&allowed.file, limiter, cancel)
        .map_err(wrap_ignore_error)?;
    ignores
        .ingest(&allowed.file, Path::new(""), limiter, cancel)
        .map_err(wrap_ignore_error)?;
    if relative.as_os_str().is_empty() {
        return Ok((
            allowed.try_clone()?,
            allowed_path.to_path_buf(),
            ignores,
            PathBuf::new(),
            false,
        ));
    }

    let mut parent = allowed.file.try_clone()?;
    let mut walked = PathBuf::new();
    let mut hidden = false;
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsafe relative component",
            ));
        };
        let is_last = index + 1 == components.len();
        if !limiter.try_reserve_entry() {
            return Err(io::Error::other("walk entry limit reached"));
        }
        if is_last {
            return open_final_target(
                allowed,
                allowed_path,
                relative,
                &parent,
                name,
                &walked,
                ignores,
                limiter,
                cancel,
                hidden,
                access,
            );
        }
        let (child, exact) = open_alias_component(
            &parent,
            name,
            Some(FsEntryKind::Directory),
            SearchAccess::Content,
            limiter,
            cancel,
        )?;
        hidden |= component_is_hidden(&exact, &child)?;
        walked.push(exact);
        ignores
            .ingest(&child, &walked, limiter, cancel)
            .map_err(wrap_ignore_error)?;
        parent = child;
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "empty relative path cannot be opened as a walked entry",
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "final target open needs the retained handles, walked spelling, ignore stop state, and hidden accumulation"
)]
fn open_final_target(
    allowed: &StableHandle,
    allowed_path: &Path,
    relative: &Path,
    parent: &File,
    name: &OsStr,
    walked: &Path,
    mut ignores: walk::IgnoreStack,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
    mut hidden: bool,
    access: SearchAccess,
) -> io::Result<(StableHandle, PathBuf, walk::IgnoreStack, PathBuf, bool)> {
    let (mut target, exact) = open_alias_target(allowed, parent, name, access, limiter, cancel)?;
    if target.kind == FsEntryKind::Directory && access == SearchAccess::Metadata {
        let (content, content_exact) = open_alias_target(
            allowed,
            parent,
            name,
            SearchAccess::Content,
            limiter,
            cancel,
        )?;
        if content.identity != target.identity || content_exact != exact {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory identity changed between metadata and content open",
            ));
        }
        target = content;
    }
    hidden |= handle_is_hidden(&target, &exact)?;
    let mut target_relative = walked.to_path_buf();
    target_relative.push(exact);
    if target.kind == FsEntryKind::Directory {
        if !target.is_content_file() {
            return Err(io::Error::other(
                "directory search root was bound metadata-only",
            ));
        }
        ignores
            .ingest(&target.file, &target_relative, limiter, cancel)
            .map_err(wrap_ignore_error)?;
    }
    #[cfg(unix)]
    let root_path = {
        let _ = relative;
        allowed_path.join(&target_relative)
    };
    #[cfg(windows)]
    let root_path = {
        let _ = (allowed_path, relative);
        target.final_path.clone()
    };
    #[cfg(not(any(unix, windows)))]
    let root_path = allowed_path.join(relative);
    Ok((target, root_path, ignores, target_relative, hidden))
}

fn component_is_hidden(name: &OsStr, file: &File) -> io::Result<bool> {
    if walk::name_is_hidden(name) {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        windows_file_is_hidden(file)
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Ok(false)
    }
}

/// Opens one user-typed child and returns the on-disk component spelling.
fn open_alias_component(
    parent: &File,
    name: &OsStr,
    expected: Option<FsEntryKind>,
    access: SearchAccess,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<(File, OsString)> {
    let (handle, exact) = open_alias_handle(parent, name, expected, access, limiter, cancel)?;
    Ok((handle.file, exact))
}

fn open_alias_target(
    allowed: &StableHandle,
    parent: &File,
    name: &OsStr,
    access: SearchAccess,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<(StableHandle, OsString)> {
    let (handle, exact) = open_alias_handle(parent, name, None, access, limiter, cancel)?;
    #[cfg(windows)]
    if !is_within(&allowed.final_path, &handle.final_path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "opened target is outside the allowed root",
        ));
    }
    #[cfg(not(windows))]
    let _ = allowed;
    Ok((handle, exact))
}

fn open_alias_handle(
    parent: &File,
    name: &OsStr,
    expected: Option<FsEntryKind>,
    access: SearchAccess,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<(StableHandle, OsString)> {
    #[cfg(unix)]
    {
        unix_open_alias(parent, name, expected, access, limiter, cancel)
    }
    #[cfg(windows)]
    {
        let _ = cancel;
        let _ = limiter;
        let handle = open_named_windows(parent, name, expected, NameMatch::Alias, access)?;
        let exact = on_disk_component_name(&handle)?;
        Ok((handle, exact))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name, expected, access, limiter, cancel);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative child open is not implemented on this platform",
        ))
    }
}

/// Opens a user-typed Unix component and returns the unique on-disk spelling.
///
/// The opened object's identity is matched against every directory entry of
/// `parent`. Zero or several matches fail closed so a case-insensitive alias
/// cannot keep the caller's spelling as the path or ignore key.
#[cfg(unix)]
fn unix_open_alias(
    parent: &File,
    name: &OsStr,
    expected: Option<FsEntryKind>,
    access: SearchAccess,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<(StableHandle, OsString)> {
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    if access == SearchAccess::Metadata && expected != Some(FsEntryKind::Directory) {
        confirm_named_unix_metadata(parent, name, expected)?;
        let (identity, kind) = unix_named_identity(parent, name)?;
        if let Some(expected) = expected
            && kind != expected
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walked object type or identity changed before it was opened",
            ));
        }
        let exact = unix_on_disk_component_name(parent, identity, limiter, cancel)?;
        let handle = StableHandle {
            file: parent.try_clone()?,
            identity,
            kind,
            named_child: Some(exact.clone()),
        };
        return Ok((handle, exact));
    }
    let file = open_named_unix(parent, name, expected, access)?;
    let handle = stable_from_file(file)?;
    let exact = unix_on_disk_component_name(parent, handle.identity, limiter, cancel)?;
    Ok((handle, exact))
}
