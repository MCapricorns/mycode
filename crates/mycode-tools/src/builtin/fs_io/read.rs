//! Content reads, revisions, and display windowing.
use std::fmt::Write as _;
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
    let mut shown = selected.len();
    let mut cut_line = false;
    if byte_truncated {
        // Keep whole lines only, so the notice never claims a line the byte
        // cap cut and the suggested offset resumes exactly where this stops.
        match text.rfind('\n') {
            Some(last_break) => {
                text.truncate(last_break);
                shown = text.matches('\n').count() + 1;
            }
            None => {
                shown = 1;
                cut_line = true;
            }
        }
    }
    let truncated = byte_truncated || capped_end < end;
    if truncated {
        let last = start + shown;
        if cut_line {
            let _ = write!(
                text,
                "\n[output truncated: line {last} alone exceeds {MAX_BYTES} bytes; only its start is shown]"
            );
        } else {
            let _ = write!(
                text,
                "\n[output truncated: showing lines {}-{last} of {total_lines}; continue with offset={}]",
                start + 1,
                last + 1,
            );
        }
    }
    (text, total_lines, shown, truncated)
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

/// Maximum bytes of one media file that may be ingested whole.
///
/// The model cap is [`mycode_core::MAX_IMAGE_BASE64_BYTES`] on the encoded
/// payload, and base64 makes 3 bytes 4. No resizer runs in this path, so a
/// bigger picture is refused with an instruction to crop or resize instead
/// of being read and then rejected after encoding.
pub const MAX_MEDIA_INGEST_BYTES: usize = mycode_core::MAX_IMAGE_BASE64_BYTES / 4 * 3;

/// One `read` answer: an image the model can see, or a text window.
pub enum ReadOutcome {
    /// A supported picture, encoded later by the caller.
    Image {
        /// Raw file bytes.
        bytes: Vec<u8>,
        /// Media type from the magic bytes.
        mime: &'static str,
        /// Capability key the path resolved to.
        key: String,
    },
    /// The text window [`read_file`] returns.
    Text(FileRead),
}

/// Image media type from the file's leading bytes. Providers accept these
/// four; PDFs and other documents stay text errors in this tool.
fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Reads one path once: image bytes when the content is a supported picture,
/// otherwise the text window [`read_file`] returns.
///
/// # Errors
///
/// Returns [`ToolError`] when the capability is missing, the picture exceeds
/// [`MAX_MEDIA_INGEST_BYTES`], encoding is rejected, or the call is cancelled.
pub fn read_any(
    prepared: Option<&PreparedFile>,
    cwd: &Path,
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    cancel: &CancellationToken,
) -> Result<ReadOutcome, ToolError> {
    if offset.is_some_and(|start| start < 1) {
        return Err(ToolError::InvalidArgs(
            "offset must be a 1-based line number".to_owned(),
        ));
    }
    let raw = read_raw_file(prepared, cwd, path, cancel)?;
    if let Some(mime) = image_mime(&raw.raw) {
        if offset.is_some() || limit.is_some() {
            return Err(ToolError::InvalidArgs(
                "offset and limit apply to text, not to an image".to_owned(),
            ));
        }
        if raw.raw.len() > MAX_MEDIA_INGEST_BYTES {
            return Err(ToolError::Execution(format!(
                "{path}: image is larger than {} bytes; crop or resize it first",
                MAX_MEDIA_INGEST_BYTES
            )));
        }
        return Ok(ReadOutcome::Image {
            bytes: raw.raw,
            mime,
            key: raw.key,
        });
    }
    let text = reject_encoding(&raw.raw)?;
    let displayed = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (window, total_lines, returned_lines, truncated) = window_text(displayed, offset, limit);
    Ok(ReadOutcome::Text(FileRead {
        displayed: append_revision(&window, &raw.revision),
        truncated,
        total_lines,
        returned_lines,
        revision: raw.revision,
        path_key: raw.key,
    }))
}

/// Reads a prepared (or internally prepared) UTF-8 file. Test-only: the
/// `read` tool goes through [`read_any`], which also accepts pictures.
///
/// # Errors
///
/// Returns [`ToolError`] when the capability is missing, encoding is rejected,
/// the file exceeds [`MAX_READ_SCAN_BYTES`], identity changes, or the call is
/// cancelled. A picture is not text and is reported as such.
#[cfg(test)]
pub fn read_file(
    prepared: Option<&PreparedFile>,
    cwd: &Path,
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    cancel: &CancellationToken,
) -> Result<FileRead, ToolError> {
    match read_any(prepared, cwd, path, offset, limit, cancel)? {
        ReadOutcome::Text(read) => Ok(read),
        ReadOutcome::Image { .. } => Err(ToolError::Execution(format!(
            "{path}: the file is an image, not text"
        ))),
    }
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
/// Same as [`read_any`].
pub async fn read_any_async(
    prepared: Option<std::sync::Arc<PreparedFile>>,
    cwd: PathBuf,
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    cancel: CancellationToken,
) -> Result<ReadOutcome, ToolError> {
    let deadline = Instant::now() + SEARCH_TIME_LIMIT;
    run_blocking_until("file read", &cancel, deadline, move |worker_cancel| {
        read_any(
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tokio_util::sync::CancellationToken;

    use super::{MAX_BYTES, MAX_MEDIA_INGEST_BYTES, read_file, window_text};
    use crate::tool::ToolError;

    #[test]
    fn the_ingest_cap_encodes_to_the_provider_ceiling() {
        assert_eq!(
            MAX_MEDIA_INGEST_BYTES / 3 * 4,
            mycode_core::MAX_IMAGE_BASE64_BYTES
        );
    }

    #[test]
    fn a_byte_cut_window_reports_only_whole_lines() {
        let line = "x".repeat(1000);
        let file = vec![line.as_str(); 120].join("\n");
        let (text, total, shown, truncated) = window_text(&file, None, None);
        assert!(truncated);
        assert_eq!(total, 120);
        assert_eq!(shown, MAX_BYTES / 1001);
        let (body, notice) = text.rsplit_once('\n').expect("notice line");
        assert!(
            body.lines().all(|row| row == line),
            "every shown line is whole"
        );
        assert_eq!(body.lines().count(), shown);
        assert!(
            notice.ends_with(&format!("continue with offset={}]", shown + 1)),
            "{notice}"
        );
    }

    #[test]
    fn a_single_oversized_line_says_it_was_cut() {
        let file = format!("short\n{}\nafter", "y".repeat(MAX_BYTES + 10));
        let (text, _, shown, truncated) = window_text(&file, Some(2), Some(1));
        assert!(truncated);
        assert_eq!(shown, 1);
        assert!(text.ends_with("line 2 alone exceeds 51200 bytes; only its start is shown]"));
    }

    #[test]
    fn offset_zero_is_rejected_before_any_io() {
        let error = read_file(
            None,
            Path::new("/no/such"),
            "missing.txt",
            Some(0),
            None,
            &CancellationToken::new(),
        )
        .expect_err("offset 0");
        assert!(matches!(error, ToolError::InvalidArgs(message) if message.contains("1-based")));
    }
}
