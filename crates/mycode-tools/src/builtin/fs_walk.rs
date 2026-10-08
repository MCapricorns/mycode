//! Handle-relative directory walk for grep/find.
//!
//! Enumeration and ignore-file reads use retained directory handles.
//! The visitor receives that same parent handle plus the listed name so
//! later parent-relative no-follow opens never rebuild a path string or
//! re-walk ancestors from the selected target root. Nested git roots drop
//! outer Gitignore/GitExclude layers; `.ignore` layers stay. Ignore state is
//! a persistent `Arc` linked list so adding a layer is O(1) and ancestor
//! frames keep the previous head. Listings are buffered up to a width cap,
//! decorated once with the lossy rendered component key, sorted with the
//! original `OsString` as the complete tie-break, and visited best-first by the
//! full rendered path. Resolution and walk share one [`WalkLimiter`],
//! including the handle budget.
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use crate::builtin::fs_search::{
    FsEntryKind, HandleLease, IoErrors, MAX_GIT_PARENT_HOPS, NameMatch, ParentDirectory,
    PathOrderKey, ResolvedRoot, WalkLimiter, child_name_in_parent, files_same_identity,
    is_hidden_skip, lossy_component, open_child_file, open_directory_nofollow,
    open_parent_directory, to_posix,
};

mod ignores;

pub(crate) use ignores::{IgnoreStack, relative_is_skipped};

/// Buffer for one `NtQueryDirectoryFile(ReturnSingleEntry = true)` result.
///
/// 64 KiB is well above the Windows component limit while keeping one fixed,
/// bounded allocation per live directory listing.
#[cfg(windows)]
const DIR_LIST_SINGLE_U64S: usize = 8192;

/// Hard cap on bytes loaded from one `.ignore`, `.gitignore`, or git
/// exclude file. Real ignore files are tiny; a larger value would let a
/// huge or sparse ignore pin unbounded memory after the search deadline
/// has already cancelled the worker token. Oversized files fail closed.
pub(crate) const IGNORE_FILE_MAX_BYTES: usize = 1024 * 1024;

/// Ignore-file read size. Checking cancel and the deadline between these
/// kernel reads is what stops `spawn_blocking` from running `read_to_end`
/// after the outer timeout has fired. 8 KiB keeps the check frequent
/// without a syscall per byte.
const IGNORE_READ_CHUNK: usize = 8 * 1024;

/// Visits every non-hidden, non-ignored file and directory under the
/// retained target handle.
///
/// The walk never re-opens `root.root` by path. Directory listings and
/// ignore files are read through handle-relative opens. The visitor is
/// given the retained parent directory handle and the exact listed name
/// so a later open cannot re-parse the relative path or re-walk names
/// from the target root. Frontier directories are ordered by the next
/// child's full rendered path, using the same lossy component key as the
/// listing sort, so a match-count stop is the globally smallest rendered
/// top-N. Other budgets (time, entries, handles) can still stop earlier.
/// Live directory handles are charged to the shared invocation limiter.
/// Exhausted and empty directory frames drop immediately so only the
/// best-first frontier retains charged walk handles.
pub(crate) fn walk_retained_tree(
    root: &ResolvedRoot,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
    io_errors: &IoErrors,
    mut visit: impl FnMut(&Path, &OsStr, FsEntryKind, &File) -> ignore::WalkState,
) -> io::Result<()> {
    if root.is_file() {
        return Ok(());
    }
    let target = match root.target.file.try_clone() {
        Ok(file) => file,
        Err(error) => {
            io_errors.record(".", &error);
            return Ok(());
        }
    };
    let Ok(lease) = root.limiter.lease() else {
        return Ok(());
    };
    let listing = match collect_listing(&target, limiter, cancel) {
        Ok(listing) => listing,
        Err(error) => {
            io_errors.record(".", &error);
            return Ok(());
        }
    };
    if listing.is_empty() {
        return Ok(());
    }
    let mut frames: Vec<Option<WalkFrame>> = vec![Some(WalkFrame {
        dir: target,
        rel: PathBuf::new(),
        ignores: root.ignores.clone(),
        listing,
        next: 0,
        _lease: lease,
    })];
    let mut heap = BinaryHeap::new();
    if let Some(path) = peek_child_path(frames[0].as_ref()) {
        heap.push(Reverse((path, 0usize)));
    }
    while let Some(Reverse((_, frame_idx))) = heap.pop() {
        if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
            return Ok(());
        }
        let Some(frame) = frames.get_mut(frame_idx).and_then(Option::as_mut) else {
            continue;
        };
        if frame.next >= frame.listing.len() {
            frames[frame_idx] = None;
            continue;
        }
        let entry = frame.listing[frame.next].clone();
        frame.next += 1;
        if entry.skip || is_dot(&entry.name) || name_is_hidden(&entry.name) || entry.hidden_attr {
            requeue_or_release(&mut frames, &mut heap, frame_idx);
            continue;
        }
        let parent_rel = frame.rel.clone();
        let parent_ignores = frame.ignores.clone();
        let parent_dir = match frame.dir.try_clone() {
            Ok(file) => file,
            Err(error) => {
                io_errors.record(&rel_label(&frame.rel), &error);
                requeue_or_release(&mut frames, &mut heap, frame_idx);
                continue;
            }
        };
        requeue_or_release(&mut frames, &mut heap, frame_idx);
        let kind = match entry.kind {
            Some(kind) => kind,
            None => match probe_kind(&parent_dir, &entry.name) {
                Ok(Some(kind)) => kind,
                Ok(None) => continue,
                Err(error) => {
                    let child = join_rel(&parent_rel, &entry.name);
                    io_errors.record(&to_posix(&child), &error);
                    continue;
                }
            },
        };
        let child_rel = join_rel(&parent_rel, &entry.name);
        if parent_ignores.is_ignored(
            &root.target_relative,
            &child_rel,
            kind == FsEntryKind::Directory,
        ) {
            continue;
        }
        if matches!(
            visit(&child_rel, &entry.name, kind, &parent_dir),
            ignore::WalkState::Quit
        ) {
            return Ok(());
        }
        if kind != FsEntryKind::Directory {
            continue;
        }
        if child_rel.components().count() >= limiter.max_walk_depth() {
            limiter.stop("walk depth limit reached");
            return Ok(());
        }
        let mut child_ignores = parent_ignores;
        let allowed_rel = join_rel(&root.target_relative, &child_rel);
        let child_dir = match root.open_descended_dir(&parent_dir, &entry.name) {
            Ok(child_dir) => child_dir,
            Err(error) if is_hidden_skip(&error) => continue,
            Err(error) => {
                io_errors.record(&to_posix(&child_rel), &error);
                continue;
            }
        };
        if let Err(error) = child_ignores.ingest(&child_dir, &allowed_rel, limiter, cancel) {
            return Err(io::Error::other(format!(
                "search ignore files cannot be loaded at {}: {error}",
                to_posix(&child_rel)
            )));
        }
        let Ok(child_lease) = root.limiter.lease() else {
            return Ok(());
        };
        let listing = match collect_listing(&child_dir, limiter, cancel) {
            Ok(listing) => listing,
            Err(error) => {
                io_errors.record(&to_posix(&child_rel), &error);
                continue;
            }
        };
        if listing.is_empty() {
            continue;
        }
        let child_idx = frames.len();
        frames.push(Some(WalkFrame {
            dir: child_dir,
            rel: child_rel,
            ignores: child_ignores,
            listing,
            next: 0,
            _lease: child_lease,
        }));
        if let Some(path) = peek_child_path(frames[child_idx].as_ref()) {
            heap.push(Reverse((path, child_idx)));
        }
    }
    Ok(())
}

fn requeue_or_release(
    frames: &mut [Option<WalkFrame>],
    heap: &mut BinaryHeap<Reverse<(PathOrderKey, usize)>>,
    frame_idx: usize,
) {
    let done = match frames.get(frame_idx).and_then(Option::as_ref) {
        Some(frame) => frame.next >= frame.listing.len(),
        None => true,
    };
    if done {
        if let Some(slot) = frames.get_mut(frame_idx) {
            *slot = None;
        }
        return;
    }
    if let Some(path) = peek_child_path(frames[frame_idx].as_ref()) {
        heap.push(Reverse((path, frame_idx)));
    }
}

fn collect_listing(
    dir: &File,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<Vec<ListedName>> {
    let mut listing = DirListing::new();
    let mut names = Vec::new();
    loop {
        match listing.next(dir, limiter, cancel)? {
            Some(entry) => names.push(entry),
            None => return Ok(names),
        }
    }
}

fn sort_listing(entries: Vec<ListedName>) -> Vec<ListedName> {
    let mut decorated: Vec<_> = entries
        .into_iter()
        .map(|entry| {
            (
                (lossy_component(&entry.name).into_owned(), entry.name),
                (entry.kind, entry.skip, entry.hidden_attr),
            )
        })
        .collect();
    decorated.sort_by(|left, right| left.0.cmp(&right.0));
    decorated
        .into_iter()
        .map(|((_, name), (kind, skip, hidden_attr))| ListedName {
            name,
            kind,
            skip,
            hidden_attr,
        })
        .collect()
}

fn peek_child_path(frame: Option<&WalkFrame>) -> Option<PathOrderKey> {
    let frame = frame?;
    let entry = frame.listing.get(frame.next)?;
    Some(PathOrderKey::from_path(&join_rel(&frame.rel, &entry.name)))
}

struct WalkFrame {
    dir: File,
    rel: PathBuf,
    ignores: IgnoreStack,
    listing: Vec<ListedName>,
    next: usize,
    _lease: HandleLease,
}

fn rel_label(rel: &Path) -> String {
    if rel.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        to_posix(rel)
    }
}

fn join_rel(base: &Path, name: impl AsRef<Path>) -> PathBuf {
    let name = name.as_ref();
    if base.as_os_str().is_empty() {
        name.to_path_buf()
    } else if name.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        base.join(name)
    }
}

fn is_dot(name: &OsStr) -> bool {
    name == "." || name == ".."
}

pub(super) fn name_is_hidden(name: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        name.as_bytes().first() == Some(&b'.')
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        name.encode_wide().next() == Some(u16::from(b'.'))
    }
    #[cfg(not(any(unix, windows)))]
    {
        name.to_string_lossy().starts_with('.')
    }
}

#[derive(Clone)]
struct ListedName {
    name: OsString,
    kind: Option<FsEntryKind>,
    skip: bool,
    hidden_attr: bool,
}

/// Per-directory listing. Names are collected up to the width cap, then
/// sorted so match-budget truncation is a deterministic global prefix.
struct DirListing {
    pending: Vec<ListedName>,
    next_index: usize,
    loaded: bool,
    #[cfg(unix)]
    dirp: Option<UnixDirOwner>,
    #[cfg(windows)]
    words: Vec<u64>,
    #[cfg(windows)]
    restart: bool,
    #[cfg(windows)]
    exhausted: bool,
    #[cfg(not(any(unix, windows)))]
    _unsupported: (),
}

impl DirListing {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            next_index: 0,
            loaded: false,
            #[cfg(unix)]
            dirp: None,
            #[cfg(windows)]
            words: Vec::new(),
            #[cfg(windows)]
            restart: true,
            #[cfg(windows)]
            exhausted: false,
            #[cfg(not(any(unix, windows)))]
            _unsupported: (),
        }
    }

    fn next(
        &mut self,
        dir: &File,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<Option<ListedName>> {
        if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
            return Ok(None);
        }
        if !self.loaded {
            self.load(dir, limiter, cancel)?;
        }
        if self.next_index >= self.pending.len() {
            return Ok(None);
        }
        let entry = self.pending[self.next_index].clone();
        self.next_index += 1;
        Ok(Some(entry))
    }

    fn load(
        &mut self,
        dir: &File,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<()> {
        let mut entries = Vec::new();
        let width = limiter.max_dir_width();
        loop {
            if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
                self.loaded = true;
                return Ok(());
            }
            match self.next_platform(dir, limiter, cancel)? {
                None => break,
                Some(entry) => {
                    if entries.len() >= width {
                        limiter.stop("directory width limit reached");
                        return Err(io::Error::other("directory width limit reached"));
                    }
                    entries.push(entry);
                }
            }
        }
        self.pending = sort_listing(entries);
        self.next_index = 0;
        self.loaded = true;
        Ok(())
    }

    #[cfg(unix)]
    fn next_platform(
        &mut self,
        dir: &File,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<Option<ListedName>> {
        use std::ffi::CStr;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::IntoRawFd;

        if self.dirp.is_none() {
            // CONTRACT: reopen "." rather than cloning `dir`'s descriptor.
            // `File::try_clone` shares the open-file description, so a second
            // listing of the same directory would resume at the previous
            // scan's EOF and silently miss entries.
            // `fs_search::unix_reopen_dir_for_listing` implements the same
            // contract for the alias-spelling scans.
            let listing = crate::builtin::fs_search::unix_reopen_dir_for_listing(dir)?;
            let fd = listing.into_raw_fd();
            // SAFETY: `fd` is exclusively owned. `fdopendir` either takes it
            // or we close it on the failure path below.
            let dirp = unsafe { libc::fdopendir(fd) };
            if dirp.is_null() {
                let error = io::Error::last_os_error();
                // SAFETY: `fdopendir` failed, so this process still owns `fd`.
                unsafe {
                    libc::close(fd);
                }
                return Err(error);
            }
            self.dirp = Some(UnixDirOwner(dirp));
        }
        let owner = self.dirp.as_ref().expect("unix listing started");
        loop {
            if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
                return Ok(None);
            }
            if !limiter.try_reserve_entry() {
                return Err(io::Error::other("walk entry limit reached"));
            }
            crate::builtin::fs_search::unix_clear_errno();
            // SAFETY: `owner.0` is a live `DIR*`. A non-null `dirent` is valid
            // until the next `readdir`/`closedir` on this stream.
            let entry = unsafe { libc::readdir(owner.0) };
            if entry.is_null() {
                limiter.release_entry();
                let error = io::Error::last_os_error();
                if error.raw_os_error().unwrap_or(0) == 0 {
                    return Ok(None);
                }
                return Err(error);
            }
            // SAFETY: `d_name` is a NUL-terminated component from `readdir`.
            let c_name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            let name = OsStr::from_bytes(c_name.to_bytes());
            if is_dot(name) {
                limiter.release_entry();
                continue;
            }
            let file_type = unsafe { (*entry).d_type };
            let (kind, skip) = match file_type {
                libc::DT_DIR => (Some(FsEntryKind::Directory), false),
                libc::DT_REG => (Some(FsEntryKind::File), false),
                libc::DT_LNK => (None, true),
                libc::DT_UNKNOWN => (None, false),
                _ => (None, true),
            };
            return Ok(Some(ListedName {
                name: name.to_os_string(),
                kind,
                skip,
                hidden_attr: false,
            }));
        }
    }

    #[cfg(windows)]
    fn next_platform(
        &mut self,
        dir: &File,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<Option<ListedName>> {
        use std::os::windows::io::AsRawHandle;
        use std::ptr::{null, null_mut};
        use windows_sys::Wdk::Storage::FileSystem::{
            FileFullDirectoryInformation, NtQueryDirectoryFile,
        };
        use windows_sys::Win32::Foundation::{
            RtlNtStatusToDosError, STATUS_NO_MORE_FILES, STATUS_SUCCESS,
        };
        use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

        if self.exhausted {
            return Ok(None);
        }
        loop {
            if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
                return Ok(None);
            }
            if !limiter.try_reserve_entry() {
                return Err(io::Error::other("walk entry limit reached"));
            }
            if self.words.is_empty() {
                self.words.resize(DIR_LIST_SINGLE_U64S, 0);
            }
            let byte_len = u32::try_from(self.words.len().saturating_mul(8)).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "directory listing buffer is too large",
                )
            })?;
            let mut io_status = IO_STATUS_BLOCK::default();
            // SAFETY: `dir` is a live synchronous directory handle; `words`
            // is aligned writable storage; null event/APC/name pointers select
            // a synchronous unfiltered query. `ReturnSingleEntry` prevents the
            // kernel from materializing more names than the one reservation.
            let status = unsafe {
                NtQueryDirectoryFile(
                    dir.as_raw_handle(),
                    null_mut(),
                    None,
                    null(),
                    &mut io_status,
                    self.words.as_mut_ptr().cast(),
                    byte_len,
                    FileFullDirectoryInformation,
                    true,
                    null(),
                    self.restart,
                )
            };
            if status == STATUS_NO_MORE_FILES {
                limiter.release_entry();
                self.exhausted = true;
                return Ok(None);
            }
            if status != STATUS_SUCCESS {
                limiter.release_entry();
                // `NtQueryDirectoryFile` returns NTSTATUS and does not define
                // `GetLastError`; convert the returned value directly.
                let code = unsafe { RtlNtStatusToDosError(status) };
                return Err(io::Error::from_raw_os_error(code as i32));
            }
            self.restart = false;
            let used = io_status.Information;
            if used == 0 || used > self.words.len().saturating_mul(8) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "directory query returned an invalid byte count",
                ));
            }
            let (entry, next_offset) =
                parse_one_full_dir_info(self.words.as_ptr().cast(), used, 0)?;
            if next_offset != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "single-entry directory query returned multiple records",
                ));
            }
            if let Some(entry) = entry {
                return Ok(Some(entry));
            }
            limiter.release_entry();
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn next_platform(
        &mut self,
        _dir: &File,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<Option<ListedName>> {
        let _ = (limiter, cancel);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative directory listing is not implemented on this platform",
        ))
    }
}

#[cfg(unix)]
struct UnixDirOwner(*mut libc::DIR);

#[cfg(unix)]
impl Drop for UnixDirOwner {
    fn drop(&mut self) {
        // SAFETY: `dirp` is exclusively owned and not yet closed.
        unsafe {
            libc::closedir(self.0);
        }
    }
}

#[cfg(unix)]
fn probe_kind(parent: &File, name: &OsStr) -> io::Result<Option<FsEntryKind>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;

    let c_name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path component contains NUL"))?;
    // SAFETY: `stat` is an out-parameter written by `fstatat`.
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    // SAFETY: `parent` is a live directory fd, `c_name` is NUL-terminated,
    // and `AT_SYMLINK_NOFOLLOW` inspects the named child itself.
    let status = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            c_name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    let format = stat.st_mode & libc::S_IFMT;
    if format == libc::S_IFLNK {
        Ok(None)
    } else if format == libc::S_IFDIR {
        Ok(Some(FsEntryKind::Directory))
    } else if format == libc::S_IFREG {
        Ok(Some(FsEntryKind::File))
    } else {
        Ok(None)
    }
}

#[cfg(windows)]
fn parse_one_full_dir_info(
    base: *const u8,
    cap: usize,
    offset: usize,
) -> io::Result<(Option<ListedName>, usize)> {
    use std::mem::offset_of;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FULL_DIR_INFO,
    };

    let header_size = offset_of!(FILE_FULL_DIR_INFO, FileName);
    if offset.saturating_add(header_size) > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated directory listing",
        ));
    }
    // SAFETY: `offset + header` is inside `cap`, so the fixed prefix
    // of `FILE_FULL_DIR_INFO` can be read.
    let info = unsafe { &*base.add(offset).cast::<FILE_FULL_DIR_INFO>() };
    let name_bytes = info.FileNameLength as usize;
    if !name_bytes.is_multiple_of(2)
        || offset
            .saturating_add(header_size)
            .saturating_add(name_bytes)
            > cap
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory entry name is truncated",
        ));
    }
    let name_units = name_bytes / 2;
    // SAFETY: `FileName` is `name_units` UTF-16 code units inside the
    // same listing buffer already bounds-checked above.
    let name = unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), name_units) };
    let name = OsString::from_wide(name);
    let entry = if is_dot(&name) {
        None
    } else {
        let attributes = info.FileAttributes;
        let reparse = attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
        let directory = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        Some(ListedName {
            name,
            kind: if reparse {
                None
            } else if directory {
                Some(FsEntryKind::Directory)
            } else {
                Some(FsEntryKind::File)
            },
            skip: reparse,
            hidden_attr: attributes & FILE_ATTRIBUTE_HIDDEN != 0,
        })
    };
    Ok((entry, info.NextEntryOffset as usize))
}

#[cfg(windows)]
fn probe_kind(_parent: &File, _name: &OsStr) -> io::Result<Option<FsEntryKind>> {
    Ok(None)
}

#[cfg(not(any(unix, windows)))]
fn probe_kind(_parent: &File, _name: &OsStr) -> io::Result<Option<FsEntryKind>> {
    Ok(None)
}
