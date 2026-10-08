//! Content reads, revisions, and display windowing.
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use crate::builtin::blocking::run_blocking_until;
use crate::builtin::fs_search::SEARCH_TIME_LIMIT;
use crate::tool::ToolError;

use super::*;

pub(super) fn content_hash(raw: &[u8]) -> [u8; 32] {
    *blake3::hash(raw).as_bytes()
}

pub(super) fn revision_token(meta: &FileMeta, raw_hash: &[u8; 32]) -> FileRevision {
    let mut hasher = blake3::Hasher::new_derive_key(REVISION_DOMAIN);
    hasher.update(b"v1\0");
    #[cfg(unix)]
    {
        hasher.update(&meta.identity.device.to_le_bytes());
        hasher.update(&meta.identity.inode.to_le_bytes());
    }
    #[cfg(windows)]
    {
        hasher.update(&meta.identity.volume.to_le_bytes());
        hasher.update(&meta.identity.file_id);
    }
    hasher.update(&meta.size.to_le_bytes());
    hasher.update(&meta.mtime_secs.to_le_bytes());
    hasher.update(&meta.mtime_nsecs.to_le_bytes());
    hasher.update(raw_hash);
    FileRevision(format!("mycode-rev1-{}", hasher.finalize().to_hex()))
}

fn reject_encoding(raw: &[u8]) -> Result<&str, ToolError> {
    if raw.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) || raw.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        return Err(ToolError::Execution(
            "UTF-32 encoded files are not supported".to_owned(),
        ));
    }
    if raw.starts_with(&[0xFE, 0xFF]) || raw.starts_with(&[0xFF, 0xFE]) {
        return Err(ToolError::Execution(
            "UTF-16 encoded files are not supported".to_owned(),
        ));
    }
    std::str::from_utf8(raw).map_err(|_| ToolError::Execution("file is not valid UTF-8".to_owned()))
}

/// Selects the `[start, end)` line window under the [`MAX_LINES`] cap.
///
/// `offset`/`limit` are user-controlled, so all arithmetic saturates: the
/// returned invariant is `start <= capped_end <= end <= total_lines`, and
/// `selected` never exceeds [`MAX_LINES`] entries.
fn window_text(
    displayed: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> (String, usize, usize, bool) {
    let total_lines = displayed.lines().count();
    let start = offset.unwrap_or(1).saturating_sub(1).min(total_lines);
    let end = match limit {
        Some(limit) => start.saturating_add(limit).min(total_lines),
        None => total_lines,
    };
    let capped_end = end.min(start.saturating_add(MAX_LINES));
    let selected: Vec<&str> = displayed
        .lines()
        .skip(start)
        .take(capped_end - start)
        .collect();
    let text = selected.join("\n");
    let (mut text, byte_truncated) = crate::builtin::truncate_bytes(&text, MAX_BYTES);
    let truncated = byte_truncated || capped_end < end;
    if truncated {
        text.push_str(&format!(
            "\n[output truncated: showing lines {}-{} of {total_lines}; re-invoke with offset/limit to read more]",
            start + 1,
            start + selected.len(),
        ));
    }
    (text, total_lines, selected.len(), truncated)
}

fn append_revision(body: &str, revision: &FileRevision) -> String {
    if body.is_empty() {
        format!("[revision {revision}]")
    } else {
        format!("{body}\n[revision {revision}]")
    }
}

pub(super) struct RawFile {
    pub(super) raw: Vec<u8>,
    pub(super) key: String,
    pub(super) revision: FileRevision,
}

pub(super) fn read_raw_file(
    prepared: Option<&PreparedFile>,
    cwd: &Path,
    path: &str,
    cancel: &CancellationToken,
) -> Result<RawFile, ToolError> {
    check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
    let inner = bind_prepared(prepared, cwd, path, cancel, FileAccess::ExistingContent)?;
    let PreparedInner::Existing {
        parent,
        mut file,
        name,
        meta,
        key,
    } = inner
    else {
        return Err(ToolError::Execution(format!(
            "file does not exist or is inaccessible: {path}"
        )));
    };
    if meta.size > MAX_READ_SCAN_BYTES {
        return Err(ToolError::Execution(
            "file exceeds the read size limit".to_owned(),
        ));
    }
    let listed = sys::open_child(&parent, &name, ChildOpen::ExistingFile)
        .map_err(|error| map_open_error(path, error))?;
    if listed.meta.identity != meta.identity {
        return Err(ToolError::Execution(
            "file identity changed before read".to_owned(),
        ));
    }
    drop(listed);
    let before = sys::current_meta(&file).map_err(|error| map_open_error(path, error))?;
    if before.identity != meta.identity
        || before.size != meta.size
        || before.mtime_secs != meta.mtime_secs
        || before.mtime_nsecs != meta.mtime_nsecs
    {
        return Err(ToolError::Execution(
            "file identity changed before read".to_owned(),
        ));
    }
    let raw = sys::read_exact_capped(&mut file, before.size, MAX_READ_SCAN_BYTES, cancel).map_err(
        |error| {
            if error.kind() == ErrorKind::Interrupted {
                ToolError::Execution(error.to_string())
            } else {
                ToolError::Execution(format!("failed to read {path}: {error}"))
            }
        },
    )?;
    let after = sys::current_meta(&file).map_err(|error| map_open_error(path, error))?;
    if after.identity != before.identity
        || after.size != before.size
        || after.mtime_secs != before.mtime_secs
        || after.mtime_nsecs != before.mtime_nsecs
        || after.size != raw.len() as u64
    {
        return Err(ToolError::Execution(
            "file identity or size changed during read".to_owned(),
        ));
    }
    let revision = revision_token(&after, &content_hash(&raw));
    Ok(RawFile { raw, key, revision })
}

/// Reads a prepared (or internally prepared) UTF-8 file.
///
/// # Errors
///
/// Returns [`ToolError`] when the capability is missing, encoding is rejected,
/// the file exceeds [`MAX_READ_SCAN_BYTES`], identity changes, or the call is
/// cancelled. Cancel and timeout never return a partial window.
pub fn read_file(
    prepared: Option<&PreparedFile>,
    cwd: &Path,
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    cancel: &CancellationToken,
) -> Result<FileRead, ToolError> {
    let raw = read_raw_file(prepared, cwd, path, cancel)?;
    let text = reject_encoding(&raw.raw)?;
    let displayed = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (window, total_lines, returned_lines, truncated) = window_text(displayed, offset, limit);
    Ok(FileRead {
        displayed: append_revision(&window, &raw.revision),
        truncated,
        total_lines,
        returned_lines,
        revision: raw.revision,
        path_key: raw.key,
    })
}

/// Reads a prepared (or internally prepared) UTF-8 file as a full snapshot.
///
/// Unlike [`read_file`], this does not window or strip a UTF-8 BOM. Cancel
/// and timeout never return a partial snapshot.
///
/// # Errors
///
/// Same as [`read_file`].
pub(crate) fn read_file_snapshot(
    prepared: Option<&PreparedFile>,
    cwd: &Path,
    path: &str,
    cancel: &CancellationToken,
) -> Result<FileSnapshot, ToolError> {
    let raw = read_raw_file(prepared, cwd, path, cancel)?;
    let text = reject_encoding(&raw.raw)?.to_owned();
    Ok(FileSnapshot {
        text,
        revision: raw.revision,
        path_key: raw.key,
    })
}

/// Snapshot-reads on the cancellable supervisor.
///
/// # Errors
///
/// Same as [`read_file_snapshot`].
pub async fn read_file_snapshot_async(
    prepared: Option<std::sync::Arc<PreparedFile>>,
    cwd: PathBuf,
    path: String,
    cancel: CancellationToken,
) -> Result<FileSnapshot, ToolError> {
    let deadline = Instant::now() + SEARCH_TIME_LIMIT;
    run_blocking_until("file snapshot", &cancel, deadline, move |worker_cancel| {
        read_file_snapshot(prepared.as_deref(), &cwd, &path, &worker_cancel)
    })
    .await
}

/// Reads on the cancellable supervisor. Cancel/timeout do not return partial data.
///
/// # Errors
///
/// Same as [`read_file`].
pub async fn read_file_async(
    prepared: Option<std::sync::Arc<PreparedFile>>,
    cwd: PathBuf,
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    cancel: CancellationToken,
) -> Result<FileRead, ToolError> {
    let deadline = Instant::now() + SEARCH_TIME_LIMIT;
    run_blocking_until("file read", &cancel, deadline, move |worker_cancel| {
        read_file(
            prepared.as_deref(),
            &cwd,
            &path,
            offset,
            limit,
            &worker_cancel,
        )
    })
    .await
}
