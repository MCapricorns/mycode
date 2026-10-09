//! Fail-closed sys surface for platforms without handle-relative IO.

#[cfg(not(any(unix, windows)))]
mod sys {
    use super::*;
    pub(super) fn open_allowed_root(_: &Path) -> io::Result<File> {
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "handle-relative file IO is not implemented on this platform",
        ))
    }
    pub(super) fn open_child(_: &File, _: &OsStr, _: ChildOpen) -> io::Result<OpenedChild> {
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "handle-relative file IO is not implemented on this platform",
        ))
    }
    pub(super) fn unique_component_name(
        _: &File,
        _: FileIdentity,
        _: &CancellationToken,
    ) -> io::Result<OsString> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn current_meta(_: &File) -> io::Result<FileMeta> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn read_exact_capped(
        _: &mut File,
        _: u64,
        _: u64,
        _: &CancellationToken,
    ) -> io::Result<Vec<u8>> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn create_temp(_: &File, _: &OsStr) -> io::Result<OpenedChild> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn write_all_sync(_: &mut File, _: &[u8], _: &CancellationToken) -> io::Result<()> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn unlink_child(_: &File, _: &OsStr) -> io::Result<()> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn publish_replace(_: &File, _: &File, _: &OsStr, _: &OsStr) -> io::Result<()> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn publish_create_only(_: &File, _: &File, _: &OsStr, _: &OsStr) -> io::Result<()> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn sync_parent(_: &File) -> io::Result<()> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn ensure_directory(_: &File, _: &OsStr) -> io::Result<OpenedChild> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
}
