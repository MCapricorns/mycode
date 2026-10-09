//! Windows durability barriers for owned directories.

#[path = "windows_acl.rs"]
pub(super) mod windows_acl;
#[path = "windows_file.rs"]
pub(super) mod windows_file;
#[path = "windows_open.rs"]
pub(super) mod windows_open;

use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;

use windows_sys::Wdk::Storage::FileSystem::NtFlushBuffersFileEx;
use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use crate::{ConfigError, ConfigErrorKind};

pub(super) fn flush_directory(directory: &File) -> Result<(), ConfigError> {
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: `directory` is a live synchronous directory handle opened with
    // write-data access. Parameters are absent as required for flags zero, and
    // `status_block` is writable for the duration of this native flush.
    let status = unsafe {
        NtFlushBuffersFileEx(
            directory.as_raw_handle(),
            0,
            std::ptr::null(),
            0,
            &mut status_block,
        )
    };
    if status >= 0 {
        return Ok(());
    }
    // SAFETY: RtlNtStatusToDosError accepts every NTSTATUS and does not use
    // GetLastError.
    let code = unsafe { RtlNtStatusToDosError(status) };
    classify_directory_flush_error(io::Error::from_raw_os_error(code as i32))
}

fn classify_directory_flush_error(error: io::Error) -> Result<(), ConfigError> {
    Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind()))
}
