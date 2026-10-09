//! Cross-module value types shared by the fs_io kernel.
use std::ffi::OsString;
use std::fs::File;

/// How a child is opened from a retained parent handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChildOpen {
    Directory,
    ExistingFile,
    Probe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FileKind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(super) struct FileIdentity {
    #[cfg(unix)]
    pub(super) device: u64,
    #[cfg(unix)]
    pub(super) inode: u64,
    #[cfg(windows)]
    pub(super) volume: u64,
    #[cfg(windows)]
    pub(super) file_id: [u8; 16],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FileMeta {
    pub(super) identity: FileIdentity,
    pub(super) kind: FileKind,
    pub(super) size: u64,
    pub(super) mtime_secs: i64,
    pub(super) mtime_nsecs: u32,
    pub(super) nlink: u64,
    pub(super) unix_mode: u32,
    pub(super) unix_uid: u32,
    pub(super) unix_gid: u32,
    #[cfg(windows)]
    pub(super) windows_attributes: u32,
}

pub(super) struct OpenedChild {
    pub(super) file: File,
    pub(super) meta: FileMeta,
    /// Windows temp creation only: duplicate handle that already holds
    /// `DELETE`, moved into the [`TempName`] guard so fail-safe cleanup
    /// survives a later restrictive DACL copied from the source.
    #[cfg(windows)]
    pub(super) delete_handle: Option<File>,
}

pub(super) enum PreparedInner {
    Existing {
        parent: File,
        file: File,
        name: OsString,
        meta: FileMeta,
        key: String,
    },
    Missing {
        parent: File,
        remaining: Vec<OsString>,
        key: String,
        parent_identity: FileIdentity,
    },
}
