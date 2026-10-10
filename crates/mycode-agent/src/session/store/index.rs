//! SQLite index for session titles, branch heads, and JSONL byte offsets.
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use mycode_config::HomeLayout;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::super::digest::{format_digest, payload_digest};
use super::super::dto::{BranchMutationKind, EventKind, HeadStamp};
use super::super::fs;
use super::super::ids::{BranchId, SessionCallId, SessionEventId, SessionId};
use super::jsonl::{self, charge};
use super::{MAX_SESSION_TOTAL_BYTES, SessionPaths, database_path};

const SCHEMA: &str = "
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

/// Why an index or JSONL operation failed.
#[derive(Debug)]
pub(crate) enum StoreError {
    /// The filesystem or SQLite substrate failed.
    Storage,
    /// Durable bytes failed validation.
    Corrupt,
    /// The session or branch row is absent.
    NotFound,
    /// A fixed byte or count bound was hit.
    Limit,
    /// The path or publication could not be used.
    Unavailable,
    /// An expected-head compare-and-swap lost.
    Conflict(HeadStamp),
}

/// One branch row returned by a listing query.
#[derive(Clone, Debug)]
pub(crate) struct ListedBranch {
    /// Branch identity spelling.
    pub(crate) branch_id: String,
    /// `empty` or an event identity.
    pub(crate) head: String,
    /// Committed events on this branch.
    pub(crate) event_count: u64,
}

/// One session row returned by a listing query, newest first.
#[derive(Clone, Debug)]
pub(crate) struct ListedSession {
    /// Session identity spelling.
    pub(crate) id: String,
    /// Sidebar title, possibly empty.
    pub(crate) title: String,
    /// Sum of committed events across branches.
    pub(crate) event_count: u64,
    /// The row was stored but failed validation.
    pub(crate) corrupt: bool,
    /// Branches in branch-id order.
    pub(crate) branches: Vec<ListedBranch>,
}

/// One event row loaded from the offset index.
#[derive(Clone, Debug)]
pub(crate) struct StoredEvent {
    /// Event identity.
    pub(crate) event_id: SessionEventId,
    /// Canonical payload digest.
    pub(crate) digest: String,
    /// Payload byte length.
    pub(crate) bytes: u64,
    /// Event classification.
    pub(crate) kind: EventKind,
    /// Present only for tool kinds.
    pub(crate) call_id: Option<SessionCallId>,
    /// Byte offset of the JSONL line.
    pub(crate) offset: u64,
    /// JSONL line length, newline included.
    pub(crate) record_len: u64,
    /// Payload lives in `payloads/` rather than on the line.
    pub(crate) external: bool,
}

/// One branch restored from the index.
#[derive(Clone, Debug)]
pub(crate) struct StoredBranch {
    /// Branch identity.
    pub(crate) branch_id: BranchId,
    /// Committed head.
    pub(crate) head: HeadStamp,
    /// Committed JSONL prefix length.
    pub(crate) committed_bytes: u64,
    /// Offset rows in ledger order.
    pub(crate) events: Vec<StoredEvent>,
}

/// Fields of one append commit.
pub(crate) struct AppendRequest<'a> {
    /// Session receiving the event.
    pub(crate) session: &'a SessionId,
    /// Branch receiving the event.
    pub(crate) branch: &'a BranchId,
    /// Head the caller and the index must still name.
    pub(crate) expected_head: &'a HeadStamp,
    /// Committed event count the index must still name.
    pub(crate) expected_count: u64,
    /// Committed JSONL length the index must still name.
    pub(crate) expected_committed: u64,
    /// Reserved event identity.
    pub(crate) event_id: &'a SessionEventId,
    /// Event classification.
    pub(crate) kind: EventKind,
    /// Call identity for tool kinds.
    pub(crate) call_id: Option<&'a SessionCallId>,
    /// Raw payload bytes.
    pub(crate) payload: &'a [u8],
    /// Canonical digest spelling.
    pub(crate) digest: &'a str,
}

/// Byte coordinates of one committed append.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AppendedLocation {
    /// Offset of the new line.
    pub(crate) offset: u64,
    /// Line length including the newline.
    pub(crate) record_len: u64,
    /// Bytes charged against the session cap.
    pub(crate) charge: u64,
    /// Whether the payload was externalized.
    pub(crate) external: bool,
}

/// Fields of one fork or rewind commit.
pub(crate) struct BranchCommitRequest<'a> {
    /// Session receiving the branch.
    pub(crate) session: &'a SessionId,
    /// Branch whose prefix is copied.
    pub(crate) source: &'a BranchId,
    /// Branch being created.
    pub(crate) new_branch: &'a BranchId,
    /// Fork or rewind.
    pub(crate) kind: BranchMutationKind,
    /// Source head the index must still name.
    pub(crate) expected_head: &'a HeadStamp,
    /// Source event count the index must still name.
    pub(crate) expected_count: u64,
    /// Source JSONL length the index must still name.
    pub(crate) expected_committed: u64,
    /// Prefix boundary event.
    pub(crate) target_event: &'a SessionEventId,
    /// Zero-based index of the boundary event.
    pub(crate) position: u64,
    /// JSONL prefix length to copy.
    pub(crate) prefix_end: u64,
}

/// The single writer connection owned by the session actor.
pub(crate) struct SessionStore {
    home: HomeLayout,
    conn: Connection,
}

impl SessionStore {
    /// Opens or creates the index in WAL mode.
    pub(crate) fn open(home: &HomeLayout) -> Result<Self, StoreError> {
        let path = database_path(home).map_err(|_| StoreError::Unavailable)?;
        let conn = open_connection(&path, true)?;
        Ok(Self {
            home: home.clone(),
            conn,
        })
    }

    /// Inserts an empty root session. A duplicate id is [`StoreError::Unavailable`].
    pub(crate) fn create_session(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
    ) -> Result<(), StoreError> {
        let now = now_ms()?;
        let tx = self.conn.transaction().map_err(|_| StoreError::Storage)?;
        let exists = tx
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![session.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|_| StoreError::Storage)?
            .is_some();
        if exists {
            return Err(StoreError::Unavailable);
        }
        tx.execute(
            "INSERT INTO sessions (id, title, updated_at, root_branch, event_count, corrupt)
             VALUES (?1, '', ?2, ?3, 0, 0)",
            params![session.as_str(), i64_from(now)?, branch.as_str()],
        )
        .map_err(|_| StoreError::Storage)?;
        tx.execute(
            "INSERT INTO branches (
                session_id, branch_id, head, event_count, committed_bytes,
                parent_kind, source_branch_id, at_event_id, to_event_id
             ) VALUES (?1, ?2, 'empty', 0, 0, 'root', NULL, NULL, NULL)",
            params![session.as_str(), branch.as_str()],
        )
        .map_err(|_| StoreError::Storage)?;
        tx.commit().map_err(|_| StoreError::Storage)?;
        let paths = SessionPaths::new(&self.home, session);
        let dir = paths
            .absolute(&paths.session_dir())
            .map_err(|_| StoreError::Unavailable)?;
        std::fs::create_dir_all(dir).map_err(|_| StoreError::Storage)?;
        Ok(())
    }

    /// Appends one JSONL line and commits the index under expected-head CAS.
    pub(crate) fn append_event(
        &mut self,
        request: &AppendRequest<'_>,
    ) -> Result<AppendedLocation, StoreError> {
        let paths = SessionPaths::new(&self.home, request.session);
        let log_path = paths
            .absolute(&paths.branch_jsonl(request.branch))
            .map_err(|_| StoreError::Unavailable)?;
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| StoreError::Storage)?;
        }
        match fs::file_len(&log_path) {
            Ok(length) if length > request.expected_committed => {
                fs::truncate(&log_path, request.expected_committed)
                    .map_err(|_| StoreError::Storage)?;
            }
            Ok(length) if length < request.expected_committed => {
                return Err(StoreError::Corrupt);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if request.expected_committed > 0 {
                    return Err(StoreError::Corrupt);
                }
            }
            Err(_) => return Err(StoreError::Storage),
        }
        let encoded = jsonl::encode_line(
            request.event_id,
            request.kind,
            request.call_id,
            request.payload,
            request.digest,
            now_ms()?,
        )
        .map_err(|()| StoreError::Limit)?;
        let record_len = u64::try_from(encoded.line.len()).map_err(|_| StoreError::Limit)?;
        let payload_len = u64::try_from(request.payload.len()).map_err(|_| StoreError::Limit)?;
        let event_charge =
            charge(record_len, encoded.external, payload_len).ok_or(StoreError::Limit)?;
        if event_charge > MAX_SESSION_TOTAL_BYTES {
            return Err(StoreError::Limit);
        }
        let payload_path = if encoded.external {
            let payload_path = paths
                .absolute(&paths.payload_file(request.event_id))
                .map_err(|_| StoreError::Unavailable)?;
            if let Some(parent) = payload_path.parent() {
                std::fs::create_dir_all(parent).map_err(|_| StoreError::Storage)?;
            }
            fs::create_exclusive(&payload_path, request.payload)
                .map_err(|_| StoreError::Storage)?;
            Some(payload_path)
        } else {
            None
        };
        if fs::append(&log_path, &encoded.line).is_err() {
            if let Some(path) = &payload_path {
                fs::remove(path);
            }
            return Err(StoreError::Storage);
        }

        let committed = self.commit_append(request, record_len, payload_len, encoded.external);
        if committed.is_err()
            && let Some(path) = payload_path
        {
            fs::remove(&path);
        }
        committed
    }

    fn commit_append(
        &mut self,
        request: &AppendRequest<'_>,
        record_len: u64,
        payload_len: u64,
        external: bool,
    ) -> Result<AppendedLocation, StoreError> {
        let offset = request.expected_committed;
        let new_committed = offset.checked_add(record_len).ok_or(StoreError::Limit)?;
        let new_count = request
            .expected_count
            .checked_add(1)
            .ok_or(StoreError::Limit)?;
        let title = jsonl::message_title(request.kind, request.payload).unwrap_or_default();
        let expected_head = encode_head(request.expected_head);
        let event_charge = charge(record_len, external, payload_len).ok_or(StoreError::Limit)?;
        let tx = self.conn.transaction().map_err(|_| StoreError::Storage)?;
        let changed = tx
            .execute(
                "UPDATE branches
                 SET head = ?1, event_count = ?2, committed_bytes = ?3
                 WHERE session_id = ?4 AND branch_id = ?5
                   AND head = ?6 AND event_count = ?7 AND committed_bytes = ?8",
                params![
                    request.event_id.as_str(),
                    i64_from(new_count)?,
                    i64_from(new_committed)?,
                    request.session.as_str(),
                    request.branch.as_str(),
                    expected_head,
                    i64_from(request.expected_count)?,
                    i64_from(request.expected_committed)?,
                ],
            )
            .map_err(|_| StoreError::Storage)?;
        if changed != 1 {
            let actual = tx
                .query_row(
                    "SELECT head FROM branches WHERE session_id = ?1 AND branch_id = ?2",
                    params![request.session.as_str(), request.branch.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|_| StoreError::Storage)?;
            let head = actual.as_deref().and_then(decode_head);
            return Err(match head {
                Some(head) => StoreError::Conflict(head),
                None => StoreError::Corrupt,
            });
        }
        tx.execute(
            "INSERT INTO events (
                session_id, branch_id, seq, event_id, kind, call_id, digest, bytes,
                offset, record_len, external
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                request.session.as_str(),
                request.branch.as_str(),
                i64_from(request.expected_count)?,
                request.event_id.as_str(),
                i64::from(request.kind.tag()),
                request.call_id.map(SessionCallId::as_str),
                request.digest,
                i64_from(payload_len)?,
                i64_from(offset)?,
                i64_from(record_len)?,
                i64::from(external),
            ],
        )
        .map_err(|_| StoreError::Storage)?;
        tx.execute(
            "UPDATE sessions
             SET event_count = event_count + 1,
                 updated_at = ?1,
                 title = CASE WHEN title = '' AND ?2 <> '' THEN ?2 ELSE title END
             WHERE id = ?3",
            params![i64_from(now_ms()?)?, title, request.session.as_str()],
        )
        .map_err(|_| StoreError::Storage)?;
        tx.commit().map_err(|_| StoreError::Storage)?;
        Ok(AppendedLocation {
            offset,
            record_len,
            charge: event_charge,
            external,
        })
    }

    /// Copies a JSONL prefix onto a new branch and commits it under source-head CAS.
    ///
    /// Returns the bytes charged for the copied prefix.
    pub(crate) fn commit_branch(
        &mut self,
        request: &BranchCommitRequest<'_>,
    ) -> Result<u64, StoreError> {
        let paths = SessionPaths::new(&self.home, request.session);
        let source_log = paths
            .absolute(&paths.branch_jsonl(request.source))
            .map_err(|_| StoreError::Unavailable)?;
        let new_log = paths
            .absolute(&paths.branch_jsonl(request.new_branch))
            .map_err(|_| StoreError::Unavailable)?;
        if request.prefix_end > 0 {
            fs::copy_prefix(&source_log, &new_log, request.prefix_end)
                .map_err(|_| StoreError::Storage)?;
        }
        let committed = (|| {
            let (parent_kind, at_event, to_event) = match request.kind {
                BranchMutationKind::Fork => ("fork", Some(request.target_event.as_str()), None),
                BranchMutationKind::Rewind => ("rewind", None, Some(request.target_event.as_str())),
            };
            let new_count = request.position.checked_add(1).ok_or(StoreError::Limit)?;
            let tx = self.conn.transaction().map_err(|_| StoreError::Storage)?;
            let row = tx
                .query_row(
                    "SELECT head, event_count, committed_bytes
                     FROM branches WHERE session_id = ?1 AND branch_id = ?2",
                    params![request.session.as_str(), request.source.as_str()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(|_| StoreError::Storage)?;
            let Some((head, count, bytes)) = row else {
                return Err(StoreError::Corrupt);
            };
            if head != encode_head(request.expected_head)
                || u64_from(count)? != request.expected_count
                || u64_from(bytes)? != request.expected_committed
            {
                return Err(StoreError::Conflict(
                    decode_head(&head).unwrap_or(HeadStamp::Empty),
                ));
            }
            let duplicate = tx
                .query_row(
                    "SELECT 1 FROM branches WHERE session_id = ?1 AND branch_id = ?2",
                    params![request.session.as_str(), request.new_branch.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(|_| StoreError::Storage)?
                .is_some();
            if duplicate {
                return Err(StoreError::Corrupt);
            }
            tx.execute(
                "INSERT INTO branches (
                    session_id, branch_id, head, event_count, committed_bytes,
                    parent_kind, source_branch_id, at_event_id, to_event_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    request.session.as_str(),
                    request.new_branch.as_str(),
                    request.target_event.as_str(),
                    i64_from(new_count)?,
                    i64_from(request.prefix_end)?,
                    parent_kind,
                    request.source.as_str(),
                    at_event,
                    to_event,
                ],
            )
            .map_err(|_| StoreError::Storage)?;
            tx.execute(
                "INSERT INTO events (
                    session_id, branch_id, seq, event_id, kind, call_id, digest, bytes,
                    offset, record_len, external
                 )
                 SELECT session_id, ?1, seq, event_id, kind, call_id, digest, bytes,
                        offset, record_len, external
                 FROM events
                 WHERE session_id = ?2 AND branch_id = ?3 AND seq <= ?4",
                params![
                    request.new_branch.as_str(),
                    request.session.as_str(),
                    request.source.as_str(),
                    i64_from(request.position)?,
                ],
            )
            .map_err(|_| StoreError::Storage)?;
            let copied: i64 = tx
                .query_row(
                    "SELECT COALESCE(SUM(record_len + CASE WHEN external = 1 THEN bytes ELSE 0 END), 0)
                     FROM events
                     WHERE session_id = ?1 AND branch_id = ?2 AND seq <= ?3",
                    params![
                        request.session.as_str(),
                        request.new_branch.as_str(),
                        i64_from(request.position)?,
                    ],
                    |row| row.get(0),
                )
                .map_err(|_| StoreError::Storage)?;
            tx.execute(
                "UPDATE sessions
                 SET event_count = event_count + ?1, updated_at = ?2
                 WHERE id = ?3",
                params![
                    i64_from(new_count)?,
                    i64_from(now_ms()?)?,
                    request.session.as_str()
                ],
            )
            .map_err(|_| StoreError::Storage)?;
            tx.commit().map_err(|_| StoreError::Storage)?;
            u64_from(copied)
        })();
        if committed.is_err() {
            fs::remove(&new_log);
        }
        committed
    }

    /// Loads one session's offset index and drops torn JSONL tails.
    pub(crate) fn load_session(
        &self,
        session: &SessionId,
    ) -> Result<Vec<StoredBranch>, StoreError> {
        let present = self.session_exists(session)?;
        if !present {
            return Err(StoreError::NotFound);
        }
        let corrupt: i64 = self
            .conn
            .query_row(
                "SELECT corrupt FROM sessions WHERE id = ?1",
                params![session.as_str()],
                |row| row.get(0),
            )
            .map_err(|_| StoreError::Storage)?;
        if corrupt != 0 {
            return Err(StoreError::Corrupt);
        }
        let mut statement = self
            .conn
            .prepare(
                "SELECT branch_id, head, event_count, committed_bytes
                 FROM branches WHERE session_id = ?1 ORDER BY branch_id",
            )
            .map_err(|_| StoreError::Storage)?;
        let rows = statement
            .query_map(params![session.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|_| StoreError::Storage)?;
        let mut plans = Vec::new();
        for row in rows {
            let (branch_id, head, count, bytes) = row.map_err(|_| StoreError::Storage)?;
            plans.push((branch_id, head, u64_from(count)?, u64_from(bytes)?));
        }
        drop(statement);
        if plans.is_empty() {
            return Err(StoreError::Corrupt);
        }
        let mut branches = Vec::with_capacity(plans.len());
        for (branch_id, head, event_count, committed_bytes) in plans {
            let branch_id = BranchId::parse(&branch_id).ok_or(StoreError::Corrupt)?;
            let head = decode_head(&head).ok_or(StoreError::Corrupt)?;
            self.discard_torn_tail(session, &branch_id, committed_bytes)?;
            let events = self.load_events(session, &branch_id)?;
            if events.len() as u64 != event_count {
                return Err(StoreError::Corrupt);
            }
            match (&head, events.last()) {
                (HeadStamp::Empty, None) => {}
                (HeadStamp::Event(expected), Some(last)) if &last.event_id == expected => {}
                _ => return Err(StoreError::Corrupt),
            }
            branches.push(StoredBranch {
                branch_id,
                head,
                committed_bytes,
                events,
            });
        }
        Ok(branches)
    }

    /// Reads one digest-checked payload by its index offset.
    pub(crate) fn read_payload(
        &self,
        session: &SessionId,
        branch: &BranchId,
        event: &StoredEvent,
    ) -> Result<Vec<u8>, StoreError> {
        let paths = SessionPaths::new(&self.home, session);
        let log_path = paths
            .absolute(&paths.branch_jsonl(branch))
            .map_err(|_| StoreError::Unavailable)?;
        let bytes = fs::read_range(&log_path, event.offset, event.record_len)
            .map_err(|_| StoreError::Storage)?;
        let decoded = jsonl::decode_line(&bytes).map_err(|()| StoreError::Corrupt)?;
        if decoded.event_id != event.event_id.as_str()
            || decoded.record_len != event.record_len
            || decoded.digest != event.digest
            || decoded.external != event.external
            || decoded.kind != event.kind
        {
            return Err(StoreError::Corrupt);
        }
        if event.external {
            let payload_path = paths
                .absolute(&paths.payload_file(&event.event_id))
                .map_err(|_| StoreError::Unavailable)?;
            let payload = fs::read_range(&payload_path, 0, event.bytes).map_err(|error| {
                if error.kind() == std::io::ErrorKind::UnexpectedEof {
                    StoreError::Corrupt
                } else {
                    StoreError::Storage
                }
            })?;
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

    fn load_events(
        &self,
        session: &SessionId,
        branch: &BranchId,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT event_id, kind, call_id, digest, bytes, offset, record_len, external
                 FROM events
                 WHERE session_id = ?1 AND branch_id = ?2
                 ORDER BY seq",
            )
            .map_err(|_| StoreError::Storage)?;
        let rows = statement
            .query_map(params![session.as_str(), branch.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })
            .map_err(|_| StoreError::Storage)?;
        let mut events = Vec::new();
        for row in rows {
            let (event_id, kind, call_id, digest, bytes, offset, record_len, external) =
                row.map_err(|_| StoreError::Storage)?;
            let event_id = SessionEventId::parse(&event_id).ok_or(StoreError::Corrupt)?;
            let kind = EventKind::from_tag(u8::try_from(kind).map_err(|_| StoreError::Corrupt)?)
                .ok_or(StoreError::Corrupt)?;
            let call_id = match call_id {
                Some(call) => Some(SessionCallId::parse(&call).ok_or(StoreError::Corrupt)?),
                None => None,
            };
            let tool = matches!(kind, EventKind::ToolCall | EventKind::ToolResult);
            if tool != call_id.is_some() {
                return Err(StoreError::Corrupt);
            }
            events.push(StoredEvent {
                event_id,
                digest,
                bytes: u64_from(bytes)?,
                kind,
                call_id,
                offset: u64_from(offset)?,
                record_len: u64_from(record_len)?,
                external: external != 0,
            });
        }
        Ok(events)
    }

    fn discard_torn_tail(
        &self,
        session: &SessionId,
        branch: &BranchId,
        committed_bytes: u64,
    ) -> Result<(), StoreError> {
        let paths = SessionPaths::new(&self.home, session);
        let log_path = paths
            .absolute(&paths.branch_jsonl(branch))
            .map_err(|_| StoreError::Unavailable)?;
        match fs::file_len(&log_path) {
            Ok(length) if length > committed_bytes => {
                fs::truncate(&log_path, committed_bytes).map_err(|_| StoreError::Storage)
            }
            Ok(length) if length < committed_bytes => Err(StoreError::Corrupt),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if committed_bytes == 0 {
                    Ok(())
                } else {
                    Err(StoreError::Corrupt)
                }
            }
            Err(_) => Err(StoreError::Storage),
        }
    }

    fn session_exists(&self, session: &SessionId) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![session.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|_| StoreError::Storage)?
            .is_some())
    }
}

/// Lists sessions from the index only. A missing database is an empty list.
///
/// Directories left behind by older builds are not scanned.
pub(crate) fn list_sessions(home: &HomeLayout) -> Result<Vec<ListedSession>, StoreError> {
    let path = database_path(home).map_err(|_| StoreError::Unavailable)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    // Read-only. The writer connection already holds the index; opening a
    // second connection and flipping `journal_mode` takes an exclusive lock
    // and surfaces as "the session service is unavailable" while a turn runs.
    let conn = open_reader(&path)?;
    if !table_ready(&conn)? {
        return Ok(Vec::new());
    }
    let mut sessions = conn
        .prepare(
            "SELECT id, title, event_count, corrupt
             FROM sessions
             ORDER BY updated_at DESC, id ASC",
        )
        .map_err(|_| StoreError::Storage)?;
    let session_rows = sessions
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|_| StoreError::Storage)?;
    let mut listed = Vec::new();
    for row in session_rows {
        let (id, title, event_count, corrupt) = row.map_err(|_| StoreError::Storage)?;
        listed.push(ListedSession {
            id,
            title,
            event_count: u64_from(event_count)?,
            corrupt: corrupt != 0,
            branches: Vec::new(),
        });
    }
    drop(sessions);
    let mut branches = conn
        .prepare(
            "SELECT session_id, branch_id, head, event_count
             FROM branches
             ORDER BY session_id, branch_id",
        )
        .map_err(|_| StoreError::Storage)?;
    let branch_rows = branches
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|_| StoreError::Storage)?;
    let mut by_session: BTreeMap<String, Vec<ListedBranch>> = BTreeMap::new();
    for row in branch_rows {
        let (session_id, branch_id, head, event_count) = row.map_err(|_| StoreError::Storage)?;
        by_session
            .entry(session_id)
            .or_default()
            .push(ListedBranch {
                branch_id,
                head,
                event_count: u64_from(event_count)?,
            });
    }
    for session in &mut listed {
        if let Some(branches) = by_session.remove(&session.id) {
            session.branches = branches;
        }
    }
    Ok(listed)
}

/// Removes one session's index rows. A missing database is success.
pub(crate) fn delete_indexed_session(
    home: &HomeLayout,
    session_id: &str,
) -> Result<(), StoreError> {
    if SessionId::parse(session_id).is_none() {
        return Ok(());
    }
    let path = database_path(home).map_err(|_| StoreError::Unavailable)?;
    if !path.exists() {
        return Ok(());
    }
    let conn = open_connection(&path, false)?;
    if !table_ready(&conn)? {
        return Ok(());
    }
    conn.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])
        .map_err(|_| StoreError::Storage)?;
    Ok(())
}

/// Indexes JSONL logs written by an import when the session is not already indexed.
///
/// Returns whether a new index row was inserted. Invalid ids and sessions
/// already present are left untouched.
pub(crate) fn index_jsonl_session(home: &HomeLayout, session_id: &str) -> Result<bool, StoreError> {
    let Some(session) = SessionId::parse(session_id) else {
        return Ok(false);
    };
    let mut store = SessionStore::open(home)?;
    if store.session_exists(&session)? {
        return Ok(false);
    }
    let paths = SessionPaths::new(home, &session);
    let dir = paths
        .absolute(&paths.session_dir())
        .map_err(|_| StoreError::Unavailable)?;
    let mut logs = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(StoreError::Storage),
    };
    for entry in entries {
        let entry = entry.map_err(|_| StoreError::Storage)?;
        if !entry
            .file_type()
            .map_err(|_| StoreError::Storage)?
            .is_file()
        {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(stem) = name.strip_suffix(super::JSONL_SUFFIX) else {
            continue;
        };
        let Some(branch) = BranchId::parse(stem) else {
            return Err(StoreError::Corrupt);
        };
        logs.push((branch, entry.path()));
    }
    if logs.is_empty() {
        return Ok(false);
    }
    logs.sort_by(|left, right| left.0.cmp(&right.0));
    let mut scanned = Vec::new();
    let mut title = String::new();
    let mut total_events: u64 = 0;
    for (branch, path) in &logs {
        let events = scan_jsonl(path)?;
        if title.is_empty() {
            for event in &events {
                if let Some(next) = jsonl::message_title(event.kind, &event.payload_for_title) {
                    title = next;
                    break;
                }
            }
        }
        total_events = total_events
            .checked_add(events.len() as u64)
            .ok_or(StoreError::Limit)?;
        scanned.push((branch.clone(), events));
    }
    let root = scanned[0].0.as_str();
    let now = now_ms()?;
    let tx = store.conn.transaction().map_err(|_| StoreError::Storage)?;
    tx.execute(
        "INSERT INTO sessions (id, title, updated_at, root_branch, event_count, corrupt)
         VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        params![
            session.as_str(),
            title,
            i64_from(now)?,
            root,
            i64_from(total_events)?
        ],
    )
    .map_err(|_| StoreError::Storage)?;
    for (branch, events) in &scanned {
        let head = match events.last() {
            None => "empty".to_owned(),
            Some(event) => event.event_id.clone(),
        };
        let committed = events.last().map(|event| event.end).unwrap_or(0);
        tx.execute(
            "INSERT INTO branches (
                session_id, branch_id, head, event_count, committed_bytes,
                parent_kind, source_branch_id, at_event_id, to_event_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'root', NULL, NULL, NULL)",
            params![
                session.as_str(),
                branch.as_str(),
                head,
                i64_from(events.len() as u64)?,
                i64_from(committed)?,
            ],
        )
        .map_err(|_| StoreError::Storage)?;
        for (seq, event) in events.iter().enumerate() {
            tx.execute(
                "INSERT INTO events (
                    session_id, branch_id, seq, event_id, kind, call_id, digest, bytes,
                    offset, record_len, external
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    session.as_str(),
                    branch.as_str(),
                    i64_from(seq as u64)?,
                    event.event_id,
                    i64::from(event.kind.tag()),
                    event.call_id.clone(),
                    event.digest,
                    i64_from(event.bytes)?,
                    i64_from(event.offset)?,
                    i64_from(event.record_len)?,
                    i64::from(event.external),
                ],
            )
            .map_err(|_| StoreError::Storage)?;
        }
    }
    tx.commit().map_err(|_| StoreError::Storage)?;
    Ok(true)
}

struct ScannedEvent {
    event_id: String,
    kind: EventKind,
    call_id: Option<String>,
    digest: String,
    bytes: u64,
    offset: u64,
    record_len: u64,
    external: bool,
    end: u64,
    payload_for_title: Vec<u8>,
}

fn scan_jsonl(path: &Path) -> Result<Vec<ScannedEvent>, StoreError> {
    let file = std::fs::File::open(path).map_err(|_| StoreError::Storage)?;
    let mut reader = std::io::BufReader::new(file);
    let mut offset = 0_u64;
    let mut events = Vec::new();
    loop {
        let mut line = Vec::new();
        let read = {
            use std::io::BufRead as _;
            reader
                .read_until(b'\n', &mut line)
                .map_err(|_| StoreError::Storage)?
        };
        if read == 0 {
            break;
        }
        if !line.ends_with(b"\n") {
            break;
        }
        let record_len = u64::try_from(line.len()).map_err(|_| StoreError::Corrupt)?;
        let decoded = jsonl::decode_line(&line).map_err(|()| StoreError::Corrupt)?;
        let payload_for_title = if decoded.external {
            Vec::new()
        } else {
            decoded.payload
        };
        let end = offset.checked_add(record_len).ok_or(StoreError::Corrupt)?;
        events.push(ScannedEvent {
            event_id: decoded.event_id,
            kind: decoded.kind,
            call_id: decoded.call_id,
            digest: decoded.digest,
            bytes: decoded.bytes,
            offset,
            record_len,
            external: decoded.external,
            end,
            payload_for_title,
        });
        offset = end;
    }
    Ok(events)
}

fn open_reader(path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| StoreError::Storage)?;
    conn.busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|_| StoreError::Storage)?;
    Ok(conn)
}

fn open_connection(path: &Path, create_schema: bool) -> Result<Connection, StoreError> {
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
    if create_schema {
        conn.execute_batch(SCHEMA)
            .map_err(|_| StoreError::Storage)?;
    }
    Ok(conn)
}

fn table_ready(conn: &Connection) -> Result<bool, StoreError> {
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

fn now_ms() -> Result<u64, StoreError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::Storage)?
        .as_millis();
    u64::try_from(millis).map_err(|_| StoreError::Storage)
}

fn i64_from(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Limit)
}

fn u64_from(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt)
}

fn encode_head(head: &HeadStamp) -> String {
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
