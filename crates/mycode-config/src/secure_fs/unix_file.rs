//! Unix anchored private regular-file transactions.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::path::Path;

use rustix::fs::{self as rfs, AtFlags, Mode, OFlags};
use rustix::io::Errno;
use zeroize::Zeroizing;

use super as unix;
use crate::secure_fs::owned_file::{legacy_lock_name, temporary_name};
use crate::{ConfigError, ConfigErrorKind};

const FILE_MODE: rfs::RawMode = 0o600;
const MAX_CREATE_ATTEMPTS: usize = 16;

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

/// One locked read-modify-replace.
///
/// The advisory lock is `flock` on the parent directory handle itself, so no
/// `<name>.lock` file appears beside the documents. Every transaction opens
/// its own directory description, so threads and processes both exclude.
pub(in crate::secure_fs) struct Transaction {
    parent: File,
    name: OsString,
}

impl Transaction {
    pub(in crate::secure_fs) fn begin(
        root: &Path,
        components: &[OsString],
    ) -> Result<Self, ConfigError> {
        let (directories, name) = components
            .split_last()
            .map(|(name, directories)| (directories, name.clone()))
            .ok_or_else(|| ConfigError::new(ConfigErrorKind::PathEscape))?;
        let mut parent = open_or_create_root(root)?;
        for component in directories {
            reject_wrong_case(&parent, component)?;
            parent = unix::create_or_open_directory(&parent, component, true)?;
        }
        File::lock(&parent)
            .map_err(|error| ConfigError::new(ConfigErrorKind::Lock).with_io_kind(error.kind()))?;
        remove_legacy_lock(&parent, &name);
        reject_wrong_case(&parent, &name)?;
        Ok(Self { parent, name })
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
        let (mut temporary, temporary_name) = create_temporary(&self.parent, &self.name)?;
        let mut cleanup = TemporaryName::new(&self.parent, temporary_name);
        let prepared = (|| {
            temporary.write_all(bytes).map_err(io_error)?;
            temporary.flush().map_err(io_error)?;
            sync_file(&temporary)?;
            verify_private_regular(&temporary)?;
            Ok(())
        })();
        if let Err(error) = prepared {
            cleanup.remove()?;
            return Err(error);
        }

        if let Err(error) = rfs::renameat(
            self.parent.as_fd(),
            cleanup.name(),
            self.parent.as_fd(),
            &self.name,
        ) {
            cleanup.remove()?;
            return Err(unix::map_errno(error, ConfigErrorKind::AtomicReplace));
        }
        cleanup.disarm();
        verify_published(&self.parent, &self.name, &temporary)?;
        unix::sync_directory(&self.parent)
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        let _ = File::unlock(&self.parent);
    }
}

/// Deletes a `<name>.lock` left by builds that locked through a file. Only a
/// regular file is removed; anything else is left alone.
fn remove_legacy_lock(parent: &File, name: &OsStr) {
    let legacy = legacy_lock_name(name);
    if let Ok(stat) = rfs::statat(parent.as_fd(), &legacy, AtFlags::SYMLINK_NOFOLLOW)
        && rfs::FileType::from_raw_mode(stat.st_mode) == rfs::FileType::RegularFile
    {
        let _ = rfs::unlinkat(parent.as_fd(), &legacy, AtFlags::empty());
    }
}

/// Moves one entry aside without reading it; see
/// [`crate::secure_fs::owned_file::quarantine_owned_file`]. `renameat` moves
/// a symlink itself, never its target.
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
        reject_wrong_case(&parent, component)?;
        parent = unix::create_or_open_directory(&parent, component, true)?;
    }
    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    let renamed = rfs::renameat_with(
        parent.as_fd(),
        name,
        parent.as_fd(),
        backup,
        rfs::RenameFlags::NOREPLACE,
    );
    #[cfg(not(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    )))]
    let renamed = {
        // Coordinate with owned-file writers on platforms without an atomic
        // no-replace rename. The directory handle releases this lock on drop.
        parent
            .lock()
            .map_err(|error| ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind()))?;
        match rfs::statat(parent.as_fd(), backup, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => {
                return Err(ConfigError::new(ConfigErrorKind::Io)
                    .with_io_kind(io::ErrorKind::AlreadyExists));
            }
            Err(Errno::NOENT) => {}
            Err(error) => return Err(unix::map_errno(error, ConfigErrorKind::Io)),
        }
        rfs::renameat(parent.as_fd(), name, parent.as_fd(), backup)
    };
    match renamed {
        Ok(()) => {
            unix::sync_directory(&parent)?;
            Ok(true)
        }
        Err(Errno::NOENT) => Ok(false),
        Err(error) => Err(unix::map_errno(error, ConfigErrorKind::Io)),
    }
}

fn open_or_create_root(root: &Path) -> Result<File, ConfigError> {
    let expected = root.file_name().and_then(OsStr::to_str);
    unix::create_owned_root(root, expected)
}

/// Opens one existing directory and tightens it when we own it.
///
/// Settings startup only reads. A home created outside this crate is often
/// mode `0o755`. Verifying that mode without `fchmod` surfaces as
/// `settings error: owned path access control failed` and the page stays
/// loading. Wrong owners and symlinks are rejected before any mode change.
fn open_existing_owned_directory(parent: &File, name: &OsStr) -> Result<File, ConfigError> {
    let directory = unix::open_existing_directory(parent, name)?;
    unix::enforce_owned_directory(&directory)?;
    Ok(directory)
}

fn open_existing_root(root: &Path) -> Result<Option<File>, ConfigError> {
    let Some(parent_path) = root.parent() else {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    };
    let Some(name) = root.file_name() else {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    };
    let parent = unix::open_trailing_directory(parent_path)?;
    if let Some(expected) = name.to_str() {
        unix::reject_wrong_case_child(&parent, expected, ConfigErrorKind::AccessControl)?;
    }
    match rfs::statat(parent.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if rfs::FileType::from_raw_mode(stat.st_mode) == rfs::FileType::Symlink => {
            Err(ConfigError::new(ConfigErrorKind::LinkEscape))
        }
        Ok(_) => Ok(Some(open_existing_owned_directory(&parent, name)?)),
        Err(Errno::NOENT) => Ok(None),
        Err(error) => Err(unix::map_errno(error, ConfigErrorKind::Io)),
    }
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
        reject_wrong_case(&parent, component)?;
        match rfs::statat(parent.as_fd(), component, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if rfs::FileType::from_raw_mode(stat.st_mode) == rfs::FileType::Symlink => {
                return Err(ConfigError::new(ConfigErrorKind::LinkEscape));
            }
            Ok(_) => {
                parent = open_existing_owned_directory(&parent, component)?;
            }
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => return Err(unix::map_errno(error, ConfigErrorKind::Io)),
        }
    }
    reject_wrong_case(&parent, name)?;
    Ok(Some((parent, name)))
}

fn reject_wrong_case(parent: &File, name: &OsStr) -> Result<(), ConfigError> {
    let Some(expected) = name.to_str() else {
        return Ok(());
    };
    unix::reject_wrong_case_child(parent, expected, ConfigErrorKind::AccessControl)
}

fn open_existing_regular(
    parent: &File,
    name: &OsStr,
    require_private_mode: bool,
) -> Result<Option<File>, ConfigError> {
    reject_wrong_case(parent, name)?;
    match rfs::statat(parent.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => match rfs::FileType::from_raw_mode(stat.st_mode) {
            rfs::FileType::Symlink => return Err(ConfigError::new(ConfigErrorKind::LinkEscape)),
            rfs::FileType::RegularFile => {}
            _ => {
                return Err(
                    ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::InvalidData)
                );
            }
        },
        Err(Errno::NOENT) => return Ok(None),
        Err(error) => return Err(unix::map_errno(error, ConfigErrorKind::Io)),
    }
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let file = File::from(unix::open_component(parent, name, flags)?);
    verify_regular_owner(&file)?;
    if require_private_mode {
        verify_private_regular(&file)?;
    }
    reject_wrong_case(parent, name)?;
    Ok(Some(file))
}

fn validate_replace_target(parent: &File, name: &OsStr) -> Result<(), ConfigError> {
    if let Some(file) = open_existing_regular(parent, name, false)? {
        verify_regular_owner(&file)?;
    }
    Ok(())
}

fn create_temporary(parent: &File, destination: &OsStr) -> Result<(File, OsString), ConfigError> {
    for _ in 0..MAX_CREATE_ATTEMPTS {
        let name = temporary_name(destination);
        let flags =
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW;
        match unix::open_component(parent, &name, flags) {
            Ok(descriptor) => {
                let file = File::from(descriptor);
                let mut cleanup = TemporaryName::new(parent, name.clone());
                let checked = rfs::fchmod(file.as_fd(), Mode::from_raw_mode(FILE_MODE))
                    .map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))
                    .and_then(|()| verify_private_regular(&file));
                if let Err(error) = checked {
                    cleanup.remove()?;
                    return Err(error);
                }
                cleanup.disarm();
                return Ok((file, name));
            }
            Err(error) if error.io_kind() == Some(io::ErrorKind::AlreadyExists) => {}
            Err(error) => return Err(error),
        }
    }
    Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::AlreadyExists))
}

fn verify_regular_owner(file: &File) -> Result<rfs::Stat, ConfigError> {
    let stat =
        rfs::fstat(file.as_fd()).map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))?;
    if rfs::FileType::from_raw_mode(stat.st_mode) != rfs::FileType::RegularFile {
        return Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::InvalidData));
    }
    if stat.st_uid != rustix::process::geteuid().as_raw() {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(stat)
}

fn verify_private_regular(file: &File) -> Result<(), ConfigError> {
    let stat = verify_regular_owner(file)?;
    if stat.st_mode & 0o777 != FILE_MODE {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(())
}

fn verify_published(parent: &File, name: &OsStr, source: &File) -> Result<(), ConfigError> {
    let published = open_existing_regular(parent, name, true)?
        .ok_or_else(|| ConfigError::new(ConfigErrorKind::AtomicReplace))?;
    let source_stat = verify_regular_owner(source)?;
    let published_stat = verify_regular_owner(&published)?;
    if source_stat.st_dev != published_stat.st_dev || source_stat.st_ino != published_stat.st_ino {
        return Err(ConfigError::new(ConfigErrorKind::AtomicReplace));
    }
    Ok(())
}

fn read_bounded(mut file: File, maximum_bytes: usize) -> Result<Zeroizing<Vec<u8>>, ConfigError> {
    let stat = verify_regular_owner(&file)?;
    let declared =
        usize::try_from(stat.st_size).map_err(|_| ConfigError::new(ConfigErrorKind::Oversized))?;
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

#[cfg(target_vendor = "apple")]
fn sync_file(file: &File) -> Result<(), ConfigError> {
    rfs::fcntl_fullfsync(file.as_fd()).map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))
}

#[cfg(not(target_vendor = "apple"))]
fn sync_file(file: &File) -> Result<(), ConfigError> {
    rfs::fsync(file.as_fd()).map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))
}

fn io_error(error: io::Error) -> ConfigError {
    ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind())
}

struct TemporaryName<'a> {
    parent: &'a File,
    name: OsString,
    armed: bool,
}

impl<'a> TemporaryName<'a> {
    fn new(parent: &'a File, name: OsString) -> Self {
        Self {
            parent,
            name,
            armed: true,
        }
    }

    fn name(&self) -> &OsStr {
        &self.name
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn remove(&mut self) -> Result<(), ConfigError> {
        if self.armed {
            rfs::unlinkat(self.parent.as_fd(), &self.name, AtFlags::empty())
                .map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))?;
            self.armed = false;
        }
        Ok(())
    }
}

impl Drop for TemporaryName<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = rfs::unlinkat(self.parent.as_fd(), &self.name, AtFlags::empty());
        }
    }
}
