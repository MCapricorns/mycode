//! Unix handle-relative opens, identity, and unique on-disk spelling proofs.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use tokio_util::sync::CancellationToken;

use super::*;

#[cfg(target_os = "macos")]
pub(crate) fn unix_device_identity(value: libc::dev_t) -> io::Result<u64> {
    // Darwin dev_t is signed. Match MetadataExt::dev's bit-preserving cast
    // rather than rejecting valid high-bit device identifiers.
    Ok(value as u64)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn unix_device_identity(value: libc::dev_t) -> io::Result<u64> {
    unix_identity_part(value, "device")
}

#[cfg(target_os = "macos")]
pub(crate) fn unix_inode_identity(value: u64) -> io::Result<u64> {
    // Darwin ino_t is already u64. Keep the kernel identity bits unchanged.
    Ok(value)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn unix_inode_identity<T>(value: T) -> io::Result<u64>
where
    T: TryInto<u64>,
{
    unix_identity_part(value, "inode")
}

#[cfg(unix)]
pub(crate) fn unix_identity_part<T>(value: T, label: &str) -> io::Result<u64>
where
    T: TryInto<u64>,
{
    value.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid {label} identity"),
        )
    })
}

/// Reopens `parent` as `"."` so listing does not share the parent's offset.
///
/// POSIX `fdopendir` starts at the current offset of the given descriptor.
/// `File::try_clone` duplicates the descriptor but not the open-file
/// description, so a cloned listing would resume after a previous scan's
/// EOF and miss every entry. Opening `"."` creates a distinct description.
///
/// CONTRACT: `fs_walk::DirListing` follows the same reopen-"." approach for
/// its Unix listings; do not switch either site back to `try_clone`.
#[cfg(unix)]
pub(crate) fn unix_reopen_dir_for_listing(parent: &File) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let c_dot = c".";
    let flags =
        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY;
    let descriptor = open_unix_descriptor(
        parent.as_raw_fd(),
        c_dot.as_ptr(),
        flags,
        SearchAccess::Content,
    )?;
    // SAFETY: `descriptor` is a fresh owned fd from `openat`/`openat2`.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

/// Unique directory-entry spelling in `parent` whose identity equals `want`.
// NOTE: parallel implementation in fs_io (unix.rs/windows.rs
// `unique_component_name`); kept separate because fs_search proves the
// spelling through raw fdopendir/readdir with its own entry budget, while
// fs_io walks rustix/NT listings under the file-kernel limiter.
#[cfg(unix)]
pub(crate) fn unix_on_disk_component_name(
    parent: &File,
    want: FileIdentity,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<OsString> {
    use std::ffi::{CStr, CString};
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, IntoRawFd};
    use std::os::unix::ffi::OsStrExt;

    struct DirOwner(*mut libc::DIR);
    impl Drop for DirOwner {
        fn drop(&mut self) {
            // SAFETY: `fdopendir` transferred exclusive ownership of this `DIR*`.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }

    let listing = unix_reopen_dir_for_listing(parent)?;
    let fd = listing.into_raw_fd();
    // SAFETY: `fd` is exclusively owned. `fdopendir` takes it or we close it.
    let dirp = unsafe { libc::fdopendir(fd) };
    if dirp.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so this process still owns `fd`.
        unsafe {
            libc::close(fd);
        }
        return Err(error);
    }
    let owner = DirOwner(dirp);
    let mut found: Option<OsString> = None;
    let mut scanned = 0usize;
    loop {
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "search stopped"));
        }
        if !limiter.try_reserve_entry() {
            return Err(io::Error::other("walk entry limit reached"));
        }
        unix_clear_errno();
        // SAFETY: `owner.0` is a live `DIR*`. A non-null `dirent` is valid
        // until the next `readdir`/`closedir` on this stream.
        let entry = unsafe { libc::readdir(owner.0) };
        if entry.is_null() {
            limiter.release_entry();
            let error = io::Error::last_os_error();
            if error.raw_os_error().unwrap_or(0) == 0 {
                break;
            }
            return Err(error);
        }
        // SAFETY: `d_name` is a NUL-terminated component from `readdir`.
        let c_name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let name = OsStr::from_bytes(c_name.to_bytes());
        if name == "." || name == ".." {
            limiter.release_entry();
            continue;
        }
        scanned += 1;
        if scanned > MAX_DIR_WIDTH {
            limiter.release_entry();
            return Err(io::Error::other(
                "directory is too wide to prove unique on-disk component spelling",
            ));
        }
        let c_owned = match CString::new(name.as_bytes()) {
            Ok(c_owned) => c_owned,
            Err(_) => {
                limiter.release_entry();
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path component contains NUL",
                ));
            }
        };
        let mut stat = MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `parent` is a live directory fd, `c_owned` is a single
        // component, and `AT_SYMLINK_NOFOLLOW` inspects the named child.
        let status = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                c_owned.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if status != 0 {
            let error = io::Error::last_os_error();
            // A vanished sibling cannot currently name `want`. Failing the
            // whole scan would treat a busy case-sensitive parent (shared
            // temp directories) as identity ambiguity. Live hardlink and
            // case-alias duplicates are still observed below.
            if error.kind() == io::ErrorKind::NotFound {
                continue;
            }
            limiter.release_entry();
            return Err(io::Error::other(
                "cannot prove unique on-disk component spelling",
            ));
        }
        // SAFETY: `fstatat` returned 0 and initialized `stat`.
        let stat = unsafe { stat.assume_init() };
        let device = unix_device_identity(stat.st_dev)?;
        let inode = unix_identity_part(stat.st_ino, "inode")?;
        let identity = FileIdentity { device, inode };
        if identity == want {
            if found.is_some() {
                limiter.release_entry();
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "multiple directory entries share the opened identity",
                ));
            }
            found = Some(name.to_os_string());
        }
    }
    found.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "opened object has no unique on-disk file name",
        )
    })
}

/// Clears thread-local errno before `readdir` so EOF is not a stale error.
///
/// Symbols match `libc` 0.2.189. Unknown Unix errno ABIs fail at compile
/// time rather than treating a leftover errno as a listing failure.
#[cfg(unix)]
pub(crate) fn unix_clear_errno() {
    // SAFETY: writing 0 into thread-local errno distinguishes `readdir`
    // EOF from a real failure after a previous fallible syscall.
    #[cfg(any(
        target_os = "linux",
        target_os = "emscripten",
        target_os = "fuchsia",
        target_os = "hurd",
        target_os = "l4re",
        target_os = "redox",
        target_os = "dragonfly"
    ))]
    unsafe {
        *libc::__errno_location() = 0;
    }
    #[cfg(any(target_os = "android", target_os = "openbsd", target_os = "netbsd"))]
    unsafe {
        *libc::__errno() = 0;
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "visionos",
        target_os = "freebsd"
    ))]
    unsafe {
        *libc::__error() = 0;
    }
    #[cfg(any(target_os = "solaris", target_os = "illumos"))]
    unsafe {
        *libc::___errno() = 0;
    }
    #[cfg(target_os = "aix")]
    unsafe {
        *libc::_Errno() = 0;
    }
    #[cfg(target_os = "haiku")]
    unsafe {
        *libc::_errnop() = 0;
    }
    #[cfg(target_os = "nto")]
    unsafe {
        *libc::__get_errno_ptr() = 0;
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "emscripten",
        target_os = "fuchsia",
        target_os = "hurd",
        target_os = "l4re",
        target_os = "redox",
        target_os = "dragonfly",
        target_os = "android",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "visionos",
        target_os = "freebsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "haiku",
        target_os = "nto"
    )))]
    {
        compile_error!("readdir errno ABI is not implemented for this Unix target");
    }
}

#[cfg(unix)]
pub(crate) fn stable_from_file(file: File) -> io::Result<StableHandle> {
    let (identity, kind) = identity_and_kind(&file)?;
    Ok(StableHandle {
        file,
        identity,
        kind,
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
        named_child: None,
    })
}

#[cfg(unix)]
pub(crate) fn identity_and_kind(file: &File) -> io::Result<(FileIdentity, FsEntryKind)> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata()?;
    let kind = if metadata.is_file() {
        FsEntryKind::File
    } else if metadata.is_dir() {
        FsEntryKind::Directory
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened object is neither a regular file nor a directory",
        ));
    };
    Ok((
        FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        kind,
    ))
}

#[cfg(unix)]
pub(crate) fn open_named_unix(
    parent: &File,
    name: &OsStr,
    expected: Option<FsEntryKind>,
    access: SearchAccess,
) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    validate_component_name(name)?;
    let c_name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path component contains NUL"))?;
    let mut flags = libc::O_CLOEXEC | libc::O_NOFOLLOW;
    match access {
        SearchAccess::Content => flags |= libc::O_RDONLY | libc::O_NONBLOCK,
        SearchAccess::Metadata => {
            #[cfg(any(target_os = "linux", target_os = "android"))]
            {
                flags |= libc::O_PATH;
            }
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            {
                let _ = flags;
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "metadata-only child fds require O_PATH",
                ));
            }
        }
    }
    if matches!(expected, Some(FsEntryKind::Directory)) {
        flags |= libc::O_DIRECTORY;
    }
    let descriptor = open_unix_descriptor(parent.as_raw_fd(), c_name.as_ptr(), flags, access)?;
    // SAFETY: the descriptor is a fresh owned fd from `openat`/`openat2`.
    let file = unsafe { File::from_raw_fd(descriptor) };
    enforce_unix_child_containment(parent, &file, expected)?;
    Ok(file)
}

/// Reads one named Unix child's identity without following its final link.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
pub(crate) fn unix_named_identity(
    parent: &File,
    name: &OsStr,
) -> io::Result<(FileIdentity, FsEntryKind)> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    validate_component_name(name)?;
    let c_name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path component contains NUL"))?;
    let mut stat = MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `parent` is a live directory fd, `c_name` is a NUL-terminated
    // component, and `stat` is writable. The final link is not followed.
    let status = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            c_name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        return Err(map_unix_open_error(io::Error::last_os_error()));
    }
    // SAFETY: successful `fstatat` initialized `stat`.
    let stat = unsafe { stat.assume_init() };
    let fmt = stat.st_mode & libc::S_IFMT;
    if fmt == libc::S_IFLNK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "symlink traversal is not permitted",
        ));
    }
    let parent_meta = parent.metadata()?;
    let child_dev = stat.st_dev as u64;
    if parent_meta.dev() != child_dev {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount traversal is not permitted",
        ));
    }
    let kind = if fmt == libc::S_IFREG {
        FsEntryKind::File
    } else if fmt == libc::S_IFDIR {
        FsEntryKind::Directory
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened object is neither a regular file nor a directory",
        ));
    };
    if kind == FsEntryKind::File && stat.st_nlink as u64 != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "multi-link regular files are not permitted",
        ));
    }
    let device = unix_device_identity(stat.st_dev)?;
    let inode = unix_identity_part(stat.st_ino, "inode")?;
    Ok((FileIdentity { device, inode }, kind))
}

/// Confirms `name` with no-follow metadata and mount/type/`nlink` proof.
///
/// Used on Unix platforms without `O_PATH`/`openat2`. Lookup does not open
/// the child for reading; inability to prove containment fails closed.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
pub(crate) fn confirm_named_unix_metadata(
    parent: &File,
    name: &OsStr,
    expected: Option<FsEntryKind>,
) -> io::Result<()> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    validate_component_name(name)?;
    let c_name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path component contains NUL"))?;
    let mut stat = MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `parent` is a live directory fd, `c_name` is a NUL-terminated
    // single component, and `stat` is writable `stat` storage. `AT_SYMLINK_NOFOLLOW`
    // prevents following the last component.
    let status = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            c_name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        return Err(map_unix_open_error(io::Error::last_os_error()));
    }
    // SAFETY: `fstatat` returned 0 and initialized `stat`.
    let stat = unsafe { stat.assume_init() };
    let fmt = stat.st_mode & libc::S_IFMT;
    if fmt == libc::S_IFLNK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "symlink traversal is not permitted",
        ));
    }
    let parent_meta = parent.metadata()?;
    let child_dev = stat.st_dev as u64;
    if parent_meta.dev() != child_dev {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount traversal is not permitted",
        ));
    }
    let kind = if fmt == libc::S_IFREG {
        FsEntryKind::File
    } else if fmt == libc::S_IFDIR {
        FsEntryKind::Directory
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened object is neither a regular file nor a directory",
        ));
    };
    if let Some(expected) = expected
        && kind != expected
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "walked object type or identity changed before it was opened",
        ));
    }
    if kind == FsEntryKind::File && stat.st_nlink as u64 != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "multi-link regular files are not permitted",
        ));
    }
    Ok(())
}

/// Opens `name` relative to `dirfd` without following links.
///
/// Linux uses `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_XDEV |
/// RESOLVE_NO_SYMLINKS`. Kernels without `openat2` fail closed rather than
/// falling back to mount-crossing `openat`. Other Unix uses `openat` and
/// relies on the post-open `st_dev` / `st_nlink` check.
#[cfg(unix)]
fn open_unix_descriptor(
    dirfd: std::os::fd::RawFd,
    c_name: *const libc::c_char,
    flags: libc::c_int,
    access: SearchAccess,
) -> io::Result<std::os::fd::RawFd> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let descriptor = openat2_beneath(dirfd, c_name, flags, access)?;
        Ok(descriptor)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = access;
        // SAFETY: `dirfd` is a live directory fd, `c_name` is NUL-terminated,
        // and flags request a new owned descriptor only.
        let descriptor = unsafe { libc::openat(dirfd, c_name, flags) };
        if descriptor == -1 {
            return Err(map_unix_open_error(io::Error::last_os_error()));
        }
        Ok(descriptor)
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

/// Required Linux/Android `openat2` containment policy.
#[cfg(any(target_os = "linux", target_os = "android"))]
const OPENAT2_REQUIRED_RESOLVE: u64 = 0x01 | 0x04 | 0x08;

#[cfg(any(target_os = "linux", target_os = "android"))]
fn openat2_beneath(
    dirfd: std::os::fd::RawFd,
    c_name: *const libc::c_char,
    flags: libc::c_int,
    access: SearchAccess,
) -> io::Result<std::os::fd::RawFd> {
    let how = OpenHow {
        flags: flags as u64,
        mode: 0,
        resolve: OPENAT2_REQUIRED_RESOLVE,
    };
    let _ = access;
    // SAFETY: `dirfd` is a live directory fd, `c_name` is a NUL-terminated
    // component, and `how` is the documented 24-byte `open_how` layout.
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            c_name,
            std::ptr::addr_of!(how),
            std::mem::size_of::<OpenHow>(),
        )
    };
    if descriptor < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOSYS) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "openat2 is required to prove NO_XDEV/BENEATH containment",
            ));
        }
        return Err(map_unix_open_error(error));
    }
    Ok(descriptor as std::os::fd::RawFd)
}

#[cfg(unix)]
fn map_unix_open_error(error: io::Error) -> io::Error {
    match error.raw_os_error() {
        Some(libc::ELOOP) => io::Error::new(
            io::ErrorKind::InvalidInput,
            "symlink traversal is not permitted",
        ),
        Some(libc::EXDEV) => io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount traversal is not permitted",
        ),
        _ => error,
    }
}

#[cfg(unix)]
fn enforce_unix_child_containment(
    parent: &File,
    child: &File,
    expected: Option<FsEntryKind>,
) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let parent_meta = parent.metadata()?;
    let child_meta = child.metadata()?;
    let child_dev = child_meta.dev();
    if parent_meta.dev() != child_dev {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount traversal is not permitted",
        ));
    }
    let (_, kind) = identity_and_kind(child)?;
    if let Some(expected) = expected
        && kind != expected
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "walked object type or identity changed before it was opened",
        ));
    }
    if kind == FsEntryKind::File && child_meta.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "multi-link regular files are not permitted",
        ));
    }
    Ok(())
}
