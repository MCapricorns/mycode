//! Host-owned shared file capability and kernel.
//!
//! Paths are anchored at an already-opened session cwd directory handle. User
//! components are opened no-follow, one name at a time. Absolute arguments are
//! accepted only when they are lexically and handle-proven inside that cwd.
//! Hidden names are readable and writable; Search ignore policy is not applied.
//! Prepared handles stay inside this process.
use std::sync::Mutex;

/// Access requested when binding a local file capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileAccess {
    /// Existing regular file opened for content read.
    ExistingContent,
    /// Existing regular file, or a missing leaf under a retained parent.
    ExistingOrMissing,
}

/// Opaque versioned revision token. The encoding is intentionally not
/// documented beyond the `mycode-rev1-` prefix so callers treat it as a cookie.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileRevision(String);

impl FileRevision {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FileRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Outcome of a kernel read.
pub struct FileRead {
    /// Windowed UTF-8 text with BOM stripped. A truncation notice may be
    /// appended, and a `[revision ...]` line is always appended.
    pub displayed: String,
    /// True when the tool-level line or byte cap cut the window.
    pub truncated: bool,
    /// Line count of the full decoded file (BOM stripped).
    pub total_lines: usize,
    /// Lines included in `displayed` before any truncation notice.
    pub returned_lines: usize,
    /// Opaque revision covering identity, size, mtime, and raw-byte hash.
    pub revision: FileRevision,
    /// Cwd-relative on-disk spelling used as the path key.
    pub path_key: String,
}

/// Outcome of a kernel write.
#[derive(Debug)]
pub(crate) struct FileWrite {
    /// Number of UTF-8 bytes written.
    pub bytes_written: usize,
    /// Opaque revision of the published file.
    pub revision: FileRevision,
    /// True when an existing hardlinked directory entry was replaced by a new inode.
    pub detached_hardlink: bool,
    /// Cwd-relative on-disk spelling used as the path key.
    pub path_key: String,
}

impl std::fmt::Debug for FileRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileRead")
            .field("displayed", &"<redacted>")
            .field("truncated", &self.truncated)
            .field("total_lines", &self.total_lines)
            .field("returned_lines", &self.returned_lines)
            .field("revision", &self.revision)
            .field("path_key", &self.path_key)
            .finish()
    }
}

/// Full-file UTF-8 snapshot for atomic edit. The text includes a leading
/// UTF-8 BOM when the on-disk bytes had one.
pub struct FileSnapshot {
    /// Complete UTF-8 file text, including a leading BOM if present.
    pub text: String,
    /// Opaque revision covering identity, size, mtime, and raw-byte hash.
    pub revision: FileRevision,
    /// Cwd-relative on-disk spelling used as the path key.
    pub path_key: String,
}

impl std::fmt::Debug for FileSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileSnapshot")
            .field("text", &"<redacted>")
            .field("revision", &self.revision)
            .field("path_key", &self.path_key)
            .finish()
    }
}

/// Maximum bytes actually read from one file. Larger metadata sizes fail closed.
pub const MAX_READ_SCAN_BYTES: u64 = 32 * 1024 * 1024;
/// Maximum UTF-8 bytes accepted by one write.
pub const MAX_WRITE_BYTES: usize = 8 * 1024 * 1024;
/// Display line cap for one `read` window.
pub const MAX_LINES: usize = 2000;
/// Display byte cap for one `read` window.
pub const MAX_BYTES: usize = 50 * 1024;
/// Write/read chunk size. Cancel is checked between chunks.
pub(super) const WRITE_CHUNK: usize = 64 * 1024;
const MAX_WALK_DEPTH: usize = 256;
const TEMP_ATTEMPTS: usize = 8;
const REVISION_DOMAIN: &str = "mycode-tools file-revision v1";

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
use windows as sys;
#[cfg(not(any(unix, windows)))]
mod sys_stub;
#[cfg(not(any(unix, windows)))]
use sys_stub as sys;

mod prepare;
mod read;
mod types;
mod write;

// Canonical walk-width cap lives in `fs_search`; re-exported for the sys modules.
use crate::builtin::fs_search::MAX_DIR_WIDTH;

// Names the sys modules and sibling kernels import through `super::`.
use prepare::{bind_prepared, check_cancel, map_open_error};
// Used only by the Unix sys module through `super::`.
#[cfg(unix)]
use prepare::map_not_found;
use read::{content_hash, revision_token};

use types::{ChildOpen, FileIdentity, FileKind, FileMeta, OpenedChild, PreparedInner};

// Crate-public surface kept at the historical `fs_io` paths.
pub use prepare::{prepare_file, prepare_file_async};
pub use read::{read_file, read_file_async, read_file_snapshot_async};

pub(crate) use write::write_file_with_lease;

/// Handle-backed file target bound during dispatch preparation.
///
/// A value exists only as a ready retained capability. Dispatch binds
/// [`PreparedFile::key`] and execution takes the inner handles once.
pub struct PreparedFile {
    key: String,
    access: FileAccess,
    inner: Mutex<Option<PreparedInner>>,
}

impl std::fmt::Debug for PreparedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedFile")
            .field("key", &self.key)
            .field("access", &self.access)
            .finish_non_exhaustive()
    }
}

impl PreparedFile {
    /// Returns the cwd-relative on-disk spelling bound to this capability.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    #[must_use]
    pub fn access(&self) -> FileAccess {
        self.access
    }

    fn take_inner(&self) -> Option<PreparedInner> {
        self.inner.lock().ok().and_then(|mut guard| guard.take())
    }
}
