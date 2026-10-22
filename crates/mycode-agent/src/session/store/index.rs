//! SQLite index for session titles, branch heads, and JSONL byte offsets.
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use mycode_config::HomeLayout;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::super::digest::{format_digest, payload_digest};
use super::super::dto::{BranchMutationKind, EventKind, HeadStamp};
use super::super::fs;
use super::super::ids::{BranchId, SessionCallId, SessionEventId, SessionId};
use super::jsonl::{self, charge};
use super::{MAX_SESSION_TOTAL_BYTES, SessionPaths, database_path};

/// `LIKE` pattern of session ids from before the `sess-<timestamp>` format.
const LEGACY_SESSION_ID_PATTERN: &str = "ses1-%";

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
    /// Last durable mutation time in Unix epoch milliseconds (`None` when
    /// invalid or corrupt).
    pub(crate) updated_at_ms: Option<u64>,
    /// Sum of committed events across branches.
    pub(crate) event_count: u64,
    /// The row was stored but failed validation.
    pub(crate) corrupt: bool,
    /// The branch the session's writes last went to — the branch a reopen
    /// should show. Written on every append and branch commit; still the
    /// original root for sessions that predates the pointer.
    pub(crate) root_branch: String,
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

/// One session restored from the index: every branch plus the pointer the
/// session's durable writes last went to.
#[derive(Clone, Debug)]
pub(crate) struct LoadedSession {
    /// Branches in branch-id order.
    pub(crate) branches: Vec<StoredBranch>,
    /// `sessions.root_branch` when it names a loaded branch.
    pub(crate) current: Option<BranchId>,
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
    /// Session directories already found or created, so each write does not
    /// list `sessions/` again.
    dirs: RefCell<HashMap<SessionId, String>>,
}

impl SessionStore {
    /// Opens or creates the index in WAL mode.
    pub(crate) fn open(home: &HomeLayout) -> Result<Self, StoreError> {
        let path = database_path(home).map_err(|_| StoreError::Unavailable)?;
        let conn = open_connection(&path, true)?;
        // Rows from before session ids carried their creation time can no
        // longer be opened or deleted by id; drop them (branches and events
        // cascade). Their old directories are left on disk untouched.
        conn.execute(
            "DELETE FROM sessions WHERE id LIKE ?1",
            params![LEGACY_SESSION_ID_PATTERN],
        )
        .map_err(|_| StoreError::Storage)?;
        Ok(Self {
            home: home.clone(),
            conn,
            dirs: RefCell::new(HashMap::new()),
        })
    }

    /// Paths below `session`'s directory.
    fn paths(&self, session: &SessionId) -> Result<SessionPaths<'_>, StoreError> {
        if let Some(dir) = self.dirs.borrow().get(session) {
            return Ok(SessionPaths::at(&self.home, dir.clone()));
        }
        let paths = SessionPaths::locate(&self.home, session)?;
        self.dirs
            .borrow_mut()
            .insert(session.clone(), paths.session_dir());
        Ok(paths)
    }

    /// Inserts an empty root session whose directory is `dir`, a relative
    /// owned path ([`mycode_config::new_session_dir`]). A duplicate id is
    /// [`StoreError::Unavailable`].
    pub(crate) fn create_session(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
        dir: &str,
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
        let paths = SessionPaths::at(&self.home, dir.to_owned());
        let absolute = paths
            .absolute(&paths.session_dir())
            .map_err(|_| StoreError::Unavailable)?;
        std::fs::create_dir_all(absolute).map_err(|_| StoreError::Storage)?;
        self.dirs
            .borrow_mut()
            .insert(session.clone(), dir.to_owned());
        Ok(())
    }

    /// Appends one JSONL line and commits the index under expected-head CAS.
    pub(crate) fn append_event(
        &mut self,
        request: &AppendRequest<'_>,
    ) -> Result<AppendedLocation, StoreError> {
        let paths = self.paths(request.session)?;
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

        let title = jsonl::message_title(request.kind, request.payload).unwrap_or_default();
        let committed = self.commit_append(
            request,
            record_len,
            payload_len,
            encoded.external,
            event_charge,
            &title,
        );
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
        event_charge: u64,
        title: &str,
    ) -> Result<AppendedLocation, StoreError> {
        let offset = request.expected_committed;
        let new_committed = offset.checked_add(record_len).ok_or(StoreError::Limit)?;
        let new_count = request
            .expected_count
            .checked_add(1)
            .ok_or(StoreError::Limit)?;
        let expected_head = encode_head(request.expected_head);
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
        // `root_branch` tracks the branch that received the last durable
        // write, so a reopen lands where the conversation actually is —
        // not on the smallest branch id, which a rewind leaves stale.
        tx.execute(
            "UPDATE sessions
             SET event_count = event_count + 1,
                 updated_at = ?1,
                 title = CASE WHEN title = '' AND ?2 <> '' THEN ?2 ELSE title END,
                 root_branch = ?4
             WHERE id = ?3",
            params![
                i64_from(now_ms()?)?,
                title,
                request.session.as_str(),
                request.branch.as_str()
            ],
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
        let paths = self.paths(request.session)?;
        let source_log = paths
            .absolute(&paths.branch_jsonl(request.source))
            .map_err(|_| StoreError::Unavailable)?;
        let new_log = paths
            .absolute(&paths.branch_jsonl(request.new_branch))
            .map_err(|_| StoreError::Unavailable)?;
        match fs::file_len(&source_log) {
            Ok(length) if length < request.expected_committed || length < request.prefix_end => {
                return Err(StoreError::Corrupt);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if request.expected_committed > 0 || request.prefix_end > 0 {
                    return Err(StoreError::Corrupt);
                }
            }
            Err(_) => return Err(StoreError::Storage),
        }
        if request.prefix_end > 0 {
            // A failed copy can leave a truncated `new_log`; drop it like a
            // failed index commit does so no orphan file survives.
            if let Err(error) = fs::copy_prefix(&source_log, &new_log, request.prefix_end) {
                fs::remove(&new_log);
                return Err(if error.kind() == std::io::ErrorKind::UnexpectedEof {
                    StoreError::Corrupt
                } else {
                    StoreError::Storage
                });
            }
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
                 SET event_count = event_count + ?1, updated_at = ?2,
                     root_branch = ?4
                 WHERE id = ?3",
                params![
                    i64_from(new_count)?,
                    i64_from(now_ms()?)?,
                    request.session.as_str(),
                    request.new_branch.as_str()
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
    /// The returned branch pointer names the branch the session's writes
    /// last went to; `None` when the stored row does not resolve to a
    /// loaded branch (callers fall back to the smallest id).
    pub(crate) fn load_session(&self, session: &SessionId) -> Result<LoadedSession, StoreError> {
        let present = self.session_exists(session)?;
        if !present {
            return Err(StoreError::NotFound);
        }
        let (corrupt, root_branch): (i64, String) = self
            .conn
            .query_row(
                "SELECT corrupt, root_branch FROM sessions WHERE id = ?1",
                params![session.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| StoreError::Storage)?;
        if corrupt != 0 {
            return Err(StoreError::Corrupt);
        }
        let root_branch = BranchId::parse(&root_branch);
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
        if plans.is_empty() || plans.len() > super::super::dto::MAX_BRANCHES {
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
        // The pointer only counts when it names a loaded branch; a stale
        // one falls back to the caller's smallest-id pick.
        let current =
            root_branch.filter(|root| branches.iter().any(|branch| &branch.branch_id == root));
        Ok(LoadedSession { branches, current })
    }

    /// Reads one digest-checked payload by its index offset.
    pub(crate) fn read_payload(
        &self,
        session: &SessionId,
        branch: &BranchId,
        event: &StoredEvent,
    ) -> Result<Vec<u8>, StoreError> {
        let mut payloads = self.read_payloads(session, branch, std::slice::from_ref(event))?;
        payloads.pop().ok_or(StoreError::Corrupt)
    }

    /// Reads digest-checked payloads for consecutive events of one branch.
    ///
    /// Consecutive events sit back to back in the branch log, so the whole
    /// span is read with one open instead of one per event. Events that are
    /// not contiguous are read one at a time.
    pub(crate) fn read_payloads(
        &self,
        session: &SessionId,
        branch: &BranchId,
        events: &[StoredEvent],
    ) -> Result<Vec<Vec<u8>>, StoreError> {
        let Some(first) = events.first() else {
            return Ok(Vec::new());
        };
        let paths = self.paths(session)?;
        let log_path = paths
            .absolute(&paths.branch_jsonl(branch))
            .map_err(|_| StoreError::Unavailable)?;
        let mut end = first.offset;
        let mut contiguous = true;
        for event in events {
            if event.offset != end {
                contiguous = false;
                break;
            }
            end = end
                .checked_add(event.record_len)
                .ok_or(StoreError::Corrupt)?;
        }
        if !contiguous {
            return events
                .iter()
                .map(|event| {
                    let line = fs::read_range(&log_path, event.offset, event.record_len)
                        .map_err(read_error)?;
                    verified_payload(&paths, event, &line)
                })
                .collect();
        }
        let span =
            fs::read_range(&log_path, first.offset, end - first.offset).map_err(read_error)?;
        let mut cursor = 0_usize;
        events
            .iter()
            .map(|event| {
                let len = usize::try_from(event.record_len).map_err(|_| StoreError::Corrupt)?;
                let line = span.get(cursor..cursor + len).ok_or(StoreError::Corrupt)?;
                cursor += len;
                verified_payload(&paths, event, line)
            })
            .collect()
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
            let bytes = u64_from(bytes)?;
            let offset = u64_from(offset)?;
            let record_len = u64_from(record_len)?;
            let tool = matches!(kind, EventKind::ToolCall | EventKind::ToolResult);
            if tool != call_id.is_some()
                || !super::super::digest::is_canonical_digest(&digest)
                || bytes == 0
                || bytes > kind.payload_bound() as u64
                || record_len == 0
            {
                return Err(StoreError::Corrupt);
            }
            events.push(StoredEvent {
                event_id,
                digest,
                bytes,
                kind,
                call_id,
                offset,
                record_len,
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
        let paths = self.paths(session)?;
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
            "SELECT id, title, updated_at, root_branch, event_count, corrupt
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
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(|_| StoreError::Storage)?;
    let mut listed = Vec::new();
    for row in session_rows {
        let (id, title, updated_at, root_branch, event_count, corrupt) =
            row.map_err(|_| StoreError::Storage)?;
        let updated_at_ms = u64::try_from(updated_at).ok().filter(|&ms| ms > 0);
        let (event_count, corrupt) = match u64::try_from(event_count) {
            Ok(count) => (count, corrupt != 0),
            Err(_) => (0, true),
        };
        listed.push(ListedSession {
            id,
            title,
            updated_at_ms,
            event_count,
            corrupt,
            root_branch,
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
    let mut corrupt_branches: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in branch_rows {
        let (session_id, branch_id, head, event_count) = row.map_err(|_| StoreError::Storage)?;
        let Ok(event_count) = u64::try_from(event_count) else {
            corrupt_branches.insert(session_id);
            continue;
        };
        by_session
            .entry(session_id)
            .or_default()
            .push(ListedBranch {
                branch_id,
                head,
                event_count,
            });
    }
    for session in &mut listed {
        if corrupt_branches.contains(&session.id) {
            session.corrupt = true;
        }
        if let Some(branches) = by_session.remove(&session.id) {
            session.branches = branches;
        }
    }
    Ok(listed)
}

/// Replaces one session's sidebar title. A missing database or row is
/// success; the actor never caches titles, so this write cannot desync it.
pub(crate) fn rename_indexed_session(
    home: &HomeLayout,
    session_id: &str,
    title: &str,
) -> Result<(), StoreError> {
    if SessionId::parse(session_id).is_none() {
        return Ok(());
    }
    let path = database_path(home).map_err(|_| StoreError::Unavailable)?;
    if !path.exists() {
        return Ok(());
    }
    // A short-lived writer, but never the WAL-flipping pragma: the session
    // actor already holds a writer connection to the same database, and a
    // second `journal_mode=WAL` takes an exclusive lock that surfaces as
    // "the session service is unavailable" while a turn runs.
    let conn = open_writer(&path)?;
    if !table_ready(&conn)? {
        return Ok(());
    }
    conn.execute(
        "UPDATE sessions SET title = ?1 WHERE id = ?2",
        params![title, session_id],
    )
    .map_err(|_| StoreError::Storage)?;
    Ok(())
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
    // See `rename_indexed_session`: a writer without the WAL pragma, so the
    // actor's own connection is never lock-contended by this one.
    let conn = open_writer(&path)?;
    if !table_ready(&conn)? {
        return Ok(());
    }
    conn.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])
        .map_err(|_| StoreError::Storage)?;
    Ok(())
}

/// A writer connection that does not touch `journal_mode`: the database is
/// already WAL once the actor opened it, and re-issuing the pragma from a
/// second connection requires an exclusive lock.
mod connection;
pub(crate) use self::connection::decode_head;
use self::connection::*;
