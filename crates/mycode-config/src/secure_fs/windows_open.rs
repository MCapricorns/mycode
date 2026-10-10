//! Directory opens that reject a trailing reparse point.
//!
//! A pre-existing parent outside the owned boundary may resolve through prefix
//! reparses. The owned root and each child are opened or created relative to a
//! parent handle with `FILE_OPEN_REPARSE_POINT`.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Component, Path};
use std::ptr::{null, null_mut};

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_DIRECTORY_INFORMATION, FILE_NON_DIRECTORY_FILE,
    FILE_OPEN, FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_IF, FILE_OPEN_REPARSE_POINT,
    FILE_SYNCHRONOUS_IO_NONALERT, FileDirectoryInformation, NtCreateFile, NtQueryDirectoryFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_MORE_DATA, ERROR_NO_MORE_FILES, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE, NTSTATUS,
    OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, STATUS_NO_MORE_FILES, STATUS_NO_SUCH_FILE,
    STATUS_OBJECT_NAME_NOT_FOUND, UNICODE_STRING,
};
use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_APPEND_DATA, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_FULL_DIR_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, FILE_WRITE_DATA,
    FileFullDirectoryInfo, FileFullDirectoryRestartInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx, OPEN_EXISTING, READ_CONTROL, SYNCHRONIZE, WRITE_DAC,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
use windows_sys::Win32::System::WindowsProgramming::FILE_CREATED;

use super::windows_acl::SecurityDescriptor;
use crate::{ConfigError, ConfigErrorKind};

const OPEN_OR_CREATE_DISPOSITION: u32 = FILE_OPEN_IF;
pub(super) const DIRECTORY_READ_ACCESS: u32 =
    GENERIC_READ | READ_CONTROL | FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES;
// WRITE_DAC without GENERIC_READ/GENERIC_WRITE so owner-implicit WRITE_DAC still works.
pub(super) const DIRECTORY_DACL_ACCESS: u32 = READ_CONTROL
    | WRITE_DAC
    | FILE_LIST_DIRECTORY
    | FILE_TRAVERSE
    | SYNCHRONIZE
    | FILE_READ_ATTRIBUTES;
pub(super) const OWNED_DIRECTORY_ACCESS: u32 =
    DIRECTORY_READ_ACCESS | FILE_WRITE_DATA | FILE_APPEND_DATA | FILE_DELETE_CHILD | WRITE_DAC;

pub(super) struct OpenedDirectory {
    pub(super) file: File,
    publication_required: bool,
}

impl OpenedDirectory {
    pub(super) fn publication_required(&self) -> bool {
        self.publication_required
    }
}

pub(super) struct OpenedFile {
    pub(super) file: File,
    pub(super) created: bool,
}

struct NativeOpenedDirectory {
    file: File,
    created: bool,
}

/// Creates or repairs the owned root and returns its parent handle.
///
/// The root directory is opened relative to the parent with the exact
/// protected DACL applied, secured, and — when newly created — synced; the
/// returned parent lets callers reopen the root with full owned access.
pub(super) fn create_owned_root(
    root: &Path,
    expected_root_name: Option<&str>,
    descriptor: &SecurityDescriptor,
    mut secure_final: impl FnMut(&File) -> Result<(), ConfigError>,
    mut verify_created: impl FnMut(&File) -> Result<(), ConfigError>,
    mut sync_created: impl FnMut(&File, &File) -> Result<(), ConfigError>,
) -> Result<File, ConfigError> {
    if !has_normal_component(root) {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    }
    let Some(parent_path) = root.parent() else {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    };
    let Some(name) = root.file_name() else {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    };
    // Prefix opens may follow. Request write on this first open so creating the
    // owned root does not re-walk `parent_path` (ReOpenFile cannot add access).
    let parent = open_path_directory_follow(
        parent_path,
        DIRECTORY_READ_ACCESS | FILE_WRITE_DATA | FILE_APPEND_DATA,
    )?;
    reject_wrong_case_root(&parent, expected_root_name)?;
    let opened = open_relative_directory(
        &parent,
        name,
        DIRECTORY_DACL_ACCESS,
        OPEN_OR_CREATE_DISPOSITION,
        Some(descriptor),
    )?;
    secure_final(&opened.file)?;
    if opened.publication_required {
        // An absence observation owns publication even if another bootstrap
        // won FILE_OPEN_IF and the IO_STATUS_BLOCK reports FILE_OPENED.
        verify_created(&opened.file)?;
        sync_created(&opened.file, &parent)?;
    }
    reject_wrong_case_root(&parent, expected_root_name)?;
    Ok(parent)
}

pub(super) fn create_owned_child(
    parent: &File,
    name: &OsStr,
    descriptor: &SecurityDescriptor,
) -> Result<OpenedDirectory, ConfigError> {
    open_relative_directory(
        parent,
        name,
        OWNED_DIRECTORY_ACCESS,
        OPEN_OR_CREATE_DISPOSITION,
        Some(descriptor),
    )
}

pub(super) fn open_owned_relative(parent: &File, name: &OsStr) -> Result<File, ConfigError> {
    let opened = open_relative_directory(parent, name, OWNED_DIRECTORY_ACCESS, FILE_OPEN, None)?;
    Ok(opened.file)
}

/// Opens a regular file only under its exact spelling.
pub(super) fn open_relative_file(
    parent: &File,
    name: &OsStr,
    access: u32,
    disposition: u32,
    descriptor: Option<&SecurityDescriptor>,
) -> Result<Option<OpenedFile>, ConfigError> {
    let existing_attributes = child_attributes(parent, name)?;
    match existing_attributes {
        Some(attributes) if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
            return Err(ConfigError::new(ConfigErrorKind::LinkEscape));
        }
        Some(attributes) if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 => {
            return Err(
                ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::InvalidData)
            );
        }
        Some(_) => {}
        None if disposition == FILE_OPEN => return Ok(None),
        None => {}
    }
    let opened = nt_open_file(parent, name, access, disposition, descriptor, true)?;
    if !opened.created {
        reject_reparse_or_directory(&opened.file)?;
    }
    Ok(Some(OpenedFile {
        file: opened.file,
        created: opened.created,
    }))
}

pub(super) const OPEN_EXISTING_DISPOSITION: u32 = FILE_OPEN;
pub(super) const OPEN_OR_CREATE_FILE_DISPOSITION: u32 = FILE_OPEN_IF;
pub(super) const CREATE_FILE_DISPOSITION: u32 = FILE_CREATE;

pub(super) fn open_relative_directory(
    parent: &File,
    name: &OsStr,
    access: u32,
    disposition: u32,
    descriptor: Option<&SecurityDescriptor>,
) -> Result<OpenedDirectory, ConfigError> {
    let existing_attributes = child_attributes(parent, name)?;
    let observed_missing = existing_attributes.is_none();
    match existing_attributes {
        Some(attributes) if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
            return Err(ConfigError::new(ConfigErrorKind::LinkEscape));
        }
        Some(attributes) if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 => {
            return Err(
                ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::NotADirectory)
            );
        }
        Some(_) => {}
        None if disposition == FILE_OPEN => {
            return Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::NotFound));
        }
        None => {}
    }
    let desired_access = if disposition != FILE_OPEN && existing_attributes.is_none() {
        access | FILE_WRITE_DATA | FILE_APPEND_DATA
    } else {
        access
    };
    let opened = nt_open_directory(parent, name, desired_access, disposition, descriptor, true)?;
    reject_reparse_or_wrong_type(&opened.file)?;
    Ok(OpenedDirectory {
        file: opened.file,
        publication_required: requires_publication(disposition, observed_missing, opened.created),
    })
}

fn requires_publication(disposition: u32, observed_missing: bool, created: bool) -> bool {
    disposition == FILE_OPEN_IF && (observed_missing || created)
}

fn nt_open_directory(
    parent: &File,
    name: &OsStr,
    access: u32,
    disposition: u32,
    descriptor: Option<&SecurityDescriptor>,
    case_insensitive: bool,
) -> Result<NativeOpenedDirectory, ConfigError> {
    let mut wide = wide_component(name)?;
    let byte_length = u16::try_from(wide.len().saturating_mul(2))
        .map_err(|_| ConfigError::new(ConfigErrorKind::PathEscape))?;
    wide.push(0);
    let name_string = UNICODE_STRING {
        Length: byte_length,
        MaximumLength: byte_length.saturating_add(2),
        Buffer: wide.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>()).expect("OBJECT_ATTRIBUTES fits u32"),
        RootDirectory: parent.as_raw_handle(),
        ObjectName: std::ptr::addr_of!(name_string),
        Attributes: if case_insensitive {
            OBJ_CASE_INSENSITIVE
        } else {
            0
        },
        SecurityDescriptor: descriptor.map_or(null(), |value| value.as_ptr().cast()),
        SecurityQualityOfService: null(),
    };
    let mut status_block = IO_STATUS_BLOCK::default();
    let mut handle: HANDLE = null_mut();
    let sharing = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    // SAFETY: The object attributes borrow a live parent handle, validated
    // single-component UTF-16 name, and optional live security descriptor for
    // this call. A successful handle is returned with owned lifetime.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access | SYNCHRONIZE | FILE_READ_ATTRIBUTES,
            &attributes,
            &mut status_block,
            null(),
            FILE_ATTRIBUTE_DIRECTORY,
            sharing,
            disposition,
            FILE_DIRECTORY_FILE
                | FILE_OPEN_FOR_BACKUP_INTENT
                | FILE_OPEN_REPARSE_POINT
                | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
            0,
        )
    };
    if !nt_success(status) {
        return Err(map_ntstatus(status));
    }
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(ConfigError::new(ConfigErrorKind::Io));
    }
    // SAFETY: NtCreateFile returned a fresh successful handle.
    let file = unsafe { File::from_raw_handle(handle) };
    Ok(NativeOpenedDirectory {
        file,
        created: status_block.Information == FILE_CREATED as usize,
    })
}

fn nt_open_file(
    parent: &File,
    name: &OsStr,
    access: u32,
    disposition: u32,
    descriptor: Option<&SecurityDescriptor>,
    case_insensitive: bool,
) -> Result<NativeOpenedDirectory, ConfigError> {
    let mut wide = wide_component(name)?;
    let byte_length = u16::try_from(wide.len().saturating_mul(2))
        .map_err(|_| ConfigError::new(ConfigErrorKind::PathEscape))?;
    wide.push(0);
    let name_string = UNICODE_STRING {
        Length: byte_length,
        MaximumLength: byte_length.saturating_add(2),
        Buffer: wide.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>()).expect("OBJECT_ATTRIBUTES fits u32"),
        RootDirectory: parent.as_raw_handle(),
        ObjectName: std::ptr::addr_of!(name_string),
        Attributes: if case_insensitive {
            OBJ_CASE_INSENSITIVE
        } else {
            0
        },
        SecurityDescriptor: descriptor.map_or(null(), |value| value.as_ptr().cast()),
        SecurityQualityOfService: null(),
    };
    let mut status_block = IO_STATUS_BLOCK::default();
    let mut handle: HANDLE = null_mut();
    // SAFETY: The object attributes borrow a live parent, one validated UTF-16
    // component, and an optional live descriptor. Success returns one owned
    // regular-file handle.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access | SYNCHRONIZE | FILE_READ_ATTRIBUTES,
            &attributes,
            &mut status_block,
            null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            disposition,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
            0,
        )
    };
    if !nt_success(status) {
        return Err(map_ntstatus(status));
    }
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(ConfigError::new(ConfigErrorKind::Io));
    }
    // SAFETY: NtCreateFile returned a fresh successful handle.
    let file = unsafe { File::from_raw_handle(handle) };
    Ok(NativeOpenedDirectory {
        file,
        created: status_block.Information == FILE_CREATED as usize,
    })
}

/// Reports the attributes of one child under its exact spelling.
pub(super) fn child_attributes(parent: &File, name: &OsStr) -> Result<Option<u32>, ConfigError> {
    let mut wide = wide_component(name)?;
    let byte_length = u16::try_from(wide.len().saturating_mul(2))
        .map_err(|_| ConfigError::new(ConfigErrorKind::PathEscape))?;
    wide.push(0);
    let name_string = UNICODE_STRING {
        Length: byte_length,
        MaximumLength: byte_length.saturating_add(2),
        Buffer: wide.as_mut_ptr(),
    };
    let mut status_block = IO_STATUS_BLOCK::default();
    // FILE_DIRECTORY_INFORMATION requires 8-byte alignment.
    let word_count = size_of::<FILE_DIRECTORY_INFORMATION>()
        .saturating_add(512)
        .div_ceil(size_of::<u64>());
    let mut storage = vec![0u64; word_count];
    let byte_length = u32::try_from(storage.len().saturating_mul(size_of::<u64>()))
        .expect("directory query buffer fits u32");
    // SAFETY: `parent` is a live directory handle; all input and output buffers
    // remain live for this synchronous query.
    let status = unsafe {
        NtQueryDirectoryFile(
            parent.as_raw_handle(),
            null_mut(),
            None,
            null(),
            &mut status_block,
            storage.as_mut_ptr().cast(),
            byte_length,
            FileDirectoryInformation,
            true,
            std::ptr::addr_of!(name_string),
            true,
        )
    };
    if status == STATUS_NO_SUCH_FILE
        || status == STATUS_OBJECT_NAME_NOT_FOUND
        || status == STATUS_NO_MORE_FILES
    {
        return Ok(None);
    }
    if !nt_success(status) {
        return Err(map_ntstatus(status));
    }
    // SAFETY: NtQueryDirectoryFile initialized the fixed header in `storage`.
    let attributes = unsafe {
        storage
            .as_ptr()
            .cast::<FILE_DIRECTORY_INFORMATION>()
            .read_unaligned()
            .FileAttributes
    };
    Ok(Some(attributes))
}

fn has_normal_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::Normal(_)))
}

fn reject_wrong_case_root(
    parent: &File,
    expected_root_name: Option<&str>,
) -> Result<(), ConfigError> {
    if let Some(expected) = expected_root_name
        && find_wrong_case_child(parent, expected)?.is_some()
    {
        return Err(ConfigError::new(ConfigErrorKind::InvalidHome));
    }
    Ok(())
}

pub(super) fn open_path_directory_follow(path: &Path, access: u32) -> Result<File, ConfigError> {
    open_path_directory_access(path, access, false, false)
}

fn open_path_directory_access(
    path: &Path,
    access: u32,
    denied_is_access_control: bool,
    nofollow: bool,
) -> Result<File, ConfigError> {
    if !path.is_absolute() {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, path));
    }
    let wide = wide_path(path)?;
    // SAFETY: `wide` is a live NUL-terminated UTF-16 path. Optional security
    // and template pointers are null as documented. FILE_FLAG_OPEN_REPARSE_POINT
    // applies only to the trailing component.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS
                | if nofollow {
                    FILE_FLAG_OPEN_REPARSE_POINT
                } else {
                    0
                },
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        let kind = if denied_is_access_control && error.kind() == io::ErrorKind::PermissionDenied {
            ConfigErrorKind::AccessControl
        } else {
            ConfigErrorKind::Io
        };
        return Err(ConfigError::for_path(kind, path).with_io_kind(error.kind()));
    }
    // SAFETY: CreateFileW returned a fresh successful handle.
    let file = unsafe { File::from_raw_handle(handle) };
    if nofollow {
        reject_reparse_or_wrong_type(&file)?;
    } else {
        reject_wrong_type(&file)?;
    }
    Ok(file)
}

fn reject_reparse_or_wrong_type(file: &File) -> Result<(), ConfigError> {
    let attributes = file_attributes(file)?;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(ConfigError::new(ConfigErrorKind::LinkEscape));
    }
    reject_non_directory(attributes)
}

fn reject_reparse_or_directory(file: &File) -> Result<(), ConfigError> {
    let attributes = file_attributes(file)?;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(ConfigError::new(ConfigErrorKind::LinkEscape));
    }
    if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::InvalidData));
    }
    Ok(())
}

fn reject_wrong_type(file: &File) -> Result<(), ConfigError> {
    reject_non_directory(file_attributes(file)?)
}

pub(super) fn file_attributes(file: &File) -> Result<u32, ConfigError> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` is live and `information` is writable output storage.
    let queried = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if queried == 0 {
        let error = io::Error::last_os_error();
        return Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind()));
    }
    Ok(information.dwFileAttributes)
}

fn reject_non_directory(attributes: u32) -> Result<(), ConfigError> {
    if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::NotADirectory)
        );
    }
    Ok(())
}

pub(super) fn find_wrong_case_child(
    directory: &File,
    expected: &str,
) -> Result<Option<OsString>, ConfigError> {
    let expected = OsStr::new(expected);
    query_directory_names(directory, |name| {
        if name != expected && ordinal_names_equal_ignore_case(&name, expected)? {
            Ok(Some(name))
        } else {
            Ok(None)
        }
    })
}

fn query_directory_names<T>(
    directory: &File,
    visitor: impl FnMut(OsString) -> Result<Option<T>, ConfigError>,
) -> Result<Option<T>, ConfigError> {
    let query = reopen_directory_for_query(directory)?;
    query_directory_names_handle(&query, visitor)
}

fn reopen_directory_for_query(directory: &File) -> Result<File, ConfigError> {
    // An empty handle-relative name reopens the directory as an independent
    // file object, so prior native search expressions cannot filter this scan.
    let opened = nt_open_directory(
        directory,
        OsStr::new(""),
        DIRECTORY_READ_ACCESS,
        FILE_OPEN,
        None,
        false,
    )?;
    reject_reparse_or_wrong_type(&opened.file)?;
    Ok(opened.file)
}

fn query_directory_names_handle<T>(
    directory: &File,
    mut visitor: impl FnMut(OsString) -> Result<Option<T>, ConfigError>,
) -> Result<Option<T>, ConfigError> {
    // FILE_FULL_DIR_INFO requires 8-byte alignment.
    let mut storage = vec![0u64; 2 * 1024];
    let mut information_class = FileFullDirectoryRestartInfo;
    let mut started = false;
    loop {
        let byte_length = storage.len().saturating_mul(size_of::<u64>());
        // SAFETY: `directory` is live and `storage` is aligned writable output
        // for FILE_FULL_DIR_INFO records.
        let queried = unsafe {
            GetFileInformationByHandleEx(
                directory.as_raw_handle(),
                information_class,
                storage.as_mut_ptr().cast(),
                u32::try_from(byte_length).expect("directory query buffer fits u32"),
            )
        };
        if queried == 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(code) if code == ERROR_NO_MORE_FILES as i32 => break,
                Some(code)
                    if code == ERROR_MORE_DATA as i32 && !started && storage.len() < 128 * 1024 =>
                {
                    storage.resize(storage.len().saturating_mul(2), 0);
                    continue;
                }
                _ => {
                    return Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind()));
                }
            }
        }
        // SAFETY: The successful query initialized bytes within `storage`.
        let bytes =
            unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), byte_length) };
        if let Some(found) = visit_directory_names(bytes, &mut visitor)? {
            return Ok(Some(found));
        }
        started = true;
        information_class = FileFullDirectoryInfo;
    }
    Ok(None)
}

pub(super) fn visit_directory_names<T>(
    bytes: &[u8],
    visitor: &mut impl FnMut(OsString) -> Result<Option<T>, ConfigError>,
) -> Result<Option<T>, ConfigError> {
    let header_length = size_of::<FILE_FULL_DIR_INFO>();
    let name_offset = offset_of!(FILE_FULL_DIR_INFO, FileName);
    let mut offset = 0usize;
    while offset < bytes.len() {
        if bytes.len().saturating_sub(offset) < header_length {
            return Err(ConfigError::new(ConfigErrorKind::Io));
        }
        let base = bytes[offset..].as_ptr();
        // SAFETY: The query wrote a FILE_FULL_DIR_INFO header at `base`.
        let information = unsafe { base.cast::<FILE_FULL_DIR_INFO>().read_unaligned() };
        let name_bytes = usize::try_from(information.FileNameLength)
            .map_err(|_| ConfigError::new(ConfigErrorKind::Io))?;
        let next = usize::try_from(information.NextEntryOffset)
            .map_err(|_| ConfigError::new(ConfigErrorKind::Io))?;
        let record_end = if next == 0 {
            bytes.len()
        } else {
            if !next.is_multiple_of(8) || next < header_length {
                return Err(ConfigError::new(ConfigErrorKind::Io));
            }
            offset
                .checked_add(next)
                .filter(|end| *end <= bytes.len())
                .ok_or_else(|| ConfigError::new(ConfigErrorKind::Io))?
        };
        let name_start = offset
            .checked_add(name_offset)
            .ok_or_else(|| ConfigError::new(ConfigErrorKind::Io))?;
        let name_end = name_start
            .checked_add(name_bytes)
            .filter(|end| *end <= record_end)
            .ok_or_else(|| ConfigError::new(ConfigErrorKind::Io))?;
        if !name_bytes.is_multiple_of(2) {
            return Err(ConfigError::new(ConfigErrorKind::Io));
        }
        let (units, remainder) = bytes[name_start..name_end].as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(ConfigError::new(ConfigErrorKind::Io));
        }
        let wide = units
            .iter()
            .map(|unit| u16::from_ne_bytes(*unit))
            .collect::<Vec<_>>();
        if let Some(found) = visitor(OsString::from_wide(&wide))? {
            return Ok(Some(found));
        }
        if next == 0 {
            break;
        }
        offset = record_end;
    }
    Ok(None)
}

fn ordinal_names_equal_ignore_case(left: &OsStr, right: &OsStr) -> Result<bool, ConfigError> {
    let left = left.encode_wide().collect::<Vec<_>>();
    let right = right.encode_wide().collect::<Vec<_>>();
    let left_length =
        i32::try_from(left.len()).map_err(|_| ConfigError::new(ConfigErrorKind::PathEscape))?;
    let right_length =
        i32::try_from(right.len()).map_err(|_| ConfigError::new(ConfigErrorKind::PathEscape))?;
    // SAFETY: Both UTF-16 buffers remain live for their explicit lengths. The
    // API reads neither a terminator nor bytes beyond those lengths.
    let compared = unsafe {
        CompareStringOrdinal(left.as_ptr(), left_length, right.as_ptr(), right_length, 1)
    };
    if compared == 0 {
        let error = io::Error::last_os_error();
        return Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind()));
    }
    Ok(compared == CSTR_EQUAL)
}

fn wide_path(path: &Path) -> Result<Vec<u16>, ConfigError> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(ConfigError::new(ConfigErrorKind::PathEscape));
    }
    wide.push(0);
    Ok(wide)
}

pub(super) fn wide_component(name: &OsStr) -> Result<Vec<u16>, ConfigError> {
    let wide: Vec<u16> = name.encode_wide().collect();
    if wide.contains(&0) {
        return Err(ConfigError::new(ConfigErrorKind::PathEscape));
    }
    Ok(wide)
}

pub(super) fn nt_success(status: NTSTATUS) -> bool {
    status >= 0
}

pub(super) fn map_ntstatus(status: NTSTATUS) -> ConfigError {
    // SAFETY: RtlNtStatusToDosError accepts every NTSTATUS and returns its
    // documented Win32 mapping without consulting GetLastError.
    let code = unsafe { RtlNtStatusToDosError(status) };
    let error = io::Error::from_raw_os_error(code as i32);
    let kind = if error.kind() == io::ErrorKind::PermissionDenied {
        ConfigErrorKind::AccessControl
    } else {
        ConfigErrorKind::Io
    };
    ConfigError::new(kind).with_io_kind(error.kind())
}
