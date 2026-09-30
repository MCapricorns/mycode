//! Terminal actions: the durable effects behind every session operation.
//!
//! Each `action_*` method runs after admission and (for unloaded sessions)
//! recovery, mutates the owned-file store inside the generation fence, and
//! applies the matching in-memory ledger transition only after the durable
//! effect succeeded.

use mycode_config::{ConfigError, ensure_owned_directory, locked_update_owned_file};

use super::super::digest::{
    BranchMutationDigestInput, branch_mutation_digest, format_digest, payload_digest,
};
use super::super::dto::{
    AppendedResult, BranchMutationKind, BranchReservationView, BranchedResult, ConflictResult,
    CreatedResult, EventKind, EventReservationView, HeadStamp, LoadedEvent, MAX_BRANCHES,
    OpenedResult, PayloadPage, SessionError, SessionPull, SessionResult,
};
use super::super::fs;
use super::super::ids::{BranchId, BranchReservationId, SessionCallId, SessionEventId, SessionId};
use super::super::ledger::{
    BranchLedger, BranchReservationRow, EventMeta, EventReservationRow, SessionLedger,
};
use super::super::store::{
    self, MANIFEST_FORMAT_VERSION, MANIFEST_KIND, MAX_MANIFEST_BYTES, MAX_SESSION_TOTAL_BYTES,
    ManifestBranchFile, ManifestFile, ParentageFile, SessionPaths,
};
use super::{OpFail, SessionCore};

impl SessionCore {
    pub(super) fn action_create(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
    ) -> Result<SessionPull, OpFail> {
        let unavailable = || OpFail::Domain(SessionError::Unavailable);
        let Some(activity) = self.fence.enter() else {
            return Err(OpFail::Domain(SessionError::Unavailable));
        };
        let paths = SessionPaths::new(&self.home, session);
        let manifest = ManifestFile {
            format_version: MANIFEST_FORMAT_VERSION,
            kind: MANIFEST_KIND.to_owned(),
            session_id: session.as_str().to_owned(),
            branches: vec![ManifestBranchFile {
                branch_id: branch.as_str().to_owned(),
                parentage: ParentageFile {
                    kind: "root".to_owned(),
                    source_branch_id: None,
                    at_event_id: None,
                    to_event_id: None,
                },
                head: store::encode_head(&HeadStamp::Empty),
                event_count: 0,
                committed_bytes: 0,
            }],
        };
        let bytes = store::encode_manifest(&manifest).map_err(|_| unavailable())?;
        let mut collision = false;
        let commit = activity.begin_commit().map_err(|_| unavailable())?;
        let update = locked_update_owned_file(
            &self.home,
            paths.manifest(),
            MAX_MANIFEST_BYTES,
            |current| match current {
                None => Ok(bytes),
                Some(_) => {
                    collision = true;
                    Err(ConfigError::authority_rejection())
                }
            },
        );
        drop(commit);
        match update {
            Ok(()) => {}
            Err(_) if collision => return Err(unavailable()),
            Err(_) => return Err(OpFail::Storage),
        }
        for directory in [paths.branches_dir(), paths.pending_dir()] {
            ensure_owned_directory(&self.home, directory).map_err(|_| OpFail::Storage)?;
        }
        let mut ledger = SessionLedger::empty();
        ledger.branches.insert(branch.clone(), BranchLedger::root());
        self.sessions.insert(session.clone(), ledger);
        Ok(SessionPull::Complete(SessionResult::Created(
            CreatedResult {
                session_id: session.clone(),
                branch_id: branch.clone(),
            },
        )))
    }

    pub(super) fn action_heads(&mut self, session: &SessionId) -> Result<SessionPull, OpFail> {
        let Some(ledger) = self.sessions.get(session) else {
            return Err(OpFail::Domain(SessionError::NotFound));
        };
        Ok(SessionPull::Complete(SessionResult::Opened(OpenedResult {
            heads: ledger.heads(),
        })))
    }

    /// Forgets one session's in-memory ledger. Disk is untouched: the host
    /// owns the deletion this covers. Reservations die with the ledger, so a
    /// later append can at worst fail `NotFound`, never resurrect the files.
    pub(super) fn action_evict(&mut self, session: &SessionId) -> Result<SessionPull, OpFail> {
        self.sessions.remove(session);
        Ok(SessionPull::Complete(SessionResult::Evicted))
    }

    pub(super) fn action_reserve_event(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
        kind: EventKind,
        call_id: Option<SessionCallId>,
        payload: &[u8],
    ) -> Result<SessionPull, OpFail> {
        let Some(_activity) = self.fence.enter() else {
            return Err(OpFail::Domain(SessionError::Unavailable));
        };
        let paths = SessionPaths::new(&self.home, session);
        let ledger = self
            .sessions
            .get_mut(session)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        let branch_state = ledger
            .branches
            .get(branch)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        SessionLedger::check_ordering(branch_state, kind, call_id.as_ref())
            .map_err(OpFail::Domain)?;
        let record_len = store::encoded_record_len(call_id.is_some(), payload.len())
            .ok_or(OpFail::Domain(SessionError::Limit))?;
        let total = ledger
            .total_bytes
            .checked_add(record_len)
            .ok_or(OpFail::Domain(SessionError::Limit))?;
        if total > MAX_SESSION_TOTAL_BYTES {
            return Err(OpFail::Domain(SessionError::Limit));
        }
        let event_id =
            SessionEventId::generate().ok_or(OpFail::Domain(SessionError::Unavailable))?;
        let digest_raw = payload_digest(payload);
        ensure_owned_directory(&self.home, paths.pending_dir()).map_err(|_| OpFail::Storage)?;
        let pending = paths
            .absolute(&paths.pending_payload(&event_id))
            .map_err(|_| OpFail::Storage)?;
        fs::create_exclusive(&pending, payload).map_err(|_| OpFail::Storage)?;
        let view = EventReservationView {
            payload_digest: format_digest(&digest_raw),
            expected_head: branch_state.head.clone(),
            branch_id: branch.clone(),
            event_id: event_id.clone(),
        };
        ledger.event_reservations.insert(
            event_id.clone(),
            EventReservationRow {
                view: view.clone(),
                kind,
                call_id,
            },
        );
        Ok(SessionPull::Complete(SessionResult::ReservedEvent(view)))
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn action_append(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
        expected_head: &HeadStamp,
        reservation: &EventReservationView,
    ) -> Result<SessionPull, OpFail> {
        let Some(activity) = self.fence.enter() else {
            return Err(OpFail::Domain(SessionError::Unavailable));
        };
        let paths = SessionPaths::new(&self.home, session);
        let ledger = self
            .sessions
            .get_mut(session)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        // Consume the single-use reservation regardless of the commit result.
        let Some(row) = ledger.event_reservations.remove(&reservation.event_id) else {
            return Err(OpFail::Domain(SessionError::NotFound));
        };
        if row.view != *reservation {
            return Err(OpFail::Domain(SessionError::InvalidArgument));
        }
        if &row.view.branch_id != branch {
            return Err(OpFail::Domain(SessionError::InvalidArgument));
        }
        let branch_state = ledger
            .branches
            .get_mut(branch)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        if branch_state.head != *expected_head || row.view.expected_head != *expected_head {
            return Err(OpFail::Domain(SessionError::Conflict(ConflictResult {
                actual: branch_state.head.clone(),
            })));
        }
        // The staged payload was created by this actor's exclusive create;
        // read it back with the same bounded plain-FS primitive.
        let pending_path = paths
            .absolute(&paths.pending_payload(&reservation.event_id))
            .map_err(|_| OpFail::Storage)?;
        let payload = match fs::file_len(&pending_path) {
            Ok(length) if length > 0 && length <= row.kind.payload_bound() as u64 => {
                fs::read_range(&pending_path, 0, length).map_err(|_| OpFail::Storage)?
            }
            Ok(_) => return Err(OpFail::Domain(SessionError::Corrupt)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(OpFail::Domain(SessionError::Corrupt));
            }
            Err(_) => return Err(OpFail::Storage),
        };
        if format_digest(&payload_digest(&payload)) != reservation.payload_digest {
            return Err(OpFail::Domain(SessionError::Corrupt));
        }
        let record = store::encode_record(
            &reservation.event_id,
            row.kind,
            row.call_id.as_ref(),
            &payload,
        );
        let total = ledger
            .total_bytes
            .checked_add(record.len() as u64)
            .ok_or(OpFail::Domain(SessionError::Limit))?;
        if total > MAX_SESSION_TOTAL_BYTES {
            return Err(OpFail::Domain(SessionError::Limit));
        }
        let log_path = paths
            .absolute(&paths.branch_events(branch))
            .map_err(|_| OpFail::Storage)?;
        // Any bytes beyond the committed prefix are earlier uncommitted
        // attempts; drop them so this record lands contiguously.
        match fs::file_len(&log_path) {
            Ok(length) if length > branch_state.committed_bytes => {
                fs::truncate(&log_path, branch_state.committed_bytes)
                    .map_err(|_| OpFail::Storage)?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(OpFail::Storage),
        }
        fs::append(&log_path, &record).map_err(|_| OpFail::Storage)?;

        let commit = activity
            .begin_commit()
            .map_err(|_| OpFail::Domain(SessionError::Unavailable))?;
        let expected_count = branch_state.events.len() as u64;
        let expected_committed = branch_state.committed_bytes;
        let expected_head_encoded = store::encode_head(expected_head);
        let event_id = reservation.event_id.clone();
        let mut conflict: Option<HeadStamp> = None;
        let update = locked_update_owned_file(
            &self.home,
            paths.manifest(),
            MAX_MANIFEST_BYTES,
            |current| {
                let mut manifest: ManifestFile =
                    serde_json::from_slice(current.ok_or_else(ConfigError::authority_rejection)?)
                        .map_err(|_| ConfigError::authority_rejection())?;
                let target = manifest
                    .branches
                    .iter_mut()
                    .find(|row| row.branch_id == branch.as_str())
                    .ok_or_else(ConfigError::authority_rejection)?;
                if target.head != expected_head_encoded
                    || target.event_count != expected_count
                    || target.committed_bytes != expected_committed
                {
                    conflict = store::decode_head(&target.head);
                    return Err(ConfigError::authority_rejection());
                }
                target.head = event_id.as_str().to_owned();
                target.event_count = expected_count + 1;
                target.committed_bytes = expected_committed + record.len() as u64;
                store::encode_manifest(&manifest).map_err(|_| ConfigError::authority_rejection())
            },
        );
        drop(commit);
        match update {
            Ok(()) => {}
            Err(_) => {
                if let Some(actual) = conflict.take() {
                    return Err(OpFail::Domain(SessionError::Conflict(ConflictResult {
                        actual,
                    })));
                }
                return Err(OpFail::Domain(SessionError::Corrupt));
            }
        }

        let offset = branch_state.committed_bytes;
        branch_state.committed_bytes += record.len() as u64;
        match row.kind {
            EventKind::ToolCall => {
                branch_state
                    .open_calls
                    .insert(row.call_id.clone().expect("tool call carries an id"));
            }
            EventKind::ToolResult => {
                branch_state
                    .open_calls
                    .remove(&row.call_id.clone().expect("tool result carries an id"));
            }
            EventKind::Message | EventKind::Usage => {}
        }
        branch_state.head = HeadStamp::Event(reservation.event_id.clone());
        branch_state.events.push(EventMeta {
            digest: reservation.payload_digest.clone(),
            bytes: payload.len() as u64,
            event_id: reservation.event_id.clone(),
            kind: row.kind,
            call_id: row.call_id.clone(),
            offset,
            record_len: record.len() as u64,
        });
        ledger.total_bytes = total;
        if let Ok(pending) = paths.absolute(&paths.pending_payload(&reservation.event_id)) {
            // Orphan-tolerant: recovery removes anything left behind.
            fs::remove(&pending);
        }
        Ok(SessionPull::Complete(SessionResult::Appended(
            AppendedResult {
                head: HeadStamp::Event(reservation.event_id.clone()),
            },
        )))
    }

    pub(super) fn action_read(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
        snapshot_head: &HeadStamp,
        after: Option<&SessionEventId>,
        limit: u16,
    ) -> Result<SessionPull, OpFail> {
        let Some(ledger) = self.sessions.get(session) else {
            return Err(OpFail::Domain(SessionError::NotFound));
        };
        let result = ledger
            .page(branch, snapshot_head, after, limit)
            .map_err(OpFail::Domain)?;
        Ok(SessionPull::Complete(SessionResult::Events(result)))
    }

    pub(super) fn action_reserve_branch(
        &mut self,
        session: &SessionId,
        kind: BranchMutationKind,
        source_branch: &BranchId,
        target_event: &SessionEventId,
    ) -> Result<SessionPull, OpFail> {
        let Some(_activity) = self.fence.enter() else {
            return Err(OpFail::Domain(SessionError::Unavailable));
        };
        let ledger = self
            .sessions
            .get_mut(session)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        let branch_state = ledger
            .branches
            .get(source_branch)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        if !branch_state
            .events
            .iter()
            .any(|row| &row.event_id == target_event)
        {
            return Err(OpFail::Domain(SessionError::NotFound));
        }
        if ledger.branches.len() >= MAX_BRANCHES {
            return Err(OpFail::Domain(SessionError::Limit));
        }
        let new_branch = BranchId::generate().ok_or(OpFail::Domain(SessionError::Unavailable))?;
        let reservation_id =
            BranchReservationId::generate().ok_or(OpFail::Domain(SessionError::Unavailable))?;
        let digest = branch_mutation_digest(&BranchMutationDigestInput {
            session_id: session.as_str(),
            reservation_id: reservation_id.as_str(),
            kind,
            source_branch_id: source_branch.as_str(),
            source_head: &branch_state.head,
            target_event_id: target_event,
            new_branch_id: new_branch.as_str(),
        })
        .ok_or(OpFail::Domain(SessionError::Unavailable))?;
        let view = BranchReservationView {
            reservation_id: reservation_id.clone(),
            kind,
            source_branch_id: source_branch.clone(),
            source_head: branch_state.head.clone(),
            target_event_id: target_event.clone(),
            new_branch_id: new_branch,
            mutation_digest: format_digest(&digest),
        };
        ledger
            .branch_reservations
            .insert(reservation_id, BranchReservationRow { view: view.clone() });
        Ok(SessionPull::Complete(SessionResult::ReservedBranch(view)))
    }

    pub(super) fn action_branch_commit(
        &mut self,
        session: &SessionId,
        kind: BranchMutationKind,
        source_branch: &BranchId,
        target_event: &SessionEventId,
        reservation: &BranchReservationView,
    ) -> Result<SessionPull, OpFail> {
        let Some(activity) = self.fence.enter() else {
            return Err(OpFail::Domain(SessionError::Unavailable));
        };
        let paths = SessionPaths::new(&self.home, session);
        let ledger = self
            .sessions
            .get_mut(session)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        // Consume the single-use reservation regardless of the result.
        let Some(row) = ledger
            .branch_reservations
            .remove(&reservation.reservation_id)
        else {
            return Err(OpFail::Domain(SessionError::NotFound));
        };
        if row.view != *reservation
            || row.view.kind != kind
            || &row.view.source_branch_id != source_branch
            || &row.view.target_event_id != target_event
        {
            return Err(OpFail::Domain(SessionError::InvalidArgument));
        }
        let recomputed = branch_mutation_digest(&BranchMutationDigestInput {
            session_id: session.as_str(),
            reservation_id: row.view.reservation_id.as_str(),
            kind,
            source_branch_id: row.view.source_branch_id.as_str(),
            source_head: &row.view.source_head,
            target_event_id: &row.view.target_event_id,
            new_branch_id: row.view.new_branch_id.as_str(),
        })
        .ok_or(OpFail::Domain(SessionError::InvalidArgument))?;
        if format_digest(&recomputed) != row.view.mutation_digest {
            return Err(OpFail::Domain(SessionError::InvalidArgument));
        }
        let branch_state = ledger
            .branches
            .get(source_branch)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        if branch_state.head != row.view.source_head {
            return Err(OpFail::Domain(SessionError::Conflict(ConflictResult {
                actual: branch_state.head.clone(),
            })));
        }
        if ledger.branches.len() >= MAX_BRANCHES {
            return Err(OpFail::Domain(SessionError::Limit));
        }
        let new_branch = row.view.new_branch_id.clone();
        if ledger.branches.contains_key(&new_branch) {
            return Err(OpFail::Domain(SessionError::Corrupt));
        }
        let position = branch_state
            .events
            .iter()
            .position(|row| &row.event_id == target_event)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        let boundary = &branch_state.events[position];
        let prefix_end = boundary.offset + boundary.record_len;
        let total = ledger
            .total_bytes
            .checked_add(prefix_end)
            .ok_or(OpFail::Domain(SessionError::Limit))?;
        if total > MAX_SESSION_TOTAL_BYTES {
            return Err(OpFail::Domain(SessionError::Limit));
        }
        let source_log = paths
            .absolute(&paths.branch_events(source_branch))
            .map_err(|_| OpFail::Storage)?;
        let new_log = paths
            .absolute(&paths.branch_events(&new_branch))
            .map_err(|_| OpFail::Storage)?;
        ensure_owned_directory(&self.home, paths.branches_dir()).map_err(|_| OpFail::Storage)?;
        fs::copy_prefix(&source_log, &new_log, prefix_end).map_err(|_| OpFail::Storage)?;

        let commit = activity
            .begin_commit()
            .map_err(|_| OpFail::Domain(SessionError::Unavailable))?;
        let expected_head = store::encode_head(&row.view.source_head);
        let expected_count = branch_state.events.len() as u64;
        let expected_committed = branch_state.committed_bytes;
        let mut conflict = false;
        let parentage = match kind {
            BranchMutationKind::Fork => ParentageFile {
                kind: "fork".to_owned(),
                source_branch_id: Some(source_branch.as_str().to_owned()),
                at_event_id: Some(target_event.as_str().to_owned()),
                to_event_id: None,
            },
            BranchMutationKind::Rewind => ParentageFile {
                kind: "rewind".to_owned(),
                source_branch_id: Some(source_branch.as_str().to_owned()),
                at_event_id: None,
                to_event_id: Some(target_event.as_str().to_owned()),
            },
        };
        let new_row = ManifestBranchFile {
            branch_id: new_branch.as_str().to_owned(),
            parentage,
            head: target_event.as_str().to_owned(),
            event_count: (position + 1) as u64,
            committed_bytes: prefix_end,
        };
        let update = locked_update_owned_file(
            &self.home,
            paths.manifest(),
            MAX_MANIFEST_BYTES,
            |current| {
                let mut manifest: ManifestFile =
                    serde_json::from_slice(current.ok_or_else(ConfigError::authority_rejection)?)
                        .map_err(|_| ConfigError::authority_rejection())?;
                let source = manifest
                    .branches
                    .iter_mut()
                    .find(|row| row.branch_id == source_branch.as_str())
                    .ok_or_else(ConfigError::authority_rejection)?;
                if source.head != expected_head
                    || source.event_count != expected_count
                    || source.committed_bytes != expected_committed
                {
                    conflict = true;
                    return Err(ConfigError::authority_rejection());
                }
                if manifest
                    .branches
                    .iter()
                    .any(|row| row.branch_id == new_row.branch_id)
                {
                    return Err(ConfigError::authority_rejection());
                }
                manifest.branches.push(new_row.clone());
                store::encode_manifest(&manifest).map_err(|_| ConfigError::authority_rejection())
            },
        );
        drop(commit);
        match update {
            Ok(()) => {}
            Err(_) if conflict => {
                // The copied file is an uncommitted attempt; remove it.
                fs::remove(&new_log);
                return Err(OpFail::Domain(SessionError::Conflict(ConflictResult {
                    actual: ledger
                        .branches
                        .get(source_branch)
                        .map_or(HeadStamp::Empty, |branch| branch.head.clone()),
                })));
            }
            Err(_) => {
                fs::remove(&new_log);
                return Err(OpFail::Domain(SessionError::Corrupt));
            }
        }

        let cloned = branch_state.events[..=position].to_vec();
        let mut open_calls = std::collections::HashSet::new();
        for row in &cloned {
            match row.kind {
                EventKind::ToolCall => {
                    open_calls.insert(row.call_id.clone().expect("tool call carries an id"));
                }
                EventKind::ToolResult => {
                    open_calls.remove(&row.call_id.clone().expect("tool result carries an id"));
                }
                EventKind::Message | EventKind::Usage => {}
            }
        }
        let new_state = BranchLedger {
            head: HeadStamp::Event(target_event.clone()),
            committed_bytes: prefix_end,
            open_calls,
            events: cloned,
        };
        ledger.branches.insert(new_branch.clone(), new_state);
        ledger.total_bytes = total;
        Ok(SessionPull::Complete(SessionResult::Branched(
            BranchedResult {
                branch_id: new_branch,
                head: HeadStamp::Event(target_event.clone()),
            },
        )))
    }

    pub(super) fn action_load_event(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
        event: &SessionEventId,
    ) -> Result<SessionPull, OpFail> {
        let Some(ledger) = self.sessions.get(session) else {
            return Err(OpFail::Domain(SessionError::NotFound));
        };
        let branch_state = ledger
            .branches
            .get(branch)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        let meta = branch_state
            .events
            .iter()
            .find(|row| &row.event_id == event)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        let paths = SessionPaths::new(&self.home, session);
        let absolute = paths
            .absolute(&paths.branch_events(branch))
            .map_err(|_| OpFail::Storage)?;
        let payload = read_verified_payload(&absolute, meta, event.as_str())?;
        Ok(SessionPull::Complete(SessionResult::Loaded(LoadedEvent {
            event: meta.to_session_event(),
            payload,
        })))
    }

    pub(super) fn action_read_payloads(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
        snapshot_head: &HeadStamp,
        after: Option<&SessionEventId>,
        limit: u16,
    ) -> Result<SessionPull, OpFail> {
        let Some(ledger) = self.sessions.get(session) else {
            return Err(OpFail::Domain(SessionError::NotFound));
        };
        let window = ledger
            .page_window(branch, snapshot_head, after, limit)
            .map_err(OpFail::Domain)?;
        let branch_state = ledger
            .branches
            .get(branch)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        let paths = SessionPaths::new(&self.home, session);
        let absolute = paths
            .absolute(&paths.branch_events(branch))
            .map_err(|_| OpFail::Storage)?;
        let mut items = Vec::with_capacity(window.end - window.start);
        for meta in &branch_state.events[window.start..window.end] {
            let payload = read_verified_payload(&absolute, meta, meta.event_id.as_str())?;
            items.push(LoadedEvent {
                event: meta.to_session_event(),
                payload,
            });
        }
        Ok(SessionPull::Complete(SessionResult::Payloads(
            PayloadPage {
                items,
                next: window.next,
            },
        )))
    }
}

fn read_verified_payload(
    absolute: &std::path::Path,
    meta: &EventMeta,
    event_id: &str,
) -> Result<Vec<u8>, OpFail> {
    let bytes =
        fs::read_range(absolute, meta.offset, meta.record_len).map_err(|_| OpFail::Storage)?;
    let decoded =
        store::decode_record(&bytes).map_err(|_| OpFail::Domain(SessionError::Corrupt))?;
    if decoded.event_id != event_id || decoded.record_len != meta.record_len {
        return Err(OpFail::Domain(SessionError::Corrupt));
    }
    let start = decoded.payload_offset;
    let end = start + decoded.payload_len as usize;
    bytes
        .get(start..end)
        .ok_or(OpFail::Domain(SessionError::Corrupt))
        .map(<[u8]>::to_vec)
}
