//! NT handle plumbing: status mapping, component encoding, identity
//! queries, volume containment, and NtOpenFile/NtCreateFile wrappers.
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::ptr::null;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{NtCreateFile, NtOpenFile};
use windows_sys::Win32::Foundation::{
    HANDLE, INVALID_HANDLE_VALUE, NTSTATUS, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError,
    UNICODE_STRING,
};
use windows_sys::Win32::Security::SECURITY_DESCRIPTOR;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ID_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdInfo,
    GetFileInformationByHandle, GetFileInformationByHandleEx,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use crate::builtin::fs_search::validate_component_name;

use super::super::prepare::map_not_found;
use super::super::types::{FileIdentity, FileKind, FileMeta};

pub(super) fn ntstatus_error(status: NTSTATUS) -> io::Error {
    // `Nt*` calls return NTSTATUS and do not define `GetLastError`.
    // SAFETY: converting that returned status is the documented use of
    // `RtlNtStatusToDosError` and has no pointer preconditions.
    let code = unsafe { RtlNtStatusToDosError(status) };
    let code = i32::try_from(code).unwrap_or(i32::MAX);
    io::Error::from_raw_os_error(code)
}

pub(super) fn win32_error(code: u32) -> io::Error {
    let code = i32::try_from(code).unwrap_or(i32::MAX);
    io::Error::from_raw_os_error(code)
}

pub(super) fn encode_component(name: &OsStr) -> io::Result<Vec<u16>> {
    validate_component_name(name)?;
    let wide: Vec<u16> = name.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path component contains NUL",
        ));
    }
    Ok(wide)
}

pub(super) fn into_file(handle: HANDLE) -> File {
    // SAFETY: `handle` is a fresh successful NT handle and no other owner
    // will close it after this transfer.
    unsafe { File::from_raw_handle(handle) }
}

// Single canonical copy lives in `fs_search::path_util`.
pub(super) use crate::builtin::fs_search::windows_extended_length_path;

pub(super) struct HandleInfo {
    pub(super) identity: FileIdentity,
    pub(super) kind: FileKind,
    reparse: bool,
    nlink: u32,
    pub(super) attributes: u32,
    size: u64,
    mtime: i64,
}

pub(super) fn query_handle(file: &File) -> io::Result<HandleInfo> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` is live; `information` is writable documented storage.
    let success = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    let kind = if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        FileKind::Directory
    } else {
        FileKind::File
    };
    let mut id_info = FILE_ID_INFO::default();
    let id_size = u32::try_from(size_of::<FILE_ID_INFO>()).expect("FILE_ID_INFO fits in u32");
    // SAFETY: `id_info` is writable `FILE_ID_INFO` storage. ReFS uniqueness
    // requires the 128-bit `FileId`.
    let id_ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&raw mut id_info).cast(),
            id_size,
        )
    };
    if id_ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let write = information.ftLastWriteTime;
    let mtime = (i64::from(write.dwHighDateTime) << 32) | i64::from(write.dwLowDateTime);
    Ok(HandleInfo {
        identity: FileIdentity {
            volume: id_info.VolumeSerialNumber,
            file_id: id_info.FileId.Identifier,
        },
        kind,
        reparse: information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        nlink: information.nNumberOfLinks,
        attributes: information.dwFileAttributes,
        size: (u64::from(information.nFileSizeHigh) << 32) | u64::from(information.nFileSizeLow),
        mtime,
    })
}

pub(super) fn meta_from_file(file: &File, reject_reparse: bool) -> io::Result<FileMeta> {
    let info = query_handle(file)?;
    if reject_reparse && info.reparse {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reparse-point traversal is not permitted",
        ));
    }
    Ok(FileMeta {
        identity: info.identity,
        kind: info.kind,
        size: info.size,
        mtime_secs: info.mtime,
        mtime_nsecs: 0,
        nlink: u64::from(info.nlink),
        unix_mode: 0,
        unix_uid: 0,
        unix_gid: 0,
        windows_attributes: info.attributes,
    })
}

pub(super) fn enforce_same_volume(parent: &File, child: &File) -> io::Result<()> {
    let parent_info = query_handle(parent)?;
    if parent_info.kind != FileKind::Directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "walk parent is no longer a directory",
        ));
    }
    let child_info = query_handle(child)?;
    if parent_info.identity.volume != child_info.identity.volume {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount traversal is not permitted",
        ));
    }
    Ok(())
}

pub(super) fn nt_open(
    parent: &File,
    name: &OsStr,
    desired_access: u32,
    options: u32,
    case_insensitive: bool,
) -> io::Result<File> {
    let mut wide = encode_component(name)?;
    let byte_len = wide
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path component is too long"))?;
    let object_name = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: wide.as_mut_ptr(),
    };
    let object_attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>())
            .expect("OBJECT_ATTRIBUTES size must fit in u32"),
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &object_name,
        Attributes: if case_insensitive {
            OBJ_CASE_INSENSITIVE
        } else {
            0
        },
        SecurityDescriptor: null(),
        SecurityQualityOfService: null(),
    };
    let mut handle = INVALID_HANDLE_VALUE;
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: `parent` stays live; `object_name` references `wide` for this
    // call; output pointers reference initialized writable storage.
    let status = unsafe {
        NtOpenFile(
            &mut handle,
            desired_access,
            &object_attributes,
            &mut io_status,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            options,
        )
    };
    if status < 0 {
        return Err(map_not_found(ntstatus_error(status)));
    }
    Ok(into_file(handle))
}

#[expect(
    clippy::too_many_arguments,
    reason = "NT create needs parent, name, access, attributes, disposition, options, share, and optional SD together"
)]
pub(super) fn nt_create(
    parent: &File,
    name: &OsStr,
    desired_access: u32,
    attributes: u32,
    disposition: u32,
    options: u32,
    share_access: u32,
    security_descriptor: *const SECURITY_DESCRIPTOR,
) -> io::Result<File> {
    let mut wide = encode_component(name)?;
    let byte_len = wide
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path component is too long"))?;
    let object_name = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: wide.as_mut_ptr(),
    };
    let object_attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>())
            .expect("OBJECT_ATTRIBUTES size must fit in u32"),
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &object_name,
        Attributes: 0,
        SecurityDescriptor: security_descriptor,
        SecurityQualityOfService: null(),
    };
    let mut handle = INVALID_HANDLE_VALUE;
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: parent handle, name buffer, optional security descriptor, and
    // output pointers are live for the call. NTSTATUS is the return value.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            desired_access,
            &object_attributes,
            &mut io_status,
            null(),
            attributes,
            share_access,
            disposition,
            options,
            null(),
            0,
        )
    };
    if status < 0 {
        return Err(map_not_found(ntstatus_error(status)));
    }
    Ok(into_file(handle))
}
