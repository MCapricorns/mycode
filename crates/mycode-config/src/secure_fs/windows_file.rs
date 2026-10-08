//! Windows native private regular-file transactions.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use uuid::Uuid;
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
use crate::{ConfigError, ConfigErrorKind};

const READ_FILE_ACCESS: u32 = GENERIC_READ | READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const LOCK_FILE_ACCESS: u32 = GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | SYNCHRONIZE;
const TEMP_FILE_ACCESS: u32 =
    GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | DELETE | SYNCHRONIZE;
const MAX_TEMPORARY_ATTEMPTS: usize = 16;

pub(in crate::secure_fs) fn ensure_directory(
    root: &Path,
    components: &[OsString],
) -> Result<(), ConfigError> {
    let mut directory = open_or_create_root(root)?;
    for component in components {
        directory = open_or_create_directory(&directory, component)?;
    }
    Ok(())
}

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

pub(in crate::secure_fs) struct Transaction {
    parent: File,
    name: OsString,
    _lock: File,
    /// Whether the persistent lock already carried the exact fixed descriptor
    /// when it was opened.
    lock_was_private: bool,
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
        for component in directories {
            parent = open_or_create_directory(&parent, component)?;
        }
        reject_wrong_case(&parent, name)?;
        let lock_name = lock_name(name);
        reject_wrong_case(&parent, &lock_name)?;
        let (lock, lock_was_private) = open_lock(&parent, &lock_name)?;
        reject_wrong_case(&parent, &lock_name)?;
        File::lock(&lock)
            .map_err(|error| ConfigError::new(ConfigErrorKind::Lock).with_io_kind(error.kind()))?;
        Ok(Self {
            parent,
            name: name.clone(),
            _lock: lock,
            lock_was_private,
        })
    }

    /// Rejects a transaction whose persistent lock was widened before opening.
    ///
    /// A read-modify-write publishes content derived from what it just read,
    /// so a lock file that another principal could hold or rewrite is an
    /// access-control failure rather than something to tighten and continue.
    pub(in crate::secure_fs) fn require_private_lock(&self) -> Result<(), ConfigError> {
        if self.lock_was_private {
            Ok(())
        } else {
            Err(ConfigError::new(ConfigErrorKind::AccessControl))
        }
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

impl Drop for Transaction {
    fn drop(&mut self) {
        let _ = File::unlock(&self._lock);
    }
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
        windows_acl::verify_fixed_descriptor(&opened.file)?;
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

fn open_lock(parent: &File, name: &OsStr) -> Result<(File, bool), ConfigError> {
    let descriptor = windows_acl::protected_descriptor()?;
    let opened = windows_open::open_relative_file(
        parent,
        name,
        LOCK_FILE_ACCESS,
        windows_open::OPEN_OR_CREATE_FILE_DISPOSITION,
        Some(&descriptor),
    )?
    .ok_or_else(|| ConfigError::new(ConfigErrorKind::Lock))?;
    windows_acl::require_current_owner(&opened.file)?;
    // A widened existing lock is tightened here, and the pre-tightening
    // evidence is reported to callers that must fail closed instead.
    // A lock created by this call has no previous holder. `NtCreateFile`
    // can still attach inherited ACEs until the protected DACL is installed,
    // so that pre-repair check is not evidence another principal held it.
    let created = opened.created;
    let was_private = windows_acl::verify_fixed_descriptor(&opened.file).is_ok();
    windows_acl::secure_existing_object(&opened.file)?;
    windows_acl::verify_fixed_descriptor(&opened.file)?;
    Ok((opened.file, was_private || created))
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
    let wide = windows_open::wide_component(destination)?;
    let name_bytes = wide.len().saturating_mul(2);
    let header = size_of::<FILE_RENAME_INFORMATION>();
    let total = header.saturating_add(name_bytes.saturating_sub(2));
    let mut storage = vec![0u64; total.div_ceil(size_of::<u64>()).max(1)];
    let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: `storage` is aligned and covers the fixed header plus UTF-16
    // component. RootDirectory anchors the replacement to `parent`.
    unsafe {
        (*information).Anonymous.Flags = 1;
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

fn lock_name(destination: &OsStr) -> OsString {
    let mut name = destination.to_os_string();
    name.push(".lock");
    name
}

fn temporary_name(destination: &OsStr) -> OsString {
    let mut name = OsString::from(".");
    name.push(destination);
    name.push(format!(".{}.tmp", Uuid::new_v4().simple()));
    name
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{
        AppSettings, AuthorityRevision, HomeLayout, read_app_settings, replace_app_settings,
    };

    struct TempTree {
        path: PathBuf,
    }

    impl TempTree {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
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
        settings.appearance.palette = "ocean".to_owned();
        replace_app_settings(&home, AuthorityRevision::ABSENT, &settings).expect("publish");

        let loaded = read_app_settings(&home).expect("reread");
        assert_eq!(loaded.appearance.palette, "ocean");
    }
}
