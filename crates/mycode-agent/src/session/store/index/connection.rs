//! Opening the index database: the schema, reader and writer handles,
//! and the integer conversions the rows use.

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL,
    root_branch TEXT NOT NULL,
    event_count INTEGER NOT NULL,
    corrupt INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS branches (
    session_id TEXT NOT NULL,
    branch_id TEXT NOT NULL,
    head TEXT NOT NULL,
    event_count INTEGER NOT NULL,
    committed_bytes INTEGER NOT NULL,
    parent_kind TEXT NOT NULL,
    source_branch_id TEXT,
    at_event_id TEXT,
    to_event_id TEXT,
    PRIMARY KEY (session_id, branch_id),
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS events (
    session_id TEXT NOT NULL,
    branch_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    event_id TEXT NOT NULL,
    kind INTEGER NOT NULL,
    call_id TEXT,
    digest TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    offset INTEGER NOT NULL,
    record_len INTEGER NOT NULL,
    external INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, branch_id, seq),
    UNIQUE (session_id, branch_id, event_id),
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS events_by_id
    ON events (session_id, branch_id, event_id);
";

use super::*;
pub(super) fn open_writer(path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open(path).map_err(|_| StoreError::Storage)?;
    conn.busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|_| StoreError::Storage)?;
    conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")
        .map_err(|_| StoreError::Storage)?;
    Ok(conn)
}

pub(super) fn open_reader(path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| StoreError::Storage)?;
    conn.busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|_| StoreError::Storage)?;
    Ok(conn)
}

pub(super) fn open_connection(path: &Path) -> Result<Connection, StoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| StoreError::Storage)?;
    }
    let conn = Connection::open(path).map_err(|_| StoreError::Storage)?;
    conn.busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|_| StoreError::Storage)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;",
    )
    .map_err(|_| StoreError::Storage)?;
    conn.execute_batch(SCHEMA)
        .map_err(|_| StoreError::Storage)?;
    Ok(conn)
}

pub(super) fn table_ready(conn: &Connection) -> Result<bool, StoreError> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|_| StoreError::Storage)?
        .is_some())
}

pub(super) fn now_ms() -> Result<u64, StoreError> {
    u64::try_from(crate::unix_now().as_millis()).map_err(|_| StoreError::Storage)
}

pub(super) fn i64_from(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Limit)
}

pub(super) fn u64_from(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt)
}

pub(super) fn encode_head(head: &HeadStamp) -> String {
    match head {
        HeadStamp::Empty => "empty".to_owned(),
        HeadStamp::Event(event) => event.as_str().to_owned(),
    }
}

pub(crate) fn decode_head(value: &str) -> Option<HeadStamp> {
    if value == "empty" {
        return Some(HeadStamp::Empty);
    }
    SessionEventId::parse(value).map(HeadStamp::Event)
}

/// A short or missing log or payload file is corruption; anything else is
/// a storage failure the caller may retry.
pub(super) fn read_error(error: std::io::Error) -> StoreError {
    if matches!(
        error.kind(),
        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::NotFound
    ) {
        StoreError::Corrupt
    } else {
        StoreError::Storage
    }
}

/// Decodes one log line and checks it against its index row, loading an
/// external payload when the line points at one.
pub(super) fn verified_payload(
    paths: &SessionPaths<'_>,
    event: &StoredEvent,
    line: &[u8],
) -> Result<Vec<u8>, StoreError> {
    let decoded = jsonl::decode_line(line).map_err(|()| StoreError::Corrupt)?;
    if decoded.event_id != event.event_id.as_str()
        || decoded.record_len != event.record_len
        || decoded.digest != event.digest
        || decoded.external != event.external
        || decoded.kind != event.kind
        || decoded.bytes != event.bytes
        || decoded.call_id.as_deref() != event.call_id.as_ref().map(|id| id.as_str())
    {
        return Err(StoreError::Corrupt);
    }
    if event.external {
        let payload_path = paths
            .absolute(&paths.payload_file(&event.event_id))
            .map_err(|_| StoreError::Unavailable)?;
        let payload = fs::read_exact_file(&payload_path, event.bytes).map_err(read_error)?;
        if payload.len() as u64 != event.bytes {
            return Err(StoreError::Corrupt);
        }
        if format_digest(&payload_digest(&payload)) != event.digest {
            return Err(StoreError::Corrupt);
        }
        return Ok(payload);
    }
    if decoded.payload.len() as u64 != event.bytes {
        return Err(StoreError::Corrupt);
    }
    Ok(decoded.payload)
}
