//! Windows handle-relative opens, NT identity queries, and path snapshots.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::*;

/// Last on-disk path component of a Windows handle opened by alias.
///
/// `GetFinalPathNameByHandleW` returns the NTFS long name with stored case,
/// so a user-typed `visible` or `VISIBL~1` becomes `Visible` for ignore
/// matching. The last component is taken from the UTF-16 handle path, not
/// `Path::file_name`, which can strip trailing dots and spaces. Unix keeps
/// the opened byte spelling.
#[cfg(windows)]
pub(crate) fn on_disk_component_name(handle: &StableHandle) -> io::Result<OsString> {
    last_wide_component(handle.final_path.as_os_str()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "opened object has no on-disk file name",
        )
    })
}

/// Last `\`/`/`-separated UTF-16 component, preserving stored spelling.
#[cfg(windows)]
pub(crate) fn last_wide_component(path: &OsStr) -> Option<OsString> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    let wide: Vec<u16> = path.encode_wide().collect();
    let start = wide
        .iter()
        .rposition(|&unit| unit == u16::from(b'\\') || unit == u16::from(b'/'))
        .map_or(0, |index| index + 1);
    let last = wide.get(start..)?;
    if last.is_empty() || last == [u16::from(b'.')] || last == [u16::from(b'.'), u16::from(b'.')] {
        return None;
    }
    Some(OsString::from_wide(last))
}

#[cfg(windows)]
pub(crate) fn open_windows_parent_directory(dir: &File) -> io::Result<ParentDirectory> {
    let path = final_path_by_handle(dir)?;
    let Some(parent_path) = windows_git_parent_path(&path) else {
        return Ok(ParentDirectory::FilesystemRoot);
    };
    let Some(child_name) = last_wide_component(path.as_os_str()) else {
        return Ok(ParentDirectory::FilesystemRoot);
    };
    let parent = open_windows_path_nofollow(parent_path)?;
    let reopened = open_named_windows(
        &parent,
        &child_name,
        Some(FsEntryKind::Directory),
        NameMatch::Exact,
        SearchAccess::Content,
    )?;
    let want = identity_and_kind(dir)?.0;
    if reopened.identity != want {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "parent directory no longer contains the opened child",
        ));
    }
    Ok(ParentDirectory::Parent(parent))
}

#[cfg(windows)]
pub(crate) fn windows_git_parent_path(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() || parent == path {
        return None;
    }
    if parent
        .components()
        .all(|component| matches!(component, Component::Prefix(_)))
    {
        return None;
    }
    Some(parent)
}

#[cfg(windows)]
pub(crate) fn open_windows_path_nofollow(path: &Path) -> io::Result<File> {
    let mut components = path.components();
    let Some(first) = components.next() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "parent path is empty",
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
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "parent path is not normalized",
                ));
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
                    "parent path has an interior prefix",
                ));
            }
        }
    }
    Ok(current)
}

#[cfg(windows)]
pub(crate) fn windows_child_name_in_parent(parent: &File, child: &File) -> io::Result<OsString> {
    let want = identity_and_kind(child)?.0;
    let child_path = final_path_by_handle(child)?;
    let name = last_wide_component(child_path.as_os_str()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "parent directory has no on-disk name",
        )
    })?;
    let reopened = open_named_windows(
        parent,
        &name,
        Some(FsEntryKind::Directory),
        NameMatch::Exact,
        SearchAccess::Content,
    )?;
    if reopened.identity != want {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "parent directory no longer contains the opened child",
        ));
    }
    Ok(name)
}

#[cfg(windows)]
pub(crate) fn open_windows(path: &Path) -> io::Result<StableHandle> {
    open_windows_create(path, 0)
}

#[cfg(windows)]
pub(crate) fn open_windows_prefix(path: &Path) -> io::Result<StableHandle> {
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    open_windows_create(path, FILE_FLAG_OPEN_REPARSE_POINT)
}

#[cfg(windows)]
pub(crate) fn open_windows_create(path: &Path, extra_flags: u32) -> io::Result<StableHandle> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let path = windows_extended_length_path(path);
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    wide.push(0);
    // SAFETY: `wide` is a live NUL-terminated UTF-16 path. Null optional
    // pointers satisfy `CreateFileW`; a successful handle is transferred
    // immediately into `File` ownership below.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | extra_flags,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is a fresh successful `CreateFileW` result and no
    // other owner will close it after this transfer.
    let file = unsafe { File::from_raw_handle(handle) };
    stable_from_windows_file(file, false, false)
}

#[cfg(windows)]
pub(crate) fn open_named_windows(
    parent: &File,
    name: &OsStr,
    expected: Option<FsEntryKind>,
    name_match: NameMatch,
    access: SearchAccess,
) -> io::Result<StableHandle> {
    validate_component_name(name)?;
    open_windows_component(parent, name, expected, name_match, access)
}

#[cfg(windows)]
pub(crate) fn open_windows_component(
    parent: &File,
    name: &OsStr,
    expected: Option<FsEntryKind>,
    name_match: NameMatch,
    access: SearchAccess,
) -> io::Result<StableHandle> {
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::ptr::null;
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT,
        FILE_SYNCHRONOUS_IO_NONALERT, NtOpenFile,
    };
    use windows_sys::Win32::Foundation::{
        INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_GENERIC_READ, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let mut wide: Vec<u16> = name.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path component contains NUL",
        ));
    }
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
        Attributes: match name_match {
            NameMatch::Alias => OBJ_CASE_INSENSITIVE,
            NameMatch::Exact => 0,
        },
        SecurityDescriptor: null(),
        SecurityQualityOfService: null(),
    };
    let mut options = FILE_OPEN_REPARSE_POINT;
    match expected {
        Some(FsEntryKind::File) => options |= FILE_NON_DIRECTORY_FILE,
        Some(FsEntryKind::Directory) => options |= FILE_DIRECTORY_FILE,
        None => {}
    }
    let mut handle = INVALID_HANDLE_VALUE;
    let mut io_status = IO_STATUS_BLOCK::default();
    // `FILE_SYNCHRONOUS_IO_NONALERT` requires `SYNCHRONIZE`. Metadata opens
    // omit both so a `FILE_READ_ATTRIBUTES`-only ACL still confirms, and the
    // resulting asynchronous handle is never used for content reads.
    let desired_access = match access {
        SearchAccess::Content => {
            options |= FILE_SYNCHRONOUS_IO_NONALERT;
            FILE_GENERIC_READ
        }
        SearchAccess::Metadata => FILE_READ_ATTRIBUTES,
    };
    // SAFETY: `parent` stays live; `object_name` references `wide` for this
    // call; all output pointers reference initialized writable storage.
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
        // `NtOpenFile` returns an NTSTATUS and does not define `GetLastError`.
        // SAFETY: converting that returned status is the documented use of
        // `RtlNtStatusToDosError` and has no pointer preconditions.
        let code = unsafe { RtlNtStatusToDosError(status) };
        let code = i32::try_from(code)
            .map_err(|_| io::Error::other(format!("unrepresentable Windows error code: {code}")))?;
        return Err(io::Error::from_raw_os_error(code));
    }
    // SAFETY: successful `NtOpenFile` returned a fresh owned handle and no
    // other owner will close it after this transfer.
    let file = unsafe { File::from_raw_handle(handle) };
    let opened = stable_from_windows_file(file, true, access == SearchAccess::Metadata)?;
    enforce_windows_child_containment(parent, &opened)?;
    Ok(opened)
}

#[cfg(windows)]
pub(crate) fn stable_from_windows_file(
    file: File,
    reject_reparse: bool,
    metadata_only: bool,
) -> io::Result<StableHandle> {
    let (identity, kind, reparse, links) = windows_identity_kind_reparse(&file)?;
    if reject_reparse && reparse {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reparse-point traversal is not permitted",
        ));
    }
    if kind == FsEntryKind::File && links != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "multi-link regular files are not permitted",
        ));
    }
    let final_path = final_path_by_handle(&file)?;
    Ok(StableHandle {
        file,
        identity,
        kind,
        final_path,
        metadata_only,
    })
}

#[cfg(windows)]
pub(crate) fn enforce_windows_child_containment(
    parent: &File,
    child: &StableHandle,
) -> io::Result<()> {
    let (parent_identity, parent_kind, _, _) = windows_identity_kind_reparse(parent)?;
    if parent_kind != FsEntryKind::Directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "walk parent is no longer a directory",
        ));
    }
    let child_volume = child.identity.volume;
    if parent_identity.volume != child_volume {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount traversal is not permitted",
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn identity_and_kind(file: &File) -> io::Result<(FileIdentity, FsEntryKind)> {
    let (identity, kind, _, _) = windows_identity_kind_reparse(file)?;
    Ok((identity, kind))
}

#[cfg(windows)]
pub(crate) fn windows_file_is_hidden(file: &File) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_HIDDEN, GetFileInformationByHandle,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is borrowed from a live `File` and `information`
    // points to initialized writable storage of the documented type.
    let success = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(information.dwFileAttributes & FILE_ATTRIBUTE_HIDDEN != 0)
}

#[cfg(windows)]
pub(crate) fn windows_identity_kind_reparse(
    file: &File,
) -> io::Result<(FileIdentity, FsEntryKind, bool, u32)> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is borrowed from a live `File` and `information`
    // points to initialized writable storage of the documented type.
    let success = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    let kind = if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        FsEntryKind::Directory
    } else {
        FsEntryKind::File
    };
    let mut id_info = FILE_ID_INFO::default();
    let id_size = u32::try_from(size_of::<FILE_ID_INFO>()).expect("FILE_ID_INFO fits in u32");
    // SAFETY: `file` is live; `id_info` is writable `FILE_ID_INFO` storage.
    // ReFS uniqueness requires the 128-bit `FileId`; the 64-bit
    // `BY_HANDLE_FILE_INFORMATION` index is not used as identity.
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
    Ok((
        FileIdentity {
            volume: id_info.VolumeSerialNumber,
            file_id: id_info.FileId.Identifier,
        },
        kind,
        information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        information.nNumberOfLinks,
    ))
}

#[cfg(windows)]
pub(crate) fn final_path_by_handle(file: &File) -> io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use std::ptr::null_mut;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };

    let flags = FILE_NAME_NORMALIZED | VOLUME_NAME_DOS;
    // SAFETY: the handle is borrowed from a live `File`; a null zero-sized
    // output buffer is the documented size-query form.
    let needed = unsafe { GetFinalPathNameByHandleW(file.as_raw_handle(), null_mut(), 0, flags) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u16; needed as usize + 1];
    loop {
        let capacity = u32::try_from(buffer.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "final path is too long"))?;
        // SAFETY: `buffer` is live writable UTF-16 storage with the exact
        // capacity passed to the API, and the file handle remains valid.
        let written = unsafe {
            GetFinalPathNameByHandleW(file.as_raw_handle(), buffer.as_mut_ptr(), capacity, flags)
        };
        if written == 0 {
            return Err(io::Error::last_os_error());
        }
        if written < capacity {
            buffer.truncate(written as usize);
            let path = PathBuf::from(OsString::from_wide(&buffer));
            return Ok(strip_verbatim_prefix(&path));
        }
        buffer.resize(written as usize + 1, 0);
    }
}
