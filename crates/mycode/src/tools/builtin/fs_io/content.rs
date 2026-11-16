//! Bounded, cancellable content IO shared by the platform file kernels.
use std::fs::File;
use std::io::{self, Read, Write};

use tokio_util::sync::CancellationToken;

use super::{WRITE_CHUNK, check_cancel, sys};

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
    for chunk in bytes.chunks(WRITE_CHUNK) {
        check_cancel(cancel)?;
        file.write_all(chunk)?;
    }
    file.flush()?;
    sys::sync_file(file)
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
