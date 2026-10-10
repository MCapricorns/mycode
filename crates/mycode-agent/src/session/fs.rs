//! Blocking owned-file primitives for the session ledger store.
//!
//! Every function operates on absolute paths that the caller validated
//! through [`mycode_config::HomeLayout::owned_join`]. New files are created
//! exclusively, made private on Unix, and fsynced; on Windows they inherit
//! the protected DACL of the hardened parent directory. Directory fsync is
//! applied where the platform supports it.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;

/// Chunk size for bounded copies and reads.
const CHUNK_BYTES: u64 = 1024 * 1024;

fn private_append_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
}

fn private_create_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
}

/// Durably appends bytes to one owned file, creating it when absent.
pub(crate) fn append(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    let created = !path.exists();
    let mut file = private_append_options().open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    if created {
        sync_parent(path)?;
    }
    Ok(created)
}

/// Reads one exact byte range from one owned file.
pub(crate) fn read_range(path: &Path, offset: u64, length: u64) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let length = usize::try_from(length).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "range length exceeds addressable bytes",
        )
    })?;
    let mut buffer = vec![0_u8; length];
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

/// Returns one file's current length in bytes.
pub(crate) fn file_len(path: &Path) -> io::Result<u64> {
    Ok(File::open(path)?.metadata()?.len())
}

/// Truncates one owned file to a shorter durable length.
pub(crate) fn truncate(path: &Path, length: u64) -> io::Result<()> {
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(length)?;
    file.sync_all()?;
    Ok(())
}

/// Creates one exclusive file with the given bytes and fsyncs it.
pub(crate) fn create_exclusive(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = private_create_options().open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    sync_parent(path)
}

/// Copies one bounded prefix of a file into one exclusive new file.
pub(crate) fn copy_prefix(source: &Path, destination: &Path, length: u64) -> io::Result<()> {
    let mut reader = File::open(source)?;
    let mut writer = private_create_options().open(destination)?;
    let mut remaining = length;
    let mut chunk = vec![0_u8; CHUNK_BYTES as usize];
    while remaining > 0 {
        let want = remaining.min(CHUNK_BYTES) as usize;
        reader.read_exact(&mut chunk[..want])?;
        writer.write_all(&chunk[..want])?;
        remaining -= want as u64;
    }
    writer.sync_all()?;
    drop(writer);
    sync_parent(destination)
}

/// Best-effort removal of one owned file; recovery also removes orphans.
pub(crate) fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Fsyncs a directory entry where the platform supports it.
pub(crate) fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
