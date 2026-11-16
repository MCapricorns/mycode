use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::PathBuf;

use tokio_util::sync::CancellationToken;

use super::{WRITE_CHUNK, read_exact_capped, write_all_sync};

struct TemporaryPath(PathBuf);

impl Drop for TemporaryPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn with_file(bytes: &[u8], check: impl FnOnce(&mut File)) {
    let path = TemporaryPath(
        std::env::temp_dir().join(format!("mycode-file-content-{}", uuid::Uuid::new_v4())),
    );
    let mut file = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path.0)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.rewind().unwrap();
    check(&mut file);
}

#[test]
fn content_round_trips_empty_and_multiple_chunks() {
    for size in [0, WRITE_CHUNK - 1, WRITE_CHUNK, WRITE_CHUNK * 2 + 1] {
        let bytes = vec![b'x'; size];
        with_file(&[], |file| {
            let cancel = CancellationToken::new();
            write_all_sync(file, &bytes, &cancel).unwrap();
            file.rewind().unwrap();
            assert_eq!(
                read_exact_capped(file, size as u64, size as u64, &cancel).unwrap(),
                bytes,
            );
        });
    }
}

#[test]
fn a_declared_size_above_the_cap_fails_before_reading() {
    with_file(b"abc", |file| {
        let error = read_exact_capped(file, 3, 2, &CancellationToken::new()).unwrap_err();
        assert_eq!(error.to_string(), "file exceeds the read size limit");
        assert_eq!(file.stream_position().unwrap(), 0);
    });
}

#[test]
fn reads_reject_growth_and_shrinkage_from_the_declared_size() {
    with_file(b"abc", |file| {
        let cancel = CancellationToken::new();
        let error = read_exact_capped(file, 2, 4, &cancel).unwrap_err();
        assert_eq!(
            error.to_string(),
            "file grew or exceeded the read size limit"
        );
        file.rewind().unwrap();
        let error = read_exact_capped(file, 4, 4, &cancel).unwrap_err();
        assert_eq!(
            error.to_string(),
            "file size changed during read or did not match metadata",
        );
    });
}

#[test]
fn cancelled_content_io_leaves_existing_bytes_unread_and_unchanged() {
    with_file(b"original", |file| {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = read_exact_capped(file, 8, 8, &cancel).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert_eq!(file.stream_position().unwrap(), 0);
        let error = write_all_sync(file, b"replacement", &cancel).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        let mut actual = Vec::new();
        file.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, b"original");
    });
}

#[test]
fn an_empty_write_still_syncs_without_checking_cancellation() {
    with_file(b"original", |file| {
        let cancel = CancellationToken::new();
        cancel.cancel();
        write_all_sync(file, &[], &cancel).unwrap();
        assert_eq!(file.stream_position().unwrap(), 0);
        assert_eq!(file.metadata().unwrap().len(), 8);
    });
}
