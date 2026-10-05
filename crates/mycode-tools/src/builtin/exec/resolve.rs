//! Fail-closed executable resolution and identity pinning for structured exec.
//!
//! Basename lookup searches only absolute host `PATH` entries. A program that
//! contains a separator is resolved against the session cwd and must be
//! absolute after lexical normalization. Final symlink and reparse aliases are
//! followed; the regular target is opened and retained. Identity is the
//! canonical path, native file identity, and SHA-256 digest of that opened
//! target.
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;

use super::ResolveError;
use super::env::is_searchable_path_entry;
use super::image::{ImageKind, classify_image, read_pe_tail};
use super::spawn::{SpawnFailure, SpawnGate};
use crate::builtin::fs_search::lexical_normalize;
use crate::tool::ToolError;

/// Maximum UTF-8 bytes accepted for `program`.
const MAX_PROGRAM_BYTES: usize = 32_767;
/// Maximum arguments in one call.
const MAX_ARG_COUNT: usize = 4_096;
/// Maximum UTF-8 bytes accepted for one argument.
const MAX_ARG_BYTES: usize = 64 * 1024;
/// Maximum aggregate UTF-8 bytes accepted across all arguments.
///
/// One MiB bounds validation and platform-encoding allocations independently
/// of the target-specific command-line limit applied by the OS.
const MAX_TOTAL_ARG_BYTES: usize = 1024 * 1024;
/// Maximum image size hashed and launched.
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
/// Prefix read for magic-byte classification.
const IMAGE_HEADER_BYTES: usize = 4_096;

/// Retained executable object used as the launch anchor.
#[derive(Debug)]
pub(super) struct PinnedImage {
    /// Open file describing the same object that will be launched.
    pub file: File,
    /// Canonical path of the opened object.
    pub canonical_path: PathBuf,
    /// SHA-256 digest of the whole file at pin time.
    pub digest: [u8; 32],
    /// Native identity of the opened object.
    pub identity: FileIdentity,
    /// Classified image kind.
    pub kind: ImageKind,
}

/// Native file identity recorded from the retained handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FileIdentity {
    #[cfg(unix)]
    pub device: u64,
    #[cfg(unix)]
    pub inode: u64,
    #[cfg(windows)]
    pub volume: u64,
    #[cfg(windows)]
    pub file_id: [u8; 16],
}

impl FileIdentity {
    /// Renders identity as a short, non-path token for UI details.
    #[must_use]
    pub(super) fn debug_token(&self) -> String {
        #[cfg(unix)]
        {
            format!("dev:{:x} ino:{:x}", self.device, self.inode)
        }
        #[cfg(windows)]
        {
            format!("vol:{:x} id:{}", self.volume, encode_hex(&self.file_id))
        }
        #[cfg(not(any(unix, windows)))]
        {
            "unknown".into()
        }
    }
}

/// Hex-encodes `bytes` with lowercase ASCII.
#[must_use]
pub(super) fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

/// Resolves, opens, classifies, and hashes `program` against the session cwd.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArgs`] when the program cannot be resolved
/// fail-closed and [`ToolError::Execution`] when the call is cancelled.
/// Resolves `program` against an already-snapshotted PATH value.
///
/// # Errors
///
/// Same as [`pin_program`].
pub(super) fn pin_program_with_path(
    session_cwd: &Path,
    program: &str,
    args: &[String],
    path_var: Option<&std::ffi::OsStr>,
    cancel: &CancellationToken,
) -> Result<PinnedImage, ResolveError> {
    check_cancelled(cancel)?;
    validate_request(program, args)?;
    require_directory(session_cwd)?;
    check_cancelled(cancel)?;
    if is_path_program(program) {
        let candidate = resolve_path_program(program, session_cwd).map_err(ResolveError::Other)?;
        check_cancelled(cancel)?;
        return pin_candidate(&candidate, cancel);
    }
    pin_basename(program, path_var, cancel)
}

/// PATH search that skips unopenable hits (Windows Store execution aliases
/// return ERROR_CANT_ACCESS_FILE / empty images) and continues to the next
/// directory instead of failing closed on the first name match.
fn pin_basename(
    name: &str,
    path_var: Option<&std::ffi::OsStr>,
    cancel: &CancellationToken,
) -> Result<PinnedImage, ResolveError> {
    let path_var = path_var.unwrap_or_default();
    let mut searched = 0usize;
    for entry in std::env::split_paths(path_var) {
        if !is_searchable_path_entry(&entry) {
            continue;
        }
        searched += 1;
        check_cancelled(cancel)?;
        let Some(found) = candidate_in_dir(&entry, name)? else {
            continue;
        };
        match pin_candidate(&found, cancel) {
            Ok(pinned) => return Ok(pinned),
            Err(error) if is_skippable_path_hit(&error) => continue,
            Err(error) => return Err(error),
        }
    }
    Err(ResolveError::NotFound {
        program: name.to_owned(),
        searched: Some(searched),
    })
}

fn is_skippable_path_hit(error: &ResolveError) -> bool {
    if error.is_not_found() {
        return true;
    }
    let text = error.to_string();
    text.contains("program is empty")
        || text.contains("os error 1920")
        || text.contains("系统无法访问此文件")
}

/// Validates program and argument resource limits before request cloning.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArgs`] for empty or oversized input and
/// interior NUL bytes.
pub(super) fn validate_request(program: &str, args: &[String]) -> Result<(), ToolError> {
    reject_nul(program, "program")?;
    if program.is_empty() {
        return Err(ToolError::InvalidArgs("program must not be empty".into()));
    }
    if program.len() > MAX_PROGRAM_BYTES {
        return Err(ToolError::InvalidArgs(
            "program is longer than 32,767 bytes".into(),
        ));
    }
    if args.len() > MAX_ARG_COUNT {
        return Err(ToolError::InvalidArgs(format!(
            "argument list exceeds {MAX_ARG_COUNT} entries"
        )));
    }

    let mut total_bytes = 0_usize;
    for arg in args {
        reject_nul(arg, "argument")?;
        if arg.len() > MAX_ARG_BYTES {
            return Err(ToolError::InvalidArgs(format!(
                "argument exceeds {MAX_ARG_BYTES} bytes"
            )));
        }
        total_bytes = total_bytes
            .checked_add(arg.len())
            .ok_or_else(|| ToolError::InvalidArgs("aggregate argument length overflowed".into()))?;
        if total_bytes > MAX_TOTAL_ARG_BYTES {
            return Err(ToolError::InvalidArgs(format!(
                "argument data exceeds {MAX_TOTAL_ARG_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

/// Returns the native identity of an already-opened file.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArgs`] when the handle is not a regular file.
#[cfg(windows)]
pub(super) fn identity_of(file: &File) -> Result<FileIdentity, ToolError> {
    windows_file_identity(file)
}

/// Re-hashes the retained file while polling `check_cancelled` between reads.
///
/// # Errors
///
/// Returns the first cancellation or file I/O error.
pub(super) fn rehash_image_cancellable<F>(
    file: &mut File,
    mut check_cancelled: F,
) -> Result<[u8; 32], ToolError>
where
    F: FnMut() -> Result<(), ToolError>,
{
    file.seek(SeekFrom::Start(0)).map_err(|err| {
        ToolError::Execution(format!("failed to rewind the pinned executable: {err}"))
    })?;
    hash_file_cancellable(file, &mut check_cancelled)
}

/// Re-hashes the pinned image and rejects a pre-launch digest change.
///
/// Shared prelude of every platform spawn: the digest pinned during
/// resolution must still match immediately before launch, and the gate must
/// stay pending-free across the rehash.
pub(super) fn verify_pinned_digest(
    pinned: &mut PinnedImage,
    gate: &SpawnGate,
) -> Result<(), SpawnFailure> {
    let digest = rehash_image_cancellable(&mut pinned.file, || gate.check_pending())?;
    if digest != pinned.digest {
        return Err(ToolError::Execution(
            "pinned executable digest changed before launch \
             (a same-account writer rewrote the file; this is outside the security boundary)"
                .into(),
        )
        .into());
    }
    gate.check_pending()?;
    Ok(())
}

pub(super) fn check_cancelled(cancel: &CancellationToken) -> Result<(), ToolError> {
    if cancel.is_cancelled() {
        Err(ToolError::Execution(
            "command cancelled before completion".into(),
        ))
    } else {
        Ok(())
    }
}

pub(super) fn reject_nul(value: &str, what: &str) -> Result<(), ToolError> {
    if value.contains('\0') {
        Err(ToolError::InvalidArgs(format!(
            "{what} contains an interior NUL"
        )))
    } else {
        Ok(())
    }
}

fn require_directory(path: &Path) -> Result<(), ToolError> {
    let metadata = std::fs::metadata(path).map_err(|err| {
        ToolError::InvalidArgs(format!("working directory . is unavailable: {err}"))
    })?;
    if metadata.is_dir() {
        Ok(())
    } else {
        Err(ToolError::InvalidArgs(
            "working directory . is not a directory".into(),
        ))
    }
}

fn is_path_program(program: &str) -> bool {
    if program.contains('/') {
        return true;
    }
    #[cfg(windows)]
    {
        // Only Windows treats '\' as a path separator. On Unix it is a legal
        // basename character and must search PATH, never the session cwd.
        if program.contains('\\') {
            return true;
        }
        matches!(
            Path::new(program).components().next(),
            Some(std::path::Component::Prefix(_))
        )
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn resolve_path_program(program: &str, session_cwd: &Path) -> Result<PathBuf, ToolError> {
    let path = Path::new(program);
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if let Some(Component::Prefix(prefix)) = path.components().next()
            && matches!(prefix.kind(), Prefix::Disk(_))
            && !path.has_root()
        {
            return Err(ToolError::InvalidArgs(
                "program path must be absolute after resolving against the session cwd".into(),
            ));
        }
    }
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        session_cwd.join(path)
    };
    let normalized = lexical_normalize(&joined);
    if !normalized.is_absolute() {
        return Err(ToolError::InvalidArgs(
            "program path must be absolute after resolving against the session cwd".into(),
        ));
    }
    Ok(normalized)
}

fn candidate_in_dir(dir: &Path, name: &str) -> Result<Option<PathBuf>, ResolveError> {
    let exact = dir.join(name);
    if is_present_file(&exact)? {
        return Ok(Some(exact));
    }
    #[cfg(windows)]
    {
        if !has_exe_suffix(name) {
            let with_exe = dir.join(format!("{name}.exe"));
            if is_present_file(&with_exe)? {
                return Ok(Some(with_exe));
            }
        }
    }
    Ok(None)
}

#[cfg(windows)]
fn has_exe_suffix(name: &str) -> bool {
    name.len() >= 4 && name.as_bytes()[name.len() - 4..].eq_ignore_ascii_case(b".exe")
}

fn is_present_file(path: &Path) -> Result<bool, ResolveError> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(ResolveError::Other(ToolError::InvalidArgs(
            "program candidate is not a regular file".into(),
        ))),
        Err(error) => candidate_metadata_error(error),
    }
}

fn candidate_metadata_error(error: std::io::Error) -> Result<bool, ResolveError> {
    if error.kind() == std::io::ErrorKind::NotFound {
        Ok(false)
    } else {
        Err(ResolveError::Other(ToolError::InvalidArgs(format!(
            "program candidate could not be inspected: {error}"
        ))))
    }
}

fn pin_candidate(path: &Path, cancel: &CancellationToken) -> Result<PinnedImage, ResolveError> {
    let mut file = open_executable(path)?;
    let identity = file_identity(&file)?;
    let canonical_path = canonical_from_handle(&file, path)?;
    let unicode = canonical_path.to_str().ok_or_else(|| {
        ToolError::InvalidArgs(
            "canonical program path is not valid Unicode and cannot be recorded".into(),
        )
    })?;
    if unicode.contains('\0') {
        return Err(ResolveError::Other(ToolError::InvalidArgs(
            "canonical program path contains an interior NUL".into(),
        )));
    }
    let header = read_header(&mut file)?;
    let pe_tail = read_pe_tail(&mut file, &header)?;
    let kind = classify_image(&header, pe_tail.as_ref())?;
    #[cfg(windows)]
    if kind != ImageKind::Pe {
        return Err(ResolveError::Other(ToolError::InvalidArgs(
            "program is not a kernel-loadable PE image".into(),
        )));
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if kind != ImageKind::Elf {
        return Err(ResolveError::Other(ToolError::InvalidArgs(
            "program is not a kernel-loadable ELF image".into(),
        )));
    }
    #[cfg(target_os = "macos")]
    if !matches!(kind, ImageKind::MachO { .. }) {
        return Err(ResolveError::Other(ToolError::InvalidArgs(
            "program is not a kernel-loadable Mach-O image".into(),
        )));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|err| ToolError::InvalidArgs(format!("program could not be rewound: {err}")))?;
    let digest = hash_file_cancellable(&mut file, &mut || check_cancelled(cancel))?;
    Ok(PinnedImage {
        file,
        canonical_path,
        digest,
        identity,
        kind,
    })
}

fn read_header(file: &mut File) -> Result<Vec<u8>, ToolError> {
    let mut header = vec![0_u8; IMAGE_HEADER_BYTES];
    let read = file
        .read(&mut header)
        .map_err(|err| ToolError::InvalidArgs(format!("program could not be read: {err}")))?;
    header.truncate(read);
    if header.is_empty() {
        return Err(ToolError::InvalidArgs("program is empty".into()));
    }
    Ok(header)
}

fn hash_file_cancellable<F>(file: &mut File, check_cancelled: &mut F) -> Result<[u8; 32], ToolError>
where
    F: FnMut() -> Result<(), ToolError>,
{
    let metadata = file
        .metadata()
        .map_err(|err| ToolError::InvalidArgs(format!("program metadata is unavailable: {err}")))?;
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "program exceeds the {MAX_IMAGE_BYTES} byte image limit"
        )));
    }
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        check_cancelled()?;
        let count = file
            .read(&mut buf)
            .map_err(|err| ToolError::InvalidArgs(format!("program could not be hashed: {err}")))?;
        if count == 0 {
            break;
        }
        total = total.checked_add(count as u64).ok_or_else(|| {
            ToolError::InvalidArgs("program size overflowed while hashing".into())
        })?;
        if total > MAX_IMAGE_BYTES {
            return Err(ToolError::InvalidArgs(format!(
                "program exceeds the {MAX_IMAGE_BYTES} byte image limit"
            )));
        }
        hasher.update(&buf[..count]);
    }
    Ok(hasher.finalize().into())
}

#[cfg(unix)]
fn open_executable(path: &Path) -> Result<File, ResolveError> {
    use rustix::fs::{Mode, OFlags, open};
    use std::os::fd::FromRawFd as _;
    use std::os::fd::IntoRawFd as _;

    let fd = match open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(err) if err == rustix::io::Errno::NOENT => {
            return Err(ResolveError::path_not_found(path));
        }
        Err(err) => {
            return Err(ResolveError::Other(ToolError::InvalidArgs(format!(
                "program could not be opened: {err}"
            ))));
        }
    };
    // SAFETY: `fd` is a freshly opened owned descriptor transferred into `File`.
    Ok(unsafe { File::from_raw_fd(fd.into_raw_fd()) })
}

#[cfg(windows)]
fn open_executable(path: &Path) -> Result<File, ResolveError> {
    windows_open_pin(path)
}

#[cfg(not(any(unix, windows)))]
fn open_executable(path: &Path) -> Result<File, ResolveError> {
    File::open(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ResolveError::path_not_found(path)
        } else {
            ResolveError::Other(ToolError::InvalidArgs(format!(
                "program could not be opened: {err}"
            )))
        }
    })
}

#[cfg(unix)]
fn file_identity(file: &File) -> Result<FileIdentity, ToolError> {
    use rustix::fd::AsFd as _;
    use rustix::fs::{FileType, fstat};

    let stat = fstat(file.as_fd())
        .map_err(|err| ToolError::InvalidArgs(format!("program identity is unavailable: {err}")))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(ToolError::InvalidArgs(
            "program is not a regular file".into(),
        ));
    }
    let device = crate::builtin::fs_search::unix_device_identity(stat.st_dev).map_err(|err| {
        ToolError::InvalidArgs(format!("program device identity is unavailable: {err}"))
    })?;
    let inode = crate::builtin::fs_search::unix_inode_identity(stat.st_ino).map_err(|err| {
        ToolError::InvalidArgs(format!("program inode identity is unavailable: {err}"))
    })?;
    Ok(FileIdentity { device, inode })
}

#[cfg(windows)]
fn file_identity(file: &File) -> Result<FileIdentity, ToolError> {
    windows_file_identity(file)
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File) -> Result<FileIdentity, ToolError> {
    Err(ToolError::Execution(
        "direct program launch is not supported on this platform".into(),
    ))
}

#[cfg(unix)]
fn canonical_from_handle(file: &File, _request: &Path) -> Result<PathBuf, ToolError> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd as _;
        let fd = file.as_raw_fd();
        let link = std::fs::read_link(format!("/proc/self/fd/{fd}")).map_err(|err| {
            ToolError::InvalidArgs(format!("program canonical path is unavailable: {err}"))
        })?;
        if !link.is_absolute() {
            return Err(ToolError::InvalidArgs(
                "program canonical path is not absolute".into(),
            ));
        }
        Ok(link)
    }
    #[cfg(target_os = "macos")]
    {
        use rustix::fd::AsFd as _;
        use std::os::unix::ffi::OsStringExt as _;

        let cstr = rustix::fs::getpath(file.as_fd()).map_err(|err| {
            ToolError::InvalidArgs(format!("program canonical path is unavailable: {err}"))
        })?;
        let path = PathBuf::from(std::ffi::OsString::from_vec(cstr.to_bytes().to_vec()));
        if !path.is_absolute() {
            return Err(ToolError::InvalidArgs(
                "program canonical path is not absolute".into(),
            ));
        }
        Ok(path)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = file;
        Err(ToolError::Execution(
            "direct program launch is not supported on this platform".into(),
        ))
    }
}

#[cfg(windows)]
fn canonical_from_handle(file: &File, _request: &Path) -> Result<PathBuf, ToolError> {
    windows_final_path(file)
}

#[cfg(not(any(unix, windows)))]
fn canonical_from_handle(_file: &File, _request: &Path) -> Result<PathBuf, ToolError> {
    Err(ToolError::Execution(
        "direct program launch is not supported on this platform".into(),
    ))
}

#[cfg(windows)]
fn windows_open_pin(path: &Path) -> Result<File, ResolveError> {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, OPEN_EXISTING};

    let extended = windows_extended_length_path(path);
    let mut wide: Vec<u16> = extended.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(ResolveError::Other(ToolError::InvalidArgs(
            "program path contains an interior NUL".into(),
        )));
    }
    wide.push(0);
    // SAFETY: `wide` is a NUL-terminated UTF-16 path. CreateFileW follows
    // the final reparse alias onto the regular target. FILE_SHARE_READ (no
    // WRITE/DELETE) pins that object. A non-null, non-INVALID handle is
    // uniquely owned.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        let err = std::io::Error::last_os_error();
        // ERROR_CANT_ACCESS_FILE (1920): Store execution aliases look like
        // files on PATH but cannot be opened or hashed as a regular image.
        return Err(
            if err.kind() == std::io::ErrorKind::NotFound || err.raw_os_error() == Some(1920) {
                ResolveError::path_not_found(path)
            } else {
                ResolveError::Other(ToolError::InvalidArgs(format!(
                    "program could not be opened: {err}"
                )))
            },
        );
    }
    // SAFETY: CreateFileW returned a fresh owned HANDLE.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    Ok(File::from(handle))
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> Result<FileIdentity, ToolError> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ID_INFO, FileIdInfo,
        GetFileInformationByHandle, GetFileInformationByHandleEx,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` is live; `information` is writable documented storage.
    let success = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut information) };
    if success == 0 {
        return Err(ToolError::InvalidArgs(format!(
            "program identity is unavailable: {}",
            std::io::Error::last_os_error()
        )));
    }
    if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err(ToolError::InvalidArgs(
            "program is a directory, not an executable".into(),
        ));
    }
    let mut id_info = FILE_ID_INFO::default();
    let id_size = u32::try_from(size_of::<FILE_ID_INFO>()).expect("FILE_ID_INFO fits in u32");
    // SAFETY: `id_info` is writable FILE_ID_INFO storage. ReFS uniqueness
    // requires the 128-bit FileId.
    let id_ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&raw mut id_info).cast(),
            id_size,
        )
    };
    if id_ok == 0 {
        return Err(ToolError::InvalidArgs(format!(
            "program file id is unavailable: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(FileIdentity {
        volume: id_info.VolumeSerialNumber,
        file_id: id_info.FileId.Identifier,
    })
}

#[cfg(windows)]
fn windows_final_path(file: &File) -> Result<PathBuf, ToolError> {
    use std::os::windows::ffi::OsStringExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW,
    };

    let mut buf = vec![0_u16; 512];
    loop {
        // SAFETY: `file` is live; `buf` is writable UTF-16 storage whose
        // documented length is `buf.len()`. A return of 0 is failure.
        let needed = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buf.as_mut_ptr(),
                u32::try_from(buf.len()).unwrap_or(u32::MAX),
                FILE_NAME_NORMALIZED,
            )
        };
        if needed == 0 {
            return Err(ToolError::InvalidArgs(format!(
                "program canonical path is unavailable: {}",
                std::io::Error::last_os_error()
            )));
        }
        let needed_usize = needed as usize;
        if needed_usize >= buf.len() {
            buf.resize(needed_usize.saturating_add(1), 0);
            continue;
        }
        buf.truncate(needed_usize);
        let path = PathBuf::from(std::ffi::OsString::from_wide(&buf));
        if !path.is_absolute() {
            return Err(ToolError::InvalidArgs(
                "program canonical path is not absolute".into(),
            ));
        }
        return Ok(path);
    }
}

#[cfg(windows)]
fn windows_extended_length_path(path: &Path) -> PathBuf {
    use std::path::{Component, Prefix};
    if !path.is_absolute() {
        return path.to_path_buf();
    }
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path.to_path_buf();
    };
    match prefix.kind() {
        Prefix::Disk(_) => {
            let mut extended = std::ffi::OsString::from(r"\\?\");
            extended.push(path.as_os_str());
            PathBuf::from(extended)
        }
        Prefix::VerbatimDisk(_) | Prefix::VerbatimUNC(_, _) | Prefix::Verbatim(_) => {
            path.to_path_buf()
        }
        Prefix::UNC(server, share) => {
            let mut authority = std::ffi::OsString::from(r"\\?\UNC\");
            authority.push(server);
            authority.push(r"\");
            authority.push(share);
            let mut extended = PathBuf::from(authority);
            for component in components {
                if !matches!(component, Component::RootDir) {
                    extended.push(component.as_os_str());
                }
            }
            extended
        }
        _ => path.to_path_buf(),
    }
}
