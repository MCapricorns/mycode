//! Windows native private regular-file transactions.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DISPOSITION_INFORMATION, FILE_RENAME_INFORMATION, FileDispositionInformation,
    FileRenameInformation, NtSetInformationFile,
};
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, FlushFileBuffers, GetFileInformationByHandle, READ_CONTROL, SYNCHRONIZE,
    WRITE_DAC,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
use zeroize::Zeroizing;

use super::{windows_acl, windows_open};
use crate::secure_fs::owned_file::{legacy_lock_name, temporary_name};
use crate::{ConfigError, ConfigErrorKind};

const READ_FILE_ACCESS: u32 = GENERIC_READ | READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const TEMP_FILE_ACCESS: u32 =
    GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | DELETE | SYNCHRONIZE;
const MAX_TEMPORARY_ATTEMPTS: usize = 16;

pub(in crate::secure_fs) fn read_file(
    root: &Path,
    components: &[OsString],
    maximum_bytes: usize,
) -> Result<Option<Zeroizing<Vec<u8>>>, ConfigError> {
    let Some((parent, name)) = open_existing_parent(root, components)? else {
        return Ok(None);
    };
    let Some(file) = open_existing_regular(&parent, name, true)? else {
        return Ok(None);
    };
    read_bounded(file, maximum_bytes).map(Some)
}

/// A quarantined backup: ownership still verified, DACL mode not. See
/// `owned_file::read_quarantined_backup`.
pub(in crate::secure_fs) fn read_backup(
    root: &Path,
    components: &[OsString],
    maximum_bytes: usize,
) -> Result<Option<Zeroizing<Vec<u8>>>, ConfigError> {
    let Some((parent, name)) = open_existing_parent(root, components)? else {
        return Ok(None);
    };
    let Some(file) = open_existing_regular(&parent, name, false)? else {
        return Ok(None);
    };
    read_bounded(file, maximum_bytes).map(Some)
}

pub(in crate::secure_fs) struct Transaction {
    parent: File,
    name: OsString,
    _lock: DirectoryLock,
}

impl Transaction {
    pub(in crate::secure_fs) fn begin(
        root: &Path,
        components: &[OsString],
    ) -> Result<Self, ConfigError> {
        let (name, directories) = components
            .split_last()
            .ok_or_else(|| ConfigError::new(ConfigErrorKind::PathEscape))?;
        let mut parent = open_or_create_root(root)?;
        let mut directory = root.to_path_buf();
        for component in directories {
            parent = open_or_create_directory(&parent, component)?;
            directory.push(component);
        }
        let lock = DirectoryLock::acquire(&directory)?;
        remove_legacy_lock(&parent, name);
        reject_wrong_case(&parent, name)?;
        Ok(Self {
            parent,
            name: name.clone(),
            _lock: lock,
        })
    }

    pub(in crate::secure_fs) fn read(
        &self,
        maximum_bytes: usize,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, ConfigError> {
        let Some(file) = open_existing_regular(&self.parent, &self.name, true)? else {
            return Ok(None);
        };
        read_bounded(file, maximum_bytes).map(Some)
    }

    pub(in crate::secure_fs) fn replace(&mut self, bytes: &[u8]) -> Result<(), ConfigError> {
        validate_replace_target(&self.parent, &self.name)?;
        let mut temporary = create_temporary(&self.parent, &self.name)?;
        let prepared = (|| {
            temporary.file_mut().write_all(bytes).map_err(io_error)?;
            temporary.file_mut().flush().map_err(io_error)?;
            flush_file(temporary.file())?;
            windows_acl::verify_fixed_descriptor(temporary.file())?;
            Ok(())
        })();
        if let Err(error) = prepared {
            temporary.remove()?;
            return Err(error);
        }

        if let Err(error) = rename_relative(&self.parent, temporary.file(), &self.name) {
            temporary.remove()?;
            return Err(error);
        }
        temporary.disarm();
        verify_published(&self.parent, &self.name, temporary.file())?;
        super::flush_directory(&self.parent)
    }
}

/// Cross-process exclusion for one owned directory.
///
/// Held as a session-local named mutex keyed by the directory path, so no
/// `<name>.lock` file appears beside the documents. A mutex belongs to the
/// thread that acquired it; the raw handle keeps this type (and so a
/// transaction) on that thread.
struct DirectoryLock(windows_sys::Win32::Foundation::HANDLE);

impl DirectoryLock {
    fn acquire(directory: &Path) -> Result<Self, ConfigError> {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{CreateMutexW, INFINITE, WaitForSingleObject};

        let name: Vec<u16> = directory_lock_name(directory)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `name` is a live NUL-terminated UTF-16 string; null security
        // attributes request the default descriptor.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            let error = io::Error::last_os_error();
            return Err(ConfigError::new(ConfigErrorKind::Lock).with_io_kind(error.kind()));
        }
        // SAFETY: `handle` is the live mutex handle created above.
        let waited = unsafe { WaitForSingleObject(handle, INFINITE) };
        // An abandoned mutex (its holder crashed) is still acquired; the
        // atomic replace means no half-written document can be behind it.
        if waited != WAIT_OBJECT_0 && waited != WAIT_ABANDONED {
            let error = io::Error::last_os_error();
            // SAFETY: `handle` is live and not owned by this thread.
            let _ = unsafe { CloseHandle(handle) };
            return Err(ConfigError::new(ConfigErrorKind::Lock).with_io_kind(error.kind()));
        }
        Ok(Self(handle))
    }
}

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        // SAFETY: this thread owns the mutex acquired in `acquire`.
        let _ = unsafe { ReleaseMutex(self.0) };
        // SAFETY: the handle is live and closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Mutex name for one directory: FNV-1a over the case-folded path, so every
/// build and process agrees on it regardless of the spelling it was given.
fn directory_lock_name(directory: &Path) -> String {
    let key = directory
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("Local\\mycode-config-{hash:016x}")
}

/// Deletes a `<name>.lock` left by builds that locked through a file.
/// Best-effort: one still held open by an old process is left for later.
fn remove_legacy_lock(parent: &File, name: &OsStr) {
    let legacy = legacy_lock_name(name);
    if let Ok(Some(opened)) = windows_open::open_relative_file(
        parent,
        &legacy,
        DELETE,
        windows_open::OPEN_EXISTING_DISPOSITION,
        None,
    ) {
        let _ = set_delete(&opened.file);
    }
}

/// Moves one entry aside without reading it; see
/// [`crate::secure_fs::owned_file::quarantine_owned_file`].
pub(in crate::secure_fs) fn quarantine_file(
    root: &Path,
    components: &[OsString],
    backup: &OsStr,
) -> Result<bool, ConfigError> {
    let (name, directories) = components
        .split_last()
        .ok_or_else(|| ConfigError::new(ConfigErrorKind::PathEscape))?;
    let mut parent = open_or_create_root(root)?;
    for component in directories {
        parent = open_or_create_directory(&parent, component)?;
    }
    if windows_open::child_attributes(&parent, name)?.is_none() {
        return Ok(false);
    }
    if windows_open::child_attributes(&parent, backup)?.is_some() {
        return Err(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::AlreadyExists)
        );
    }
    let entry = windows_open::open_for_rename(&parent, name)?;
    rename_relative_with_flags(&parent, &entry, backup, 0)?;
    super::flush_directory(&parent)?;
    Ok(true)
}

fn open_or_create_root(root: &Path) -> Result<File, ConfigError> {
    let descriptor = windows_acl::protected_descriptor()?;
    let expected = root.file_name().and_then(OsStr::to_str);
    let parent = windows_open::create_owned_root(
        root,
        expected,
        &descriptor,
        windows_acl::secure_existing_object,
        windows_acl::verify_fixed_descriptor,
        sync_created,
    )?;
    let name = root
        .file_name()
        .ok_or_else(|| ConfigError::for_path(ConfigErrorKind::InvalidHome, root))?;
    windows_open::open_owned_relative(&parent, name)
}

fn open_existing_root(root: &Path) -> Result<Option<File>, ConfigError> {
    let Some(parent_path) = root.parent() else {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    };
    let Some(name) = root.file_name() else {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    };
    let parent =
        windows_open::open_path_directory_follow(parent_path, windows_open::DIRECTORY_READ_ACCESS)?;
    open_existing_owned_directory(&parent, name)
}

fn open_existing_parent<'a>(
    root: &Path,
    components: &'a [OsString],
) -> Result<Option<(File, &'a OsStr)>, ConfigError> {
    let (name, directories) = components
        .split_last()
        .ok_or_else(|| ConfigError::new(ConfigErrorKind::PathEscape))?;
    let Some(mut parent) = open_existing_root(root)? else {
        return Ok(None);
    };
    for component in directories {
        let Some(next) = open_existing_owned_directory(&parent, component)? else {
            return Ok(None);
        };
        parent = next;
    }
    reject_wrong_case(&parent, name)?;
    Ok(Some((parent, name)))
}

/// Opens one existing directory and tightens it when the current user owns it.
///
/// The session index creates the home with `std::fs::create_dir_all` before
/// settings are read. That directory inherits the profile DACL. Verifying
/// the protected descriptor without installing it surfaces as
/// `settings error: owned path access control failed` and the desktop never
/// leaves its error state. A wrong owner or a reparse point is rejected
/// before any DACL change. A missing child is `Ok(None)`.
fn open_existing_owned_directory(parent: &File, name: &OsStr) -> Result<Option<File>, ConfigError> {
    reject_wrong_case(parent, name)?;
    if windows_open::child_attributes(parent, name)?.is_none() {
        return Ok(None);
    }
    let opened = windows_open::open_relative_directory(
        parent,
        name,
        windows_open::DIRECTORY_DACL_ACCESS,
        windows_open::OPEN_EXISTING_DISPOSITION,
        None,
    )?;
    windows_acl::secure_existing_object(&opened.file)?;
    windows_acl::verify_fixed_descriptor(&opened.file)?;
    reject_wrong_case(parent, name)?;
    Ok(Some(opened.file))
}

fn open_or_create_directory(parent: &File, name: &OsStr) -> Result<File, ConfigError> {
    reject_wrong_case(parent, name)?;
    let descriptor = windows_acl::protected_descriptor()?;
    let opened = windows_open::create_owned_child(parent, name, &descriptor)?;
    windows_acl::secure_existing_object(&opened.file)?;
    if opened.publication_required() {
        windows_acl::verify_fixed_descriptor(&opened.file)?;
        sync_created(&opened.file, parent)?;
    }
    reject_wrong_case(parent, name)?;
    Ok(opened.file)
}

fn open_existing_regular(
    parent: &File,
    name: &OsStr,
    require_private_dacl: bool,
) -> Result<Option<File>, ConfigError> {
    reject_wrong_case(parent, name)?;
    let opened = windows_open::open_relative_file(
        parent,
        name,
        READ_FILE_ACCESS,
        windows_open::OPEN_EXISTING_DISPOSITION,
        None,
    )?;
    let Some(opened) = opened else {
        return Ok(None);
    };
    windows_acl::require_current_owner(&opened.file)?;
    if require_private_dacl {
        // A same-user writer that created the file under its own token leaves
        // an inherited logon-session ACE behind. That is ACL drift, not
        // document damage: repair it in place and read the bytes, instead of
        // quarantining a healthy document.
        windows_acl::repair_drifted_descriptor(&opened.file)?;
    }
    reject_wrong_case(parent, name)?;
    Ok(Some(opened.file))
}

fn validate_replace_target(parent: &File, name: &OsStr) -> Result<(), ConfigError> {
    if let Some(file) = open_existing_regular(parent, name, false)? {
        windows_acl::require_current_owner(&file)?;
    }
    Ok(())
}

fn create_temporary(parent: &File, destination: &OsStr) -> Result<TemporaryFile, ConfigError> {
    let descriptor = windows_acl::protected_descriptor()?;
    for _ in 0..MAX_TEMPORARY_ATTEMPTS {
        let name = temporary_name(destination);
        match windows_open::open_relative_file(
            parent,
            &name,
            TEMP_FILE_ACCESS,
            windows_open::CREATE_FILE_DISPOSITION,
            Some(&descriptor),
        ) {
            Ok(Some(opened)) => {
                if !opened.created {
                    return Err(ConfigError::new(ConfigErrorKind::Io)
                        .with_io_kind(io::ErrorKind::AlreadyExists));
                }
                let mut temporary = TemporaryFile::new(opened.file);
                // This file was created by this call. Install the protected
                // DACL before publication when creation still inherited ACEs.
                if let Err(error) = windows_acl::secure_existing_object(temporary.file()) {
                    temporary.remove()?;
                    return Err(error);
                }
                return Ok(temporary);
            }
            Ok(None) => return Err(ConfigError::new(ConfigErrorKind::Io)),
            Err(error) if error.io_kind() == Some(io::ErrorKind::AlreadyExists) => {}
            Err(error) => return Err(error),
        }
    }
    Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::AlreadyExists))
}

pub(super) fn rename_relative(
    parent: &File,
    file: &File,
    destination: &OsStr,
) -> Result<(), ConfigError> {
    rename_relative_with_flags(parent, file, destination, 1)
}

fn rename_relative_with_flags(
    parent: &File,
    file: &File,
    destination: &OsStr,
    flags: u32,
) -> Result<(), ConfigError> {
    let wide = windows_open::wide_component(destination)?;
    let name_bytes = wide.len().saturating_mul(2);
    let header = size_of::<FILE_RENAME_INFORMATION>();
    let total = header.saturating_add(name_bytes.saturating_sub(2));
    let mut storage = vec![0u64; total.div_ceil(size_of::<u64>()).max(1)];
    let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: `storage` is aligned and covers the fixed header plus UTF-16
    // component. RootDirectory anchors the replacement to `parent`.
    unsafe {
        (*information).Anonymous.Flags = flags;
        (*information).RootDirectory = parent.as_raw_handle();
        (*information).FileNameLength =
            u32::try_from(name_bytes).map_err(|_| ConfigError::new(ConfigErrorKind::PathEscape))?;
        std::ptr::copy_nonoverlapping(
            wide.as_ptr(),
            (*information).FileName.as_mut_ptr(),
            wide.len(),
        );
    }
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: Both handles and `storage` remain live for this synchronous
    // handle-relative rename.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut status_block,
            storage.as_ptr().cast(),
            u32::try_from(total).map_err(|_| ConfigError::new(ConfigErrorKind::PathEscape))?,
            FileRenameInformation,
        )
    };
    if !windows_open::nt_success(status) {
        let error = windows_open::map_ntstatus(status);
        return Err(remap(error, ConfigErrorKind::AtomicReplace));
    }
    Ok(())
}

fn verify_published(parent: &File, name: &OsStr, source: &File) -> Result<(), ConfigError> {
    let published = open_existing_regular(parent, name, true)?
        .ok_or_else(|| ConfigError::new(ConfigErrorKind::AtomicReplace))?;
    if file_identity(source)? != file_identity(&published)? {
        return Err(ConfigError::new(ConfigErrorKind::AtomicReplace));
    }
    Ok(())
}

fn file_identity(file: &File) -> Result<(u32, u32, u32), ConfigError> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` is live and `information` is writable output storage.
    let queried = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if queried == 0 {
        let error = io::Error::last_os_error();
        return Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind()));
    }
    if information.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
    {
        return Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::InvalidData));
    }
    Ok((
        information.dwVolumeSerialNumber,
        information.nFileIndexHigh,
        information.nFileIndexLow,
    ))
}

fn read_bounded(mut file: File, maximum_bytes: usize) -> Result<Zeroizing<Vec<u8>>, ConfigError> {
    let declared = usize::try_from(file.metadata().map_err(io_error)?.len())
        .map_err(|_| ConfigError::new(ConfigErrorKind::Oversized))?;
    if declared > maximum_bytes {
        return Err(ConfigError::new(ConfigErrorKind::Oversized));
    }
    let mut bytes = Zeroizing::new(vec![0_u8; declared]);
    if let Err(error) = file.read_exact(bytes.as_mut_slice()) {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            return Err(ConfigError::new(ConfigErrorKind::Oversized));
        }
        return Err(io_error(error));
    }

    let mut extra = Zeroizing::new([0_u8; 1]);
    match file.read_exact(extra.as_mut_slice()) {
        Ok(()) => Err(ConfigError::new(ConfigErrorKind::Oversized)),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(bytes),
        Err(error) => Err(io_error(error)),
    }
}

pub(super) fn flush_file(file: &File) -> Result<(), ConfigError> {
    // SAFETY: `file` is a live regular-file handle. FlushFileBuffers documents
    // zero as failure and GetLastError only for that return value.
    let flushed = unsafe { FlushFileBuffers(file.as_raw_handle()) };
    if flushed == 0 {
        let error = io::Error::last_os_error();
        return Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind()));
    }
    Ok(())
}

fn sync_created(directory: &File, parent: &File) -> Result<(), ConfigError> {
    super::flush_directory(directory)?;
    super::flush_directory(parent)
}

fn reject_wrong_case(parent: &File, name: &OsStr) -> Result<(), ConfigError> {
    let Some(expected) = name.to_str() else {
        return Ok(());
    };
    if windows_open::find_wrong_case_child(parent, expected)?.is_some() {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(())
}

fn io_error(error: io::Error) -> ConfigError {
    ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind())
}

fn remap(error: ConfigError, kind: ConfigErrorKind) -> ConfigError {
    let mut mapped = ConfigError::new(kind);
    if let Some(io_kind) = error.io_kind() {
        mapped = mapped.with_io_kind(io_kind);
    }
    mapped
}

struct TemporaryFile {
    file: File,
    armed: bool,
}

impl TemporaryFile {
    fn new(file: File) -> Self {
        Self { file, armed: true }
    }

    fn file(&self) -> &File {
        &self.file
    }

    fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn remove(&mut self) -> Result<(), ConfigError> {
        if self.armed {
            set_delete(&self.file)?;
            self.armed = false;
        }
        Ok(())
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = set_delete(&self.file);
        }
    }
}

pub(super) fn set_delete(file: &File) -> Result<(), ConfigError> {
    let mut information = FILE_DISPOSITION_INFORMATION { DeleteFile: true };
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: `file` is a live DELETE-capable handle and `information` is live
    // writable disposition storage.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut status_block,
            std::ptr::addr_of_mut!(information).cast(),
            u32::try_from(size_of::<FILE_DISPOSITION_INFORMATION>())
                .expect("FILE_DISPOSITION_INFORMATION fits u32"),
            FileDispositionInformation,
        )
    };
    if !windows_open::nt_success(status) {
        return Err(windows_open::map_ntstatus(status));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use crate::{
        AppSettings, AuthorityRevision, HomeLayout, read_app_settings, replace_app_settings,
    };

    struct TempTree {
        path: PathBuf,
    }

    impl TempTree {
        fn new(label: &str) -> Self {
            let nanos = crate::unix_now().as_nanos();
            let path = std::env::temp_dir().join(format!(
                "mycode-secure-fs-{label}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("temp directory");
            Self { path }
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Replaces one file's DACL; the owner keeps WRITE_DAC implicitly. The
    /// open skips `SYNCHRONIZE`, which an empty DACL does not grant.
    fn set_dacl(path: &std::path::Path, descriptor: &super::windows_acl::SecurityDescriptor) {
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, SetKernelObjectSecurity};
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

        let directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path.parent().expect("parent"))
            .expect("open parent directory");
        let file = super::windows_open::open_unsynchronized(
            &directory,
            path.file_name().expect("file name"),
            super::READ_CONTROL | super::WRITE_DAC,
        )
        .unwrap_or_else(|error| panic!("open {} for WRITE_DAC: {error:?}", path.display()));
        // SAFETY: `file` is live with WRITE_DAC and `descriptor` stays live
        // for this synchronous call.
        let updated = unsafe {
            SetKernelObjectSecurity(
                file.as_raw_handle(),
                DACL_SECURITY_INFORMATION,
                descriptor.as_ptr(),
            )
        };
        assert_ne!(updated, 0, "{}", std::io::Error::last_os_error());
    }

    /// The field report: a read-only `secrets.json` whose DACL grants nobody
    /// anything blocked startup with "owned path access control failed".
    #[test]
    fn a_read_only_document_with_an_empty_dacl_is_moved_aside_and_reset() {
        let tree = TempTree::new("empty-dacl");
        let root = tree.path.join("home");
        fs::create_dir(&root).expect("home");
        let home = HomeLayout::from_root(&root).expect("layout");
        let path = root.join("secrets.json");
        fs::write(&path, br#"{"providers":{"x":"y"}}"#).expect("write");
        let mut permissions = fs::metadata(&path).expect("metadata").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).expect("read-only");
        set_dacl(
            &path,
            &super::windows_acl::empty_dacl_descriptor().expect("empty descriptor"),
        );
        let refused = crate::read_owned_file(&home, "secrets.json", 1024).expect_err("strict read");
        assert_eq!(refused.kind(), crate::ConfigErrorKind::AccessControl);

        let (secrets, repair) = crate::read_provider_secrets_with_repair(&home).expect("reset");
        assert!(secrets.provider_ids().is_empty());
        let repair = repair.expect("reported as reset");
        let backup = root.join(&repair.backup);
        assert!(backup.exists(), "the unreadable file was kept aside");
        assert!(
            crate::read_provider_secrets_with_repair(&home)
                .expect("reread")
                .1
                .is_none(),
            "the fresh document must read cleanly"
        );

        // Restore an ordinary DACL so the temp tree can be removed.
        set_dacl(
            &backup,
            &super::windows_acl::protected_descriptor().expect("protected descriptor"),
        );
        let mut permissions = fs::metadata(&backup).expect("metadata").permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = fs::set_permissions(&backup, permissions);
    }

    #[test]
    fn missing_home_read_does_not_create_it() {
        let tree = TempTree::new("missing");
        let root = tree.path.join("home");
        let home = HomeLayout::from_root(&root).expect("layout");

        let loaded = read_app_settings(&home).expect("defaults");

        assert_eq!(loaded, AppSettings::default());
        assert!(!root.exists());
    }

    /// A home created the way the session index does (`create_dir_all`)
    /// inherits the parent DACL. Reading it must install the protected
    /// descriptor instead of failing the process closed, and the first
    /// settings publish must then succeed.
    #[test]
    fn inherited_home_reads_defaults_and_publishes_settings() {
        let tree = TempTree::new("inherited");
        let root = tree.path.join("home");
        fs::create_dir(&root).expect("home");
        let home = HomeLayout::from_root(&root).expect("layout");

        let loaded = read_app_settings(&home).expect("defaults");
        assert_eq!(loaded, AppSettings::default());
        assert!(!root.join("settings.json").exists());

        let mut settings = AppSettings::default();
        settings.appearance.font_size = "l".to_owned();
        replace_app_settings(&home, AuthorityRevision::ABSENT, &settings).expect("publish");

        let loaded = read_app_settings(&home).expect("reread");
        assert_eq!(loaded.appearance.font_size, "l");
    }

    /// A same-user tool (an agent's helper process, for example) creates
    /// `settings.json` under its own token: the new file carries an extra
    /// read-only ACE and no protected DACL. The strict read repairs that drift
    /// in place instead of quarantining a healthy document and reporting it.
    #[test]
    fn a_drifted_dacl_is_repaired_without_discarding_the_document() {
        let tree = TempTree::new("dacl-drift");
        let root = tree.path.join("home");
        fs::create_dir(&root).expect("home");
        let home = HomeLayout::from_root(&root).expect("layout");
        let path = root.join("settings.json");
        fs::write(
            &path,
            br#"{"formatVersion":1,"kind":"mycode-app-settings","revision":1,"mcpServers":[],"appearance":{"theme":"dark"}}"#,
        )
        .expect("write");
        let sid = super::windows_acl::current_user_sid_string().expect("sid");
        let sddl = format!("O:{sid}D:(A;;FA;;;{sid})(A;;FA;;;SY)(A;;FR;;;WD)");
        set_dacl(
            &path,
            &super::windows_acl::descriptor_for_tests(&sddl).expect("descriptor"),
        );

        let settings = read_app_settings(&home).expect("drifted ACL is repaired on read");
        assert_eq!(settings.appearance.theme, "dark");
        let reopened = fs::File::open(&path).expect("republished file");
        super::windows_acl::verify_fixed_descriptor(&reopened).expect("protected DACL");
        assert!(
            !fs::read_dir(&root)
                .expect("home")
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().contains(".broken-")),
            "a repaired ACL must not be reported as a damaged document"
        );
    }

    /// When another principal holds write access, the bytes could have been
    /// changed by someone else: the strict read still refuses them.
    #[test]
    fn a_dacl_another_principal_can_write_is_still_damage() {
        let tree = TempTree::new("foreign-writer");
        let root = tree.path.join("home");
        fs::create_dir(&root).expect("home");
        let home = HomeLayout::from_root(&root).expect("layout");
        let path = root.join("settings.json");
        fs::write(
            &path,
            br#"{"formatVersion":1,"kind":"mycode-app-settings","revision":1,"mcpServers":[],"appearance":{"theme":"dark"}}"#,
        )
        .expect("write");
        let sid = super::windows_acl::current_user_sid_string().expect("sid");
        let sddl = format!("O:{sid}D:(A;;FA;;;{sid})(A;;FA;;;SY)(A;;FA;;;WD)");
        set_dacl(
            &path,
            &super::windows_acl::descriptor_for_tests(&sddl).expect("descriptor"),
        );

        let refused =
            crate::read_owned_file(&home, "settings.json", 4096).expect_err("strict read");
        assert_eq!(refused.kind(), crate::ConfigErrorKind::AccessControl);
    }
}
