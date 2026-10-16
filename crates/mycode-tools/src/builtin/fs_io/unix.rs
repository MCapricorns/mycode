//! macOS handle-relative file operations for the host file kernel.
//!
//! Each component opens with rustix `openat` and `O_NOFOLLOW` (plus
//! `O_DIRECTORY` for directories), then `fstat`/`statat` device and type
//! checks keep the walk on one mount. Hardlinks are allowed; callers detach
//! them by publishing a new inode.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use rustix::fs::{self as rfs, AtFlags, Mode, OFlags};
use rustix::io::Errno;

use super::{
    ChildOpen, FileIdentity, FileKind, FileMeta, MAX_DIR_WIDTH, OpenedChild, WRITE_CHUNK,
    check_cancel, map_not_found,
};
use crate::builtin::fs_search::{unix_device_identity, validate_component_name};
use tokio_util::sync::CancellationToken;

/// Creation mode for the never-written mode probe file. The kernel applies
/// the process umask and any parent default ACL; the effective result is
/// read back with `fstat` and applied to the published payload afterwards
/// through its retained handle.
const TEMP_CREATE_MODE: rfs::RawMode = 0o666;
/// Mode held by the payload temp from creation through the rename: it has
/// no group/other bits, so no umask or parent default ACL can expose
/// written content before or after a failed publish.
const TEMP_PRIVATE_MODE: rfs::RawMode = 0o600;
/// Creation mode for missing directory components. `mkdirat` masks it with
/// the process umask, matching `mkdir(1)`'s `0777 & ~umask` convention.
const DIR_CREATE_MODE: rfs::RawMode = 0o777;

fn map_errno(err: Errno) -> io::Error {
    match err {
        Errno::LOOP => io::Error::new(
            io::ErrorKind::InvalidInput,
            "symlink traversal is not permitted",
        ),
        Errno::XDEV => io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount traversal is not permitted",
        ),
        other => io::Error::from(other),
    }
}

fn open_named(
    parent: &File,
    name: &OsStr,
    oflags: OFlags,
    mode: Mode,
) -> io::Result<std::os::fd::OwnedFd> {
    validate_component_name(name)?;
    rfs::openat(parent.as_fd(), name, oflags, mode).map_err(map_errno)
}

fn stat_meta(file: &File) -> io::Result<FileMeta> {
    let stat = rfs::fstat(file.as_fd()).map_err(map_errno)?;
    meta_from_stat(&stat)
}

fn checked_u64<T>(value: T, field: &str) -> io::Result<u64>
where
    u64: TryFrom<T>,
{
    u64::try_from(value).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("filesystem {field} is outside the supported range"),
        )
    })
}

fn checked_i64<T>(value: T, field: &str) -> io::Result<i64>
where
    i64: TryFrom<T>,
{
    i64::try_from(value).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("filesystem {field} is outside the supported range"),
        )
    })
}

fn checked_u32<T>(value: T, field: &str) -> io::Result<u32>
where
    u32: TryFrom<T>,
{
    u32::try_from(value).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("filesystem {field} is outside the supported range"),
        )
    })
}

fn meta_from_stat(stat: &rfs::Stat) -> io::Result<FileMeta> {
    let file_type = rfs::FileType::from_raw_mode(stat.st_mode);
    let kind = match file_type {
        rfs::FileType::RegularFile => FileKind::File,
        rfs::FileType::Directory => FileKind::Directory,
        rfs::FileType::Symlink => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "symlink traversal is not permitted",
            ));
        }
        rfs::FileType::Fifo => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "FIFO targets are not permitted",
            ));
        }
        rfs::FileType::Socket => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "socket targets are not permitted",
            ));
        }
        rfs::FileType::CharacterDevice | rfs::FileType::BlockDevice => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "device targets are not permitted",
            ));
        }
        rfs::FileType::Unknown => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "opened object is neither a regular file nor a directory",
            ));
        }
    };
    let size = u64::try_from(stat.st_size)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative file size is invalid"))?;
    Ok(FileMeta {
        identity: FileIdentity {
            device: unix_device_identity(stat.st_dev)?,
            inode: checked_u64(stat.st_ino, "inode number")?,
        },
        kind,
        size,
        mtime_secs: checked_i64(stat.st_mtime, "modification time")?,
        mtime_nsecs: checked_u32(stat.st_mtime_nsec, "modification nanoseconds")?,
        nlink: checked_u64(stat.st_nlink, "hard-link count")?,
        unix_mode: checked_u32(rfs::Mode::from_raw_mode(stat.st_mode).as_raw_mode(), "mode")?,
        unix_uid: checked_u32(stat.st_uid, "user id")?,
        unix_gid: checked_u32(stat.st_gid, "group id")?,
    })
}

fn mount_traversal_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "mount traversal is not permitted",
    )
}

/// Rejects a child whose `st_dev` differs from its parent's.
fn reject_mount_cross(parent_device: u64, child_device: u64) -> io::Result<()> {
    if parent_device == child_device {
        Ok(())
    } else {
        Err(mount_traversal_error())
    }
}

fn mount_device(file: &File) -> io::Result<u64> {
    let stat = rfs::fstat(file.as_fd()).map_err(map_errno)?;
    unix_device_identity(stat.st_dev)
}

/// Rejects a child that sits on a different mount from `parent`.
///
/// Shared by temp-file creation and by the post-open check in [`open_child`].
fn enforce_same_mount(parent: &File, child: &File) -> io::Result<()> {
    reject_mount_cross(mount_device(parent)?, mount_device(child)?)
}

fn into_file(fd: std::os::fd::OwnedFd) -> File {
    File::from(fd)
}

/// Removes a just-created named inode unless ownership is transferred.
struct CreatedName<'a> {
    parent: &'a File,
    name: &'a OsStr,
    linked: bool,
}

impl<'a> CreatedName<'a> {
    fn new(parent: &'a File, name: &'a OsStr) -> Self {
        Self {
            parent,
            name,
            linked: true,
        }
    }

    fn finish<T>(mut self, result: io::Result<T>) -> io::Result<T> {
        match result {
            Ok(value) => {
                self.linked = false;
                Ok(value)
            }
            Err(primary) => match self.remove() {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(io::Error::new(
                    primary.kind(),
                    format!("{primary}; failed to remove rejected temporary file: {cleanup}"),
                )),
            },
        }
    }

    fn remove(&mut self) -> io::Result<()> {
        if !self.linked {
            return Ok(());
        }
        unlink_child(self.parent, self.name)?;
        self.linked = false;
        Ok(())
    }
}

impl Drop for CreatedName<'_> {
    fn drop(&mut self) {
        // `finish` reports cleanup failures. This is only a panic/unwind
        // fallback, where Drop cannot return another error.
        let _ = self.remove();
    }
}

/// Opens the host-selected session cwd. The cwd path itself may follow.
///
/// # Errors
///
/// Returns an I/O error when `path` cannot be opened as a directory.
pub(super) fn open_allowed_root(path: &std::path::Path) -> io::Result<File> {
    let fd = rfs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(map_errno)?;
    let file = into_file(fd);
    let meta = stat_meta(&file)?;
    if meta.kind != FileKind::Directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session cwd is not a directory",
        ));
    }
    Ok(file)
}

/// Opens one no-follow child relative to `parent`.
///
/// A `statat(AT_SYMLINK_NOFOLLOW)` runs first so FIFO/device/socket/symlink
/// names are rejected without a blocking open. Directories are then opened
/// with `O_NOFOLLOW | O_DIRECTORY`. The opened fd is re-checked with `fstat`
/// for type and identity, and for mount identity (`st_dev`) against the
/// parent.
///
/// # Errors
///
/// Returns an I/O error when the name is unsafe, the object is the wrong type,
/// a link or mount is crossed, or the open fails.
pub(super) fn open_child(parent: &File, name: &OsStr, how: ChildOpen) -> io::Result<OpenedChild> {
    validate_component_name(name)?;
    let named = match rfs::statat(parent.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => meta_from_stat(&stat)?,
        Err(Errno::NOENT) => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "path component does not exist",
            ));
        }
        Err(err) => return Err(map_not_found(map_errno(err))),
    };
    reject_mount_cross(mount_device(parent)?, named.identity.device)?;
    match how {
        ChildOpen::Directory if named.kind != FileKind::Directory => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "path component is not a directory",
            ));
        }
        ChildOpen::ExistingFile if named.kind != FileKind::File => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "path is not a regular file",
            ));
        }
        _ => {}
    }
    let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    if named.kind == FileKind::Directory || matches!(how, ChildOpen::Directory) {
        flags |= OFlags::DIRECTORY;
    }
    let file = match open_named(parent, name, flags, Mode::empty()) {
        Ok(fd) => into_file(fd),
        Err(error) => return Err(map_not_found(error)),
    };
    enforce_same_mount(parent, &file)?;
    let meta = stat_meta(&file)?;
    if meta.identity != named.identity || meta.kind != named.kind {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened object type or identity changed before it was used",
        ));
    }
    if let Some(expect) = match how {
        ChildOpen::Directory => Some(FileKind::Directory),
        ChildOpen::ExistingFile => Some(FileKind::File),
        ChildOpen::Probe => None,
    } && meta.kind != expect
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened object type changed before it was used",
        ));
    }
    Ok(OpenedChild { file, meta })
}

/// Creates a directory component or opens it when it already exists.
///
/// # Errors
///
/// Returns an I/O error when the name cannot be created or opened as a
/// directory without following a link.
pub(super) fn ensure_directory(parent: &File, name: &OsStr) -> io::Result<OpenedChild> {
    validate_component_name(name)?;
    match rfs::mkdirat(parent.as_fd(), name, Mode::from_raw_mode(DIR_CREATE_MODE)) {
        Ok(()) => {}
        Err(Errno::EXIST) => {}
        Err(err) => return Err(map_errno(err)),
    }
    open_child(parent, name, ChildOpen::Directory)
}

/// Unique on-disk directory-entry spelling for `want` inside `parent`.
///
/// # Errors
///
/// Zero or several matches fail closed so a case alias or same-directory
/// hardlink pair cannot keep an unproven name.
// NOTE: parallel implementation in fs_search (unix.rs
// `unix_on_disk_component_name`); kept separate because the two kernels use
// different platform APIs (rustix statat vs raw fdopendir/readdir) and
// independent budgets.
pub(super) fn unique_component_name(
    parent: &File,
    want: FileIdentity,
    cancel: &CancellationToken,
) -> io::Result<OsString> {
    let mut dir = rfs::Dir::read_from(parent.as_fd()).map_err(map_errno)?;
    let mut found: Option<OsString> = None;
    let mut scanned = 0usize;
    for entry in dir.by_ref() {
        check_cancel(cancel)?;
        let entry = entry.map_err(map_errno)?;
        let name = OsStr::from_bytes(entry.file_name().to_bytes());
        if name == "." || name == ".." {
            continue;
        }
        scanned += 1;
        if scanned > MAX_DIR_WIDTH {
            return Err(io::Error::other(
                "directory is too wide to prove unique on-disk component spelling",
            ));
        }
        let stat = match rfs::statat(parent.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            // A vanished sibling cannot currently name `want`.
            Err(Errno::NOENT) => continue,
            Err(err) => return Err(map_errno(err)),
        };
        let meta = meta_from_stat(&stat)?;
        if meta.identity == want {
            if found.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "multiple directory entries share the opened identity",
                ));
            }
            found = Some(OsString::from_vec(name.as_bytes().to_vec()));
        }
    }
    found.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "opened object has no unique on-disk file name",
        )
    })
}

fn create_temp_with<V, P>(
    parent: &File,
    name: &OsStr,
    validate: V,
    privatize: P,
) -> io::Result<OpenedChild>
where
    V: FnOnce(&File) -> io::Result<FileMeta>,
    P: FnOnce(&File) -> io::Result<()>,
{
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let descriptor = open_named(parent, name, flags, Mode::from_raw_mode(TEMP_PRIVATE_MODE))?;
    // Arm cleanup immediately after exclusive create, before any fallible
    // validation or mode transition can return the linked name to a caller.
    let created = CreatedName::new(parent, name);
    let file = into_file(descriptor);
    let result = (|| {
        let meta = validate(&file)?;
        if meta.kind != FileKind::File {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "temporary file is not a regular file",
            ));
        }
        privatize(&file)?;
        Ok(OpenedChild { file, meta })
    })();
    created.finish(result)
}

/// Creates a same-parent exclusive payload temp file.
///
/// The payload inode is private from the first instant it exists: it is
/// created with mode `0600` (no group/other bits, so no umask value can
/// widen it) and the explicit `fchmod` also drops group access a parent
/// default ACL might have granted at create time. Final modes are applied
/// to the published inode through this retained handle only after the
/// rename; see [`apply_new_file_mode`] and [`copy_safe_mode`]. A failed
/// post-create validation or mode transition unlinks the name before the
/// error is returned.
///
/// # Errors
///
/// Returns an I/O error when exclusive create, the mount check, the type
/// check, privacy `fchmod`, or mandatory cleanup fails.
pub(super) fn create_temp(parent: &File, name: &OsStr) -> io::Result<OpenedChild> {
    create_temp_with(
        parent,
        name,
        |file| {
            enforce_same_mount(parent, file)?;
            stat_meta(file)
        },
        |file| {
            // The create mode argument alone keeps group/other bits empty;
            // this also re-asserts privacy against a parent default ACL.
            rfs::fchmod(file.as_fd(), Mode::from_raw_mode(TEMP_PRIVATE_MODE)).map_err(map_errno)
        },
    )
}

fn create_mode_probe_with<S>(parent: &File, name: &OsStr, inspect: S) -> io::Result<u32>
where
    S: FnOnce(&File) -> io::Result<FileMeta>,
{
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let descriptor = open_named(parent, name, flags, Mode::from_raw_mode(TEMP_CREATE_MODE))?;
    // The caller owns the linked probe name only after stat succeeds.
    let created = CreatedName::new(parent, name);
    let file = into_file(descriptor);
    created.finish(inspect(&file).map(|meta| meta.unix_mode))
}

/// Creates a never-written mode probe next to the payload temp.
///
/// The probe is created with mode `0666` so the kernel applies the process
/// umask and any parent default ACL; the effective mode read back with
/// `fstat` is exactly what a plain `0666` create in this directory yields.
/// The probe never receives payload bytes and stays linked until the
/// caller's guard unlinks it, so the only inode ever exposed
/// group/other-readable is empty by construction. A failed post-create stat
/// removes the probe before returning.
///
/// # Errors
///
/// Returns an I/O error when exclusive create, stat, or mandatory cleanup
/// fails.
pub(super) fn create_mode_probe(parent: &File, name: &OsStr) -> io::Result<u32> {
    create_mode_probe_with(parent, name, stat_meta)
}

/// Writes `bytes` in chunks, then flushes and synchronizes `file`.
///
/// # Errors
///
/// Returns an I/O error on write, flush, sync, or cancellation.
pub(super) fn write_all_sync(
    file: &mut File,
    bytes: &[u8],
    cancel: &CancellationToken,
) -> io::Result<()> {
    let mut offset = 0usize;
    while offset < bytes.len() {
        check_cancel(cancel)?;
        let end = (offset + WRITE_CHUNK).min(bytes.len());
        file.write_all(&bytes[offset..end])?;
        offset = end;
    }
    file.flush()?;
    sync_file(file)
}

/// Reads `file` up to `declared_size` and fails if the stream disagrees.
///
/// # Errors
///
/// Returns an I/O error when the read is cancelled, exceeds `max_bytes`, or
/// does not match `declared_size`.
pub(super) fn read_exact_capped(
    file: &mut File,
    declared_size: u64,
    max_bytes: u64,
    cancel: &CancellationToken,
) -> io::Result<Vec<u8>> {
    if declared_size > max_bytes {
        return Err(io::Error::other("file exceeds the read size limit"));
    }
    let expected = usize::try_from(declared_size)
        .map_err(|_| io::Error::other("file exceeds the read size limit"))?;
    let mut out = Vec::new();
    let mut buf = [0u8; WRITE_CHUNK];
    loop {
        check_cancel(cancel)?;
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        let next = out.len().saturating_add(read);
        if next as u64 > declared_size || next as u64 > max_bytes {
            return Err(io::Error::other(
                "file grew or exceeded the read size limit",
            ));
        }
        out.extend_from_slice(&buf[..read]);
    }
    if out.len() != expected {
        return Err(io::Error::other(
            "file size changed during read or did not match metadata",
        ));
    }
    Ok(out)
}

pub(super) fn current_meta(file: &File) -> io::Result<FileMeta> {
    stat_meta(file)
}

/// Copies safe permission bits and owner from `src` onto `dst`.
///
/// Runs on the published inode through the retained temp handle after the
/// rename, so the payload is never exposed at the temp name with the
/// source's readable mode. `fchown` runs before `fchmod`. Either failure is
/// returned, so a widened owner or mode is never reported as success.
/// Setuid/setgid/sticky bits are applied only after that owner call.
///
/// # Errors
///
/// Returns an I/O error when mode or owner cannot be preserved.
pub(super) fn copy_safe_mode(src: &FileMeta, dst: &File) -> io::Result<()> {
    let uid = rfs::Uid::from_raw(src.unix_uid);
    let gid = rfs::Gid::from_raw(src.unix_gid);
    rfs::fchown(dst.as_fd(), Some(uid), Some(gid)).map_err(map_errno)?;
    // `RawMode` is u16 on macOS; the recorded value always originated as a
    // `RawMode`, so the narrowing conversion is lossless.
    let raw_mode = rfs::RawMode::try_from(src.unix_mode)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "recorded mode out of range"))?;
    rfs::fchmod(dst.as_fd(), Mode::from_raw_mode(raw_mode)).map_err(map_errno)?;
    Ok(())
}

/// Applies the probe-recorded creation mode to a newly published file.
///
/// `mode` is the permission-bits-only mode recorded from the never-written
/// `0666` probe create (umask/default ACL already applied by the kernel).
/// The payload is renamed into place while still private, and restoring the
/// recorded mode afterwards keeps published files consistent with the
/// session's umask without ever exposing the payload pre-publish.
///
/// # Errors
///
/// Returns an I/O error when `fchmod` fails.
pub(super) fn apply_new_file_mode(file: &File, mode: u32) -> io::Result<()> {
    // `unix_mode` is produced by `Mode::from_raw_mode`, which strips file
    // type bits, so the masked value always fits `RawMode` (u16 on some
    // Unix platforms) and the error arm is unreachable.
    let raw = rfs::RawMode::try_from(mode & 0o7777)
        .map_err(|_| io::Error::other("invalid recorded file mode"))?;
    rfs::fchmod(file.as_fd(), Mode::from_raw_mode(raw)).map_err(map_errno)
}

pub(super) fn unlink_child(parent: &File, name: &OsStr) -> io::Result<()> {
    validate_component_name(name)?;
    rfs::unlinkat(parent.as_fd(), name, AtFlags::empty()).map_err(map_errno)
}

/// Publishes `temp_name` over `dest_name` in `parent` (existing target).
///
/// # Errors
///
/// Returns an I/O error when `renameat` fails.
pub(super) fn publish_replace(
    parent: &File,
    _temp: &File,
    temp_name: &OsStr,
    dest_name: &OsStr,
) -> io::Result<()> {
    validate_component_name(temp_name)?;
    validate_component_name(dest_name)?;
    rfs::renameat(parent.as_fd(), temp_name, parent.as_fd(), dest_name).map_err(map_errno)
}

fn publish_link_unlink(parent: &File, temp_name: &OsStr, dest_name: &OsStr) -> io::Result<()> {
    rfs::linkat(
        parent.as_fd(),
        temp_name,
        parent.as_fd(),
        dest_name,
        AtFlags::empty(),
    )
    .map_err(map_errno)?;
    // The destination is already published, but a temp name that cannot be
    // removed is residue that must never be reported as success. The
    // caller's guard retries the unlink best-effort after this error.
    unlink_child(parent, temp_name).map_err(|cleanup| {
        io::Error::new(
            cleanup.kind(),
            format!("failed to remove the temporary name after publish: {cleanup}"),
        )
    })
}

/// Publishes `temp_name` as a new `dest_name` and fails if it exists.
///
/// Uses `renameatx_np(RENAME_EXCL)` via rustix `RenameFlags::NOREPLACE`,
/// falling back to `linkat`+`unlinkat` when the symbol is missing
/// (`Errno::NOSYS`). A temp-name removal that fails after the `linkat` is
/// returned as an error that includes the cleanup failure, never as success.
///
/// # Errors
///
/// Returns an I/O error when the destination already exists, the publish
/// syscalls fail, or the post-`linkat` temp-name cleanup fails.
pub(super) fn publish_create_only(
    parent: &File,
    _temp: &File,
    temp_name: &OsStr,
    dest_name: &OsStr,
) -> io::Result<()> {
    validate_component_name(temp_name)?;
    validate_component_name(dest_name)?;
    match rfs::renameat_with(
        parent.as_fd(),
        temp_name,
        parent.as_fd(),
        dest_name,
        rfs::RenameFlags::NOREPLACE,
    ) {
        Ok(()) => Ok(()),
        Err(Errno::NOSYS) => publish_link_unlink(parent, temp_name, dest_name),
        Err(err) => Err(map_errno(err)),
    }
}

/// Synchronizes `file` to stable storage.
///
/// On macOS this is `fcntl(F_FULLFSYNC)`. That call is required for durability
/// on APFS/HFS; a bare `fsync` only flushes to the drive cache. Failure is
/// returned rather than silently falling back, so callers must not claim
/// durability if this errors (some network volumes do not implement it).
///
/// # Errors
///
/// Returns an I/O error when the platform sync fails.
pub(super) fn sync_file(file: &File) -> io::Result<()> {
    rfs::fcntl_fullfsync(file.as_fd()).map_err(map_errno)
}

pub(super) fn sync_parent(dir: &File) -> io::Result<()> {
    sync_file(dir)
}
