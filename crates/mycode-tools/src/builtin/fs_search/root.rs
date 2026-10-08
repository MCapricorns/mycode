//! Handle-backed search root: stable identities, target open, and parent walks.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::*;

/// Type of object proven by an opened handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FsEntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    pub(crate) volume: u64,
    pub(crate) file_id: [u8; 16],
}

#[derive(Debug)]
pub(crate) struct StableHandle {
    pub(crate) file: File,
    pub(crate) identity: FileIdentity,
    pub(crate) kind: FsEntryKind,
    /// When set, `file` is the parent directory and identity is `name`.
    ///
    /// Used on Unix platforms without `O_PATH` so find can retain a
    /// metadata-only capability for a mode-`000` file.
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    pub(crate) named_child: Option<OsString>,
    #[cfg(windows)]
    pub(crate) final_path: PathBuf,
    /// Windows metadata handles omit `SYNCHRONIZE` and must not be read.
    #[cfg(windows)]
    pub(crate) metadata_only: bool,
}

impl StableHandle {
    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
            identity: self.identity,
            kind: self.kind,
            #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
            named_child: self.named_child.clone(),
            #[cfg(windows)]
            final_path: self.final_path.clone(),
            #[cfg(windows)]
            metadata_only: self.metadata_only,
        })
    }

    pub(crate) fn current_identity(&self) -> io::Result<(FileIdentity, FsEntryKind)> {
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
        if let Some(name) = &self.named_child {
            return unix_named_identity(&self.file, name);
        }
        identity_and_kind(&self.file)
    }

    pub(crate) fn is_content_file(&self) -> bool {
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
        {
            self.named_child.is_none()
        }
        #[cfg(windows)]
        {
            !self.metadata_only
        }
        #[cfg(not(any(
            windows,
            all(unix, not(any(target_os = "linux", target_os = "android")))
        )))]
        {
            true
        }
    }
}

pub(crate) fn handle_is_hidden(handle: &StableHandle, name: &OsStr) -> io::Result<bool> {
    if walk::name_is_hidden(name) {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        let _ = name;
        windows_file_is_hidden(&handle.file)
    }
    #[cfg(not(windows))]
    {
        let _ = handle;
        Ok(false)
    }
}

/// Reads the current hidden bit from an already opened content handle.
pub(crate) fn opened_file_is_hidden(file: &File) -> io::Result<bool> {
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

/// A search root backed by retained allowed-root and target handles.
///
/// `root` and `cwd` are for rendering and diagnostics only. Enumeration
/// and security decisions use `allowed`/`target` handles and each entry's
/// newly opened handle. Resolution and the walker share `limiter` so
/// ignore/handle budgets cannot be spent twice.
pub(crate) struct ResolvedRoot {
    /// Stable display spelling of the selected search root.
    pub root: PathBuf,
    /// Stable display spelling of the allowed session cwd.
    pub cwd: PathBuf,
    /// Handle-relative path from the allowed root to the selected target.
    pub(crate) target_relative: PathBuf,
    pub(crate) allowed: StableHandle,
    pub(crate) target: StableHandle,
    /// Ignore files from the allowed root through every ancestor opened while
    /// resolving `target`. Compiled from those retained handles, not by
    /// re-opening intermediate names later.
    pub(crate) ignores: walk::IgnoreStack,
    /// Invocation limiter shared by resolution and the subsequent walk.
    pub limiter: Arc<WalkLimiter>,
    /// True when any resolved component had a hidden name or attribute.
    pub(crate) hidden: bool,
    pub(crate) _allowed_lease: HandleLease,
    pub(crate) _target_lease: HandleLease,
}

impl std::fmt::Debug for ResolvedRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedRoot")
            .field("root", &self.root)
            .field("cwd", &self.cwd)
            .field("target_relative", &self.target_relative)
            .field("hidden", &self.hidden)
            .finish_non_exhaustive()
    }
}

impl ResolvedRoot {
    /// Returns whether the selected root is one file.
    pub fn is_file(&self) -> bool {
        self.target.kind == FsEntryKind::File
    }

    /// Returns whether the selected target is hidden or ignore-excluded.
    ///
    /// Applied to the on-disk allowed-relative spelling after alias open,
    /// so an explicit `path` of a gitignored file, a hidden name, a Windows
    /// `FILE_ATTRIBUTE_HIDDEN` component, or a case/8.3 alias of either is
    /// skipped the same way as a walker descendant. The session cwd itself
    /// is never skipped. Only a proven hidden bit is a silent skip; a
    /// hidden-attribute query failure is returned to the caller.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the hidden-attribute query fails.
    pub fn target_is_skipped(&self) -> io::Result<bool> {
        if self.target_relative.as_os_str().is_empty() {
            return Ok(false);
        }
        let name = self
            .target_relative
            .file_name()
            .unwrap_or_else(|| OsStr::new(""));
        if handle_is_hidden(&self.target, name)? {
            return Ok(true);
        }
        Ok(self.hidden
            || walk::relative_is_skipped(&self.ignores, &self.target_relative, !self.is_file()))
    }

    /// Handle-relative path from the allowed root to the selected target.
    ///
    /// On Windows this is the on-disk component spelling after alias open.
    /// Returns a mutable reference to the already opened single-file target.
    pub fn target_file_mut(&mut self) -> io::Result<&mut File> {
        if !self.is_file() {
            return Err(io::Error::other("search target is not a file"));
        }
        if !self.target.is_content_file() {
            return Err(io::Error::other("search target was bound metadata-only"));
        }
        self.validate_target()?;
        Ok(&mut self.target.file)
    }

    /// Revalidates the retained target handle immediately before reporting.
    pub fn validate_target(&self) -> io::Result<()> {
        let (identity, kind) = self.target.current_identity()?;
        if identity != self.target.identity || kind != self.target.kind {
            return Err(io::Error::other("search target identity changed"));
        }
        let name = self
            .target_relative
            .file_name()
            .unwrap_or_else(|| OsStr::new(""));
        if !self.target_relative.as_os_str().is_empty() && handle_is_hidden(&self.target, name)? {
            return Err(hidden_entry_error());
        }
        let allowed_identity = identity_and_kind(&self.allowed.file)?.0;
        if allowed_identity != self.allowed.identity {
            return Err(io::Error::other("allowed-root identity changed"));
        }
        #[cfg(windows)]
        {
            let final_path = final_path_by_handle(&self.target.file)?;
            if !is_within(&self.allowed.final_path, &final_path) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "opened target is outside the allowed root",
                ));
            }
        }
        Ok(())
    }

    /// Opens one listed child for content read relative to the parent handle.
    ///
    /// `name` is the directory-entry spelling from the same parent handle.
    /// The open is parent-relative and no-follow; it never rebuilds a path
    /// and never re-walks ancestors from the selected target. Windows uses
    /// the enumerated spelling case-sensitively so a listing of `Visible.txt`
    /// cannot be redirected to ignore-excluded `visible.txt`. Unix requests
    /// `O_RDONLY`; Windows requests `FILE_GENERIC_READ`.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent is no longer a directory, `name` is not
    /// a single safe component, the child type changed, a link is encountered,
    /// or Windows containment against the allowed root fails.
    pub fn open_walked(
        &self,
        parent: &File,
        name: &OsStr,
        expected: FsEntryKind,
    ) -> io::Result<File> {
        self.validate_target()?;
        validate_component_name(name)?;
        let (_, parent_kind) = identity_and_kind(parent)?;
        if parent_kind != FsEntryKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walk parent is no longer a directory",
            ));
        }

        let opened = open_child_handle(parent, name, Some(expected), SearchAccess::Content)?;
        let (identity, kind) = opened.current_identity()?;
        if identity != opened.identity || kind != expected || opened.kind != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walked object type or identity changed before it was opened",
            ));
        }
        #[cfg(windows)]
        if !is_within(&self.allowed.final_path, &opened.final_path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "opened entry is outside the allowed root",
            ));
        }
        if handle_is_hidden(&opened, name)? {
            return Err(hidden_entry_error());
        }
        Ok(opened.file)
    }

    /// Opens a listed directory for descent after proving metadata identity.
    pub fn open_descended_dir(&self, parent: &File, name: &OsStr) -> io::Result<File> {
        self.validate_target()?;
        validate_component_name(name)?;
        let (_, parent_kind) = identity_and_kind(parent)?;
        if parent_kind != FsEntryKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walk parent is no longer a directory",
            ));
        }
        let meta_identity = confirm_child_identity(parent, name, FsEntryKind::Directory)?;
        let opened = open_child_handle(
            parent,
            name,
            Some(FsEntryKind::Directory),
            SearchAccess::Content,
        )?;
        let (identity, kind) = opened.current_identity()?;
        if identity != meta_identity
            || identity != opened.identity
            || kind != FsEntryKind::Directory
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walked object type or identity changed before it was opened",
            ));
        }
        #[cfg(windows)]
        if !is_within(&self.allowed.final_path, &opened.final_path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "opened entry is outside the allowed root",
            ));
        }
        if handle_is_hidden(&opened, name)? {
            return Err(hidden_entry_error());
        }
        Ok(opened.file)
    }

    /// Confirms one listed child without requiring content-read permission.
    ///
    /// Find reports names the caller can list even when file contents are
    /// unreadable. Linux/Android confirm with `openat2` + `O_PATH` and the
    /// same `RESOLVE_BENEATH | NO_XDEV | NO_SYMLINKS` bits as descent.
    /// Other Unix confirms with no-follow metadata and the current
    /// mount / type / `nlink` checks, and fails closed when that proof is
    /// unavailable. Windows opens with `FILE_READ_ATTRIBUTES` rather than
    /// `FILE_GENERIC_READ`.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent is no longer a directory, `name` is
    /// not a single safe component, the child type changed, a link is
    /// encountered, a mount boundary cannot be proven, or Windows containment
    /// against the allowed root fails.
    pub fn confirm_walked(
        &self,
        parent: &File,
        name: &OsStr,
        expected: FsEntryKind,
    ) -> io::Result<()> {
        self.validate_target()?;
        validate_component_name(name)?;
        let (_, parent_kind) = identity_and_kind(parent)?;
        if parent_kind != FsEntryKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walk parent is no longer a directory",
            ));
        }
        let _identity = confirm_child_identity(parent, name, expected)?;
        Ok(())
    }
}

fn confirm_child_identity(
    parent: &File,
    name: &OsStr,
    expected: FsEntryKind,
) -> io::Result<FileIdentity> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let file = open_named_unix(parent, name, Some(expected), SearchAccess::Metadata)?;
        let opened = stable_from_file(file)?;
        if handle_is_hidden(&opened, name)? {
            return Err(hidden_entry_error());
        }
        Ok(opened.identity)
    }

    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    {
        confirm_named_unix_metadata(parent, name, Some(expected))?;
        let (identity, kind) = unix_named_identity(parent, name)?;
        if kind != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walked object type or identity changed before it was opened",
            ));
        }
        if walk::name_is_hidden(name) {
            return Err(hidden_entry_error());
        }
        Ok(identity)
    }

    #[cfg(windows)]
    {
        let opened = open_named_windows(
            parent,
            name,
            Some(expected),
            NameMatch::Exact,
            SearchAccess::Metadata,
        )?;
        let (identity, kind) = identity_and_kind(&opened.file)?;
        if identity != opened.identity || kind != expected || opened.kind != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walked object type or identity changed before it was opened",
            ));
        }
        if handle_is_hidden(&opened, name)? {
            return Err(hidden_entry_error());
        }
        Ok(opened.identity)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name, expected);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative child confirm is not implemented on this platform",
        ))
    }
}

fn open_child_handle(
    parent: &File,
    name: &OsStr,
    expected: Option<FsEntryKind>,
    access: SearchAccess,
) -> io::Result<StableHandle> {
    #[cfg(unix)]
    {
        stable_from_file(open_named_unix(parent, name, expected, access)?)
    }
    #[cfg(windows)]
    {
        open_named_windows(parent, name, expected, NameMatch::Exact, access)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name, expected, access);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative child open is not implemented on this platform",
        ))
    }
}

/// How a path component is matched when opening a child.
///
/// User-typed aliases stay case-insensitive on Windows. Names taken from a
/// directory listing must open the exact enumerated object so a different-case
/// sibling cannot be substituted on a case-sensitive volume.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NameMatch {
    /// User-typed path aliases. Windows opens with `OBJ_CASE_INSENSITIVE`.
    #[cfg(windows)]
    Alias,
    /// Directory-entry spellings and well-known ignore file names.
    Exact,
}

/// Opens one child name relative to an already retained parent handle.
pub(crate) fn open_child_file(
    parent: &File,
    name: &OsStr,
    expected: Option<FsEntryKind>,
    name_match: NameMatch,
) -> io::Result<File> {
    #[cfg(unix)]
    {
        let _ = name_match;
        open_named_unix(parent, name, expected, SearchAccess::Content)
    }
    #[cfg(windows)]
    {
        Ok(open_named_windows(parent, name, expected, name_match, SearchAccess::Content)?.file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name, expected, name_match);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative child open is not implemented on this platform",
        ))
    }
}

/// Result of opening the parent of a live directory handle.
#[derive(Debug)]
pub(crate) enum ParentDirectory {
    /// Opened parent directory.
    Parent(File),
    /// `dir` is the filesystem root; there is no parent.
    FilesystemRoot,
}

/// Opens the parent directory of `dir` via handle-relative `..`.
///
/// Used only for Git-boundary discovery. Does not follow the final link
/// and does not apply search-root mount containment: walking up may
/// cross a bind mount to reach the real Git common dir. Missing parents
/// and open faults fail closed; only a proven filesystem root returns
/// [`ParentDirectory::FilesystemRoot`].
///
/// # Errors
///
/// Returns an I/O error when `..` cannot be opened as a directory, or
/// when a path-derived parent no longer contains `dir`.
pub(crate) fn open_parent_directory(dir: &File) -> io::Result<ParentDirectory> {
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};

        let c_dotdot = c"..";
        let flags = libc::O_RDONLY
            | libc::O_NONBLOCK
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_DIRECTORY;
        // Parent discovery must not use `RESOLVE_BENEATH`; `..` is the point.
        // SAFETY: `dir` is a live directory fd and `c_dotdot` is `..\0`.
        let descriptor = unsafe { libc::openat(dir.as_raw_fd(), c_dotdot.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `descriptor` is a fresh owned fd from `openat`.
        let parent = unsafe { File::from_raw_fd(descriptor) };
        if files_same_identity(&parent, dir)? {
            return Ok(ParentDirectory::FilesystemRoot);
        }
        Ok(ParentDirectory::Parent(parent))
    }
    #[cfg(windows)]
    {
        open_windows_parent_directory(dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative parent open is not implemented on this platform",
        ))
    }
}

fn require_parent_directory(opened: ParentDirectory) -> io::Result<File> {
    match opened {
        ParentDirectory::Parent(file) => Ok(file),
        ParentDirectory::FilesystemRoot => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path escaped past filesystem root",
        )),
    }
}

/// Returns whether two opened objects share identity.
pub(crate) fn files_same_identity(left: &File, right: &File) -> io::Result<bool> {
    Ok(identity_and_kind(left)?.0 == identity_and_kind(right)?.0)
}

/// Directory-entry spelling of `child` inside `parent`.
pub(crate) fn child_name_in_parent(
    parent: &File,
    child: &File,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<OsString> {
    #[cfg(unix)]
    {
        let identity = identity_and_kind(child)?.0;
        unix_on_disk_component_name(parent, identity, limiter, cancel)
    }
    #[cfg(windows)]
    {
        let _ = (limiter, cancel);
        windows_child_name_in_parent(parent, child)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, child, limiter, cancel);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "child name proof is not implemented on this platform",
        ))
    }
}

/// Opens an absolute directory with no-follow component walks for Git metadata.
///
/// # Errors
///
/// Returns an I/O error when any component cannot be opened without following
/// a link, or when the path is not a directory.
pub(crate) fn open_directory_nofollow(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;

        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "git metadata path is not absolute",
            ));
        }
        let c_root = c"/";
        let flags = libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_DIRECTORY;
        // SAFETY: `c_root` is a live C string; `AT_FDCWD` is the documented
        // cwd-relative starting point for the filesystem root.
        let root_fd = unsafe { libc::openat(libc::AT_FDCWD, c_root.as_ptr(), flags) };
        if root_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut current = unsafe { File::from_raw_fd(root_fd) };
        for component in path.components() {
            match component {
                Component::RootDir | Component::Prefix(_) => {}
                Component::CurDir => {}
                Component::ParentDir => {
                    current = require_parent_directory(open_parent_directory(&current)?)?;
                }
                Component::Normal(name) => {
                    current = open_named_unix(
                        &current,
                        name,
                        Some(FsEntryKind::Directory),
                        SearchAccess::Content,
                    )?;
                }
            }
        }
        Ok(current)
    }
    #[cfg(windows)]
    {
        let mut components = path.components();
        let Some(first) = components.next() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "git metadata path is empty",
            ));
        };
        let mut prefix = PathBuf::from(first.as_os_str());
        if let Some(Component::RootDir) = components.clone().next() {
            let _ = components.next();
            prefix.push(std::path::MAIN_SEPARATOR_STR);
        }
        let mut current = open_windows_prefix(&prefix)?.file;
        for component in components {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    current = require_parent_directory(open_parent_directory(&current)?)?;
                }
                Component::Normal(name) => {
                    current = open_named_windows(
                        &current,
                        name,
                        Some(FsEntryKind::Directory),
                        NameMatch::Exact,
                        SearchAccess::Content,
                    )?
                    .file;
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "git metadata path has an interior prefix",
                    ));
                }
            }
        }
        Ok(current)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no-follow directory open is not implemented on this platform",
        ))
    }
}

/// Rejects empty names, `.`/`..`, NULs, and platform path separators.
///
/// Callers must pass a directory-entry spelling, never a reconstructed
/// relative path. A separator would re-walk ancestors from the parent.
pub(crate) fn validate_component_name(name: &OsStr) -> io::Result<()> {
    if name.is_empty() || name == "." || name == ".." || os_name_has_separator_or_nul(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "walked entry is not a single path component",
        ));
    }
    Ok(())
}

pub(crate) fn os_name_has_separator_or_nul(name: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        name.as_bytes()
            .iter()
            .any(|&byte| byte == 0 || byte == b'/')
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        // Colon is NTFS ADS / stream syntax (`file.txt:stream`). ADS names
        // are not directory-enumerated, so they would bypass ignore/policy
        // unless rejected before `NtOpenFile`.
        name.encode_wide().any(|unit| {
            unit == 0
                || unit == u16::from(b'/')
                || unit == u16::from(b'\\')
                || unit == u16::from(b':')
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        name.to_string_lossy()
            .chars()
            .any(|ch| ch == '\0' || ch == '/' || ch == '\\' || ch == ':')
    }
}

#[cfg(unix)]
pub(crate) fn open_allowed_root(path: &Path) -> io::Result<StableHandle> {
    stable_from_file(File::open(path)?)
}

#[cfg(windows)]
pub(crate) fn open_allowed_root(path: &Path) -> io::Result<StableHandle> {
    open_windows(path)
}
