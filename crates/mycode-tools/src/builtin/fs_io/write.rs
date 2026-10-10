//! The write kernel: temps, CAS, publish, verify, and mode/ACL preservation.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use crate::builtin::blocking::run_blocking_until;
use crate::builtin::fs_search::{SEARCH_TIME_LIMIT, validate_component_name};
use crate::builtin::process::ExecutionLease;
use crate::tool::ToolError;

use super::*;

static WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(super) fn write_lock() -> &'static Mutex<()> {
    WRITE_LOCK.get_or_init(|| Mutex::new(()))
}

fn temp_name() -> OsString {
    OsString::from(format!(
        "mycode-write-{}.tmp",
        uuid::Uuid::new_v4().as_simple()
    ))
}

struct TempName {
    parent: File,
    name: OsString,
    persist: bool,
    /// Never-written probe name (Unix mode probe / Windows inherited-DACL
    /// probe). Success paths remove it explicitly and fallibly before
    /// reporting success; [`Drop`] is only the unwind fallback.
    probe: Option<OsString>,
    /// Windows-only creation-time handle for the security probe. Copy and
    /// delete run through this handle so a restrictive inherited DACL cannot
    /// force a by-name reopen.
    #[cfg(windows)]
    probe_file: Option<File>,
    /// Windows-only creation-time duplicate holding `DELETE`; see [`Drop`].
    #[cfg(windows)]
    delete_handle: Option<File>,
}

impl TempName {
    /// Unlinks the never-written probe, disarming its cleanup only after
    /// the unlink succeeds.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the probe unlink fails.
    fn remove_probe(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            let Some(handle) = self.probe_file.as_ref() else {
                self.probe = None;
                return Ok(());
            };
            match sys::mark_delete(handle) {
                Ok(()) => {
                    self.probe_file = None;
                    self.probe = None;
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
        #[cfg(not(windows))]
        {
            let Some(probe) = self.probe.as_ref() else {
                return Ok(());
            };
            match sys::unlink_child(&self.parent, probe) {
                Ok(()) => {
                    self.probe = None;
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
    }

    /// Removes unpublished probe/payload names and reports cleanup errors.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a mandatory unlink or disposition fails.
    fn cleanup(&mut self) -> io::Result<()> {
        let mut probe_error = None;
        if self.probe.is_some()
            && let Err(error) = self.remove_probe()
        {
            probe_error = Some(error);
        }
        if self.persist {
            return match probe_error {
                Some(error) => Err(error),
                None => Ok(()),
            };
        }
        let unpublished = {
            #[cfg(windows)]
            {
                if let Some(handle) = self.delete_handle.as_ref() {
                    match sys::mark_delete(handle) {
                        Ok(()) => {
                            self.delete_handle = None;
                            Ok(())
                        }
                        Err(error) => Err(error),
                    }
                } else {
                    sys::unlink_child(&self.parent, &self.name)
                }
            }
            #[cfg(not(windows))]
            {
                sys::unlink_child(&self.parent, &self.name)
            }
        };
        match unpublished {
            Ok(()) => {
                self.persist = true;
                match probe_error {
                    Some(error) => Err(error),
                    None => Ok(()),
                }
            }
            Err(error) => {
                let message = match probe_error {
                    Some(probe) => {
                        format!("{probe}; failed to remove rejected temporary file: {error}")
                    }
                    None => format!("failed to remove rejected temporary file: {error}"),
                };
                Err(io::Error::new(error.kind(), message))
            }
        }
    }
}

fn fold_cleanup_error(primary: ToolError, cleanup: io::Result<()>) -> ToolError {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => ToolError::Execution(format!("{primary}; {cleanup}")),
    }
}

fn complete_temp<T>(mut temp: TempName, result: Result<T, ToolError>) -> Result<T, ToolError> {
    result.map_err(|primary| fold_cleanup_error(primary, temp.cleanup()))
}

impl Drop for TempName {
    fn drop(&mut self) {
        // `complete_temp` reports cleanup failures. This is only a
        // panic/unwind fallback, where Drop cannot return another error.
        let _ = self.cleanup();
    }
}

/// Writes `content` through a prepared capability.
///
/// Missing targets are create-only. Existing targets require `expected_revision`
/// or `overwrite`. Both together are rejected. This is process-local compare-
/// and-swap; cooperating writers in this process are serialized on a global
/// write mutex. Advisory locks are not claimed as protection against foreign
/// writers. The payload inode stays private from creation through the
/// publish rename; final modes are restored afterwards through the retained
/// temp handle, so a mode-restoration failure is reported after the
/// replacement already happened. A final cancel gate runs immediately before
/// the irreversible publish rename, and the published name is verified to
/// still be the just-published inode carrying exactly the written content.
/// Mandatory cleanups must succeed before success is reported: the
/// never-written mode probe, and on the Unix `linkat` create path the
/// temporary name left after the link. Residue is never silently presented
/// as a successful write.
///
/// # Errors
///
/// Returns [`ToolError`] on permission-style path failures, stale revisions,
/// cancellation (including the final pre-publish gate), publish failure,
/// verified-cleanup failure, or post-publish verification failure. Failure
/// before publish leaves the original file untouched.
pub(crate) fn write_file(
    prepared: Option<&PreparedFile>,
    cwd: &Path,
    path: &str,
    content: &[u8],
    expected_revision: Option<&str>,
    overwrite: bool,
    cancel: &CancellationToken,
) -> Result<FileWrite, ToolError> {
    if content.len() > MAX_WRITE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "write content exceeds {MAX_WRITE_BYTES} bytes"
        )));
    }
    if expected_revision.is_some() && overwrite {
        return Err(ToolError::InvalidArgs(
            "expected_revision and overwrite cannot both be set".to_owned(),
        ));
    }
    check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
    let inner = bind_prepared(prepared, cwd, path, cancel, FileAccess::ExistingOrMissing)?;
    let _guard = write_lock()
        .lock()
        .map_err(|_| ToolError::Execution("file write lock poisoned".to_owned()))?;
    check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
    match inner {
        PreparedInner::Existing {
            parent,
            file,
            name,
            meta,
            key,
        } => write_existing(
            parent,
            file,
            name,
            meta,
            key,
            content,
            expected_revision,
            overwrite,
            cancel,
        ),
        PreparedInner::Missing {
            parent,
            remaining,
            key,
            parent_identity,
        } => {
            if expected_revision.is_some() {
                return Err(ToolError::InvalidArgs(
                    "expected_revision cannot be used when the target does not exist".to_owned(),
                ));
            }
            write_missing(parent, remaining, key, parent_identity, content, cancel)
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "existing write needs parent, target, spelling, metadata, and CAS inputs together"
)]
fn write_existing(
    parent: File,
    existing: File,
    name: OsString,
    prepared_meta: FileMeta,
    key: String,
    content: &[u8],
    expected_revision: Option<&str>,
    overwrite: bool,
    cancel: &CancellationToken,
) -> Result<FileWrite, ToolError> {
    let listed = sys::open_child(&parent, &name, ChildOpen::ExistingFile).map_err(|error| {
        ToolError::Execution(format!("file identity changed before write: {error}"))
    })?;
    if listed.meta.identity != prepared_meta.identity {
        return Err(ToolError::Execution(
            "file identity changed before write".to_owned(),
        ));
    }
    drop(listed);
    let live = sys::current_meta(&existing)
        .map_err(|error| ToolError::Execution(format!("failed to stat {key}: {error}")))?;
    if live.identity != prepared_meta.identity {
        return Err(ToolError::Execution(
            "file identity changed before write".to_owned(),
        ));
    }
    if !overwrite {
        let expected = expected_revision.ok_or_else(|| {
            ToolError::InvalidArgs(
                "refusing to overwrite an existing file without expected_revision or overwrite=true"
                    .to_owned(),
            )
        })?;
        let mut reader = existing
            .try_clone()
            .map_err(|error| ToolError::Execution(format!("failed to reopen {key}: {error}")))?;
        let raw = sys::read_exact_capped(&mut reader, live.size, MAX_READ_SCAN_BYTES, cancel)
            .map_err(|error| ToolError::Execution(format!("failed to hash {key}: {error}")))?;
        let current = revision_token(&live, &content_hash(&raw));
        if current.as_str() != expected {
            return Err(ToolError::Execution(
                "stale expected_revision; re-read the file and retry or pass overwrite=true"
                    .to_owned(),
            ));
        }
    }
    let detached_hardlink = live.nlink > 1;
    if detached_hardlink {
        let proven =
            sys::unique_component_name(&parent, live.identity, cancel).map_err(|error| {
                ToolError::Execution(format!("cannot prove hardlink path: {error}"))
            })?;
        if proven != name {
            return Err(ToolError::Execution(
                "hardlink path cannot be uniquely proven".to_owned(),
            ));
        }
    }
    let (created, mut temp) = create_temp_in(&parent, cancel)?;
    #[cfg_attr(
        not(windows),
        expect(unused_mut, reason = "Windows drops the source handle before publish")
    )]
    let mut existing = Some(existing);
    let result = (|| {
        let temp_identity = created.meta.identity;
        let expected_hash = content_hash(content);
        let temp_file = write_temp(created.file, content, cancel)?;
        // Windows copies the source DACL and attributes (including a possible
        // read-only bit) onto the temp before publish so cleanup mirrors the
        // source; Unix keeps the payload private through the rename and
        // restores the source mode/owner afterwards on the retained handle.
        #[cfg(windows)]
        {
            preserve_existing(
                &live,
                existing.as_ref().expect("existing handle"),
                &temp_file,
            )?;
            drop(existing.take());
        }
        // All irreversible pre-publish work is complete. The final cancel gate
        // runs immediately before the rename so a cancelled or timed-out call
        // can never publish while its supervisor reports cancellation.
        check_cancel(cancel).map_err(|error| {
            ToolError::Execution(format!("cancelled before publishing {key}: {error}"))
        })?;
        sys::publish_replace(&parent, &temp_file, &temp.name, &name)
            .map_err(|error| ToolError::Execution(format!("failed to publish {key}: {error}")))?;
        temp.persist = true;
        #[cfg(unix)]
        preserve_existing(
            &live,
            existing.as_ref().expect("existing handle"),
            &temp_file,
        )?;
        sys::sync_parent(&parent).map_err(|error| {
            ToolError::Execution(format!("failed to sync parent of {key}: {error}"))
        })?;
        close_share_denying_handles(&mut temp, temp_file);
        finish_write(
            &parent,
            &name,
            temp_identity,
            expected_hash,
            content.len(),
            key,
            detached_hardlink,
            cancel,
        )
    })();
    complete_temp(temp, result)
}

fn write_missing(
    mut parent: File,
    remaining: Vec<OsString>,
    key: String,
    parent_identity: FileIdentity,
    content: &[u8],
    cancel: &CancellationToken,
) -> Result<FileWrite, ToolError> {
    let live_parent = sys::current_meta(&parent).map_err(|error| {
        ToolError::Execution(format!("failed to stat parent of {key}: {error}"))
    })?;
    if live_parent.identity != parent_identity {
        return Err(ToolError::Execution(
            "parent directory identity changed before write".to_owned(),
        ));
    }
    if remaining.is_empty() {
        return Err(ToolError::InvalidArgs(
            "path must name a file inside the session cwd".to_owned(),
        ));
    }
    let dest = remaining.last().cloned().expect("remaining is non-empty");
    for dir_name in &remaining[..remaining.len() - 1] {
        check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
        validate_component_name(dir_name)
            .map_err(|error| ToolError::InvalidArgs(error.to_string()))?;
        let opened = sys::ensure_directory(&parent, dir_name).map_err(|error| {
            ToolError::Execution(format!("failed to create directory: {error}"))
        })?;
        parent = opened.file;
    }
    validate_component_name(&dest).map_err(|error| ToolError::InvalidArgs(error.to_string()))?;
    if sys::open_child(&parent, &dest, ChildOpen::Probe).is_ok() {
        return Err(ToolError::Execution(
            "refusing to overwrite a file that appeared after create-only prepare".to_owned(),
        ));
    }
    let (created, mut temp) = create_temp_in(&parent, cancel)?;
    let result = (|| {
        let temp_identity = created.meta.identity;
        let expected_hash = content_hash(content);
        // The payload inode stays private from creation through the rename; a
        // separate never-written probe learns the umask/default-ACL effective
        // mode and is unlinked by the guard on every path.
        #[cfg(unix)]
        let new_file_mode = attach_mode_probe(&parent, &mut temp, cancel)?;
        #[cfg(windows)]
        attach_security_probe(&parent, &mut temp, cancel)?;
        // The deterministic test observer opens every inode visible to a
        // foreign reader before the payload receives any content.
        let temp_file = write_temp(created.file, content, cancel)?;
        // All pre-publish work is complete; the final cancel gate runs
        // immediately before the irreversible publish rename.
        check_cancel(cancel).map_err(|error| {
            ToolError::Execution(format!("cancelled before publishing {key}: {error}"))
        })?;
        sys::publish_create_only(&parent, &temp_file, &temp.name, &dest)
            .map_err(|error| ToolError::Execution(format!("failed to create {key}: {error}")))?;
        temp.persist = true;
        #[cfg(unix)]
        sys::apply_new_file_mode(&temp_file, new_file_mode).map_err(|error| {
            ToolError::Execution(format!("failed to set new file mode: {error}"))
        })?;
        #[cfg(windows)]
        apply_probed_security(&temp, &temp_file)?;
        sys::sync_parent(&parent).map_err(|error| {
            ToolError::Execution(format!("failed to sync parent of {key}: {error}"))
        })?;
        temp.remove_probe().map_err(|error| {
            ToolError::Execution(format!(
                "failed to remove the mode probe after publishing {key}: {error}"
            ))
        })?;
        close_share_denying_handles(&mut temp, temp_file);
        finish_write(
            &parent,
            &dest,
            temp_identity,
            expected_hash,
            content.len(),
            key,
            false,
            cancel,
        )
    })();
    complete_temp(temp, result)
}

fn create_temp_in(
    parent: &File,
    cancel: &CancellationToken,
) -> Result<(OpenedChild, TempName), ToolError> {
    // Clone before any named create. Once `sys::create_temp` succeeds, the
    // guard can be assembled without another fallible operation.
    let retained_parent = parent.try_clone().map_err(|error| {
        ToolError::Execution(format!("failed to retain parent handle: {error}"))
    })?;
    for _ in 0..TEMP_ATTEMPTS {
        check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
        let name = temp_name();
        match sys::create_temp(parent, &name) {
            Ok(opened) => {
                // The Windows DELETE duplicate moves into the guard; the
                // writer keeps the file handle and creation metadata.
                return Ok((
                    OpenedChild {
                        file: opened.file,
                        meta: opened.meta,
                        #[cfg(windows)]
                        delete_handle: None,
                    },
                    TempName {
                        parent: retained_parent,
                        name,
                        persist: false,
                        probe: None,
                        #[cfg(windows)]
                        probe_file: None,
                        #[cfg(windows)]
                        delete_handle: opened.delete_handle,
                    },
                ));
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ToolError::Execution(format!(
                    "failed to create temporary file: {error}"
                )));
            }
        }
    }
    Err(ToolError::Execution(
        "failed to create an exclusive temporary file".to_owned(),
    ))
}

/// Writes `bytes` to the private temp file, flushes, and synchronizes it.
///
/// Mode transitions are deliberately not performed here: the payload inode
/// stays private (`0600` on Unix) from creation until after the publish
/// rename, so no window exposes written content at the temp name. Callers
/// restore final modes on the retained handle after a successful publish.
///
/// # Errors
///
/// Returns [`ToolError`] when writing, flushing, syncing, or a cancel check
/// fails.
fn write_temp(mut file: File, bytes: &[u8], cancel: &CancellationToken) -> Result<File, ToolError> {
    sys::write_all_sync(&mut file, bytes, cancel).map_err(|error| {
        if error.kind() == ErrorKind::Interrupted {
            ToolError::Execution(error.to_string())
        } else {
            ToolError::Execution(format!("failed to write temporary file: {error}"))
        }
    })?;
    Ok(file)
}

/// Creates the never-written `0666` mode probe next to the payload temp.
///
/// The kernel applies the process umask and any parent default ACL to the
/// probe, so its recorded mode is exactly what a plain `0666` create in
/// that directory yields; it is applied to the published payload through
/// its retained handle after the rename. The probe never receives payload
/// bytes, so a foreign observer holding it can only ever read an empty
/// file. The linked name is recorded on `temp`; success paths remove it
/// explicitly and report a failed removal, while [`TempName::drop`] is the
/// best-effort fallback for failure paths.
///
/// # Errors
///
/// Returns [`ToolError`] when the exclusive probe create keeps colliding,
/// fails, or the call is cancelled.
#[cfg(unix)]
fn attach_mode_probe(
    parent: &File,
    temp: &mut TempName,
    cancel: &CancellationToken,
) -> Result<u32, ToolError> {
    for _ in 0..TEMP_ATTEMPTS {
        check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
        let name = temp_name();
        match sys::create_mode_probe(parent, &name) {
            Ok(mode) => {
                temp.probe = Some(name);
                return Ok(mode);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ToolError::Execution(format!(
                    "failed to probe the effective new file mode: {error}"
                )));
            }
        }
    }
    Err(ToolError::Execution(
        "failed to create an exclusive mode probe file".to_owned(),
    ))
}

/// Creates a never-written probe that inherits the parent directory DACL.
///
/// # Errors
///
/// Returns [`ToolError`] when exclusive create keeps colliding, fails, or
/// the call is cancelled.
#[cfg(windows)]
fn attach_security_probe(
    parent: &File,
    temp: &mut TempName,
    cancel: &CancellationToken,
) -> Result<(), ToolError> {
    for _ in 0..TEMP_ATTEMPTS {
        check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
        let name = temp_name();
        match windows::create_security_probe(parent, &name) {
            Ok(file) => {
                temp.probe = Some(name);
                temp.probe_file = Some(file);
                return Ok(());
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ToolError::Execution(format!(
                    "failed to probe the inherited file security: {error}"
                )));
            }
        }
    }
    Err(ToolError::Execution(
        "failed to create an exclusive security probe file".to_owned(),
    ))
}

/// Copies the probe's inherited DACL/owner onto the just-published payload.
#[cfg(windows)]
fn apply_probed_security(temp: &TempName, dst: &File) -> Result<(), ToolError> {
    let Some(probe) = temp.probe_file.as_ref() else {
        return Ok(());
    };
    let meta = windows::current_meta(probe).map_err(|error| {
        ToolError::Execution(format!("failed to stat the security probe: {error}"))
    })?;
    windows::copy_safe_mode(&meta, probe, dst).map_err(|error| {
        ToolError::Execution(format!(
            "failed to restore inherited file security: {error}"
        ))
    })
}

/// Closes handles whose share mode would block post-publish reopen-by-name.
fn close_share_denying_handles(temp: &mut TempName, temp_file: File) {
    #[cfg(windows)]
    drop(temp.delete_handle.take());
    #[cfg(not(windows))]
    let _ = temp;
    drop(temp_file);
}

/// Copies the preserved permission state of an existing target onto `dst`.
///
/// Windows runs this before publish (the temp then mirrors the source's
/// DACL/attributes, which cleanup must survive); Unix runs it after publish
/// through the retained temp handle so the payload is never exposed with
/// the source's readable mode before the rename.
///
/// # Errors
///
/// Returns [`ToolError`] when the mode/owner or DACL/attribute copy fails.
fn preserve_existing(meta: &FileMeta, src: &File, dst: &File) -> Result<(), ToolError> {
    #[cfg(unix)]
    {
        let _ = src;
        unix::copy_safe_mode(meta, dst)
            .map_err(|error| ToolError::Execution(format!("failed to preserve file mode: {error}")))
    }
    #[cfg(windows)]
    {
        windows::copy_safe_mode(meta, src, dst).map_err(|error| {
            ToolError::Execution(format!("failed to preserve DACL or attributes: {error}"))
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (meta, src, dst);
        Err(ToolError::Execution(
            "file write is not implemented on this platform".to_owned(),
        ))
    }
}

/// Verifies the just-published target and derives its revision.
///
/// The reopened name must still resolve to the published temp inode
/// (`published_identity`, recorded at temp creation) and its content must
/// hash to `expected_hash`, so a foreign replacement of the published
/// name — even with same-length content — is reported as a failure instead
/// of generating a revision for content this write never wrote.
///
/// # Errors
///
/// Returns [`ToolError`] when the call is cancelled, the published name
/// cannot be reopened, the reopened inode is not the published temp, or
/// its size or content hash does not match the written content.
#[expect(
    clippy::too_many_arguments,
    reason = "verification needs the parent, published name and identity, expected hash and size, key, and CAS result together"
)]
fn finish_write(
    parent: &File,
    name: &OsStr,
    published_identity: FileIdentity,
    expected_hash: [u8; 32],
    bytes_written: usize,
    key: String,
    detached_hardlink: bool,
    cancel: &CancellationToken,
) -> Result<FileWrite, ToolError> {
    check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
    let mut published = sys::open_child(parent, name, ChildOpen::ExistingFile)
        .map_err(|error| ToolError::Execution(format!("failed to reopen {key}: {error}")))?;
    if published.meta.identity != published_identity {
        return Err(ToolError::Execution(
            "published file was replaced before verification".to_owned(),
        ));
    }
    let raw = sys::read_exact_capped(
        &mut published.file,
        published.meta.size,
        MAX_READ_SCAN_BYTES.max(bytes_written as u64),
        cancel,
    )
    .map_err(|error| ToolError::Execution(format!("failed to hash written file: {error}")))?;
    if raw.len() != bytes_written {
        return Err(ToolError::Execution(
            "published file size does not match written content".to_owned(),
        ));
    }
    if content_hash(&raw) != expected_hash {
        return Err(ToolError::Execution(
            "published file content does not match written content".to_owned(),
        ));
    }
    let revision = revision_token(&published.meta, &content_hash(&raw));
    Ok(FileWrite {
        bytes_written,
        revision,
        detached_hardlink,
        path_key: key,
    })
}

/// Writes on the cancellable supervisor while holding `lease`.
///
/// Publication always takes a real [`ExecutionLease`]. Dropping the caller
/// future does not release `lease` while the worker may still publish.
///
/// # Errors
///
/// Same as [`write_file`].
#[expect(
    clippy::too_many_arguments,
    reason = "lease rides with the existing write arguments onto one worker"
)]
pub(crate) async fn write_file_with_lease(
    prepared: Option<std::sync::Arc<PreparedFile>>,
    cwd: PathBuf,
    path: String,
    content: String,
    expected_revision: Option<String>,
    overwrite: bool,
    lease: ExecutionLease,
    cancel: CancellationToken,
) -> Result<FileWrite, ToolError> {
    let deadline = Instant::now() + SEARCH_TIME_LIMIT;
    run_blocking_until("file write", &cancel, deadline, move |worker_cancel| {
        let _lease = lease;
        write_file(
            prepared.as_deref(),
            &cwd,
            &path,
            content.as_bytes(),
            expected_revision.as_deref(),
            overwrite,
            &worker_cancel,
        )
    })
    .await
}
