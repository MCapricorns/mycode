//! Terminal actions: the durable effects behind every session operation.
//!
//! Each `action_*` method runs after admission and (for unloaded sessions)
//! an index load. It mutates the SQLite index and JSONL log inside the
//! generation fence, then applies the matching in-memory ledger transition
//! only after that durable effect succeeded.

use super::super::digest::{
    BranchMutationDigestInput, branch_mutation_digest, format_digest, payload_digest,
};
use super::super::dto::{
    AppendedResult, BranchMutationKind, BranchReservationView, BranchedResult, ConflictResult,
    CreatedResult, EventKind, EventReservationView, HeadStamp, LoadedEvent, MAX_BRANCHES,
    OpenedResult, PayloadPage, PayloadWindow, SessionError, SessionPull, SessionResult,
};
use super::super::ids::{BranchId, SessionCallId, SessionEventId, SessionId};
use super::super::ledger::{BranchLedger, EventMeta, SessionLedger};
use super::super::store::{
    self, AppendRequest, BranchCommitRequest, MAX_SESSION_TOTAL_BYTES, StoredEvent,
};
use super::{OpFail, SessionCore};

/// Owned fields of one event append, after the reservation is consumed.
struct PreparedEvent {
    event_id: SessionEventId,
    kind: EventKind,
    call_id: Option<SessionCallId>,
    payload: Vec<u8>,
    digest: String,
    expected_count: u64,
    expected_committed: u64,
}

/// Owned fields of one fork or rewind, after the reservation is consumed.
struct PreparedBranch {
    new_branch: BranchId,
    position: u64,
    prefix_end: u64,
    source_count: u64,
    source_committed: u64,
    events: Vec<EventMeta>,
}

impl SessionCore {
    pub(super) fn action_create(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
    ) -> Result<SessionPull, OpFail> {
        let Some(activity) = self.fence.enter() else {
            return Err(OpFail::Domain(SessionError::Unavailable));
        };
        let commit = activity
            .begin_commit()
            .map_err(|_| OpFail::Domain(SessionError::Unavailable))?;
        let created = self.store_mut()?.create_session(session, branch);
        drop(commit);
        created.map_err(map_store)?;
        let mut ledger = SessionLedger::default();
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

    /// Forgets one session's in-memory ledger. Disk and the index are
    /// untouched: the host owns the deletion this covers. Reservations die
    /// with the ledger, so a later append can at worst fail `NotFound`.
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
        let view = EventReservationView {
            payload_digest: format_digest(&digest_raw),
            expected_head: branch_state.head.clone(),
            branch_id: branch.clone(),
            event_id: event_id.clone(),
        };
        ledger.event_reservations.insert(
            event_id,
            super::super::ledger::EventReservationRow {
                view: view.clone(),
                kind,
                call_id,
                payload: payload.to_vec(),
            },
        );
        Ok(SessionPull::Complete(SessionResult::ReservedEvent(view)))
    }

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
        let prepared = self.take_event_reservation(session, branch, expected_head, reservation)?;
        let request = AppendRequest {
            session,
            branch,
            expected_head,
            expected_count: prepared.expected_count,
            expected_committed: prepared.expected_committed,
            event_id: &prepared.event_id,
            kind: prepared.kind,
            call_id: prepared.call_id.as_ref(),
            payload: &prepared.payload,
            digest: &prepared.digest,
        };
        let commit = activity
            .begin_commit()
            .map_err(|_| OpFail::Domain(SessionError::Unavailable))?;
        let appended = self.store_mut()?.append_event(&request);
        drop(commit);
        let location = appended.map_err(map_store)?;
        self.record_append(session, branch, &prepared, location);
        Ok(SessionPull::Complete(SessionResult::Appended(
            AppendedResult {
                head: HeadStamp::Event(prepared.event_id),
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
        let reservation_id = super::super::ids::BranchReservationId::generate()
            .ok_or(OpFail::Domain(SessionError::Unavailable))?;
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
        ledger.branch_reservations.insert(
            reservation_id,
            super::super::ledger::BranchReservationRow { view: view.clone() },
        );
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
        let prepared =
            self.take_branch_reservation(session, kind, source_branch, target_event, reservation)?;
        let request = BranchCommitRequest {
            session,
            source: source_branch,
            new_branch: &prepared.new_branch,
            kind,
            expected_head: &reservation.source_head,
            expected_count: prepared.source_count,
            expected_committed: prepared.source_committed,
            target_event,
            position: prepared.position,
            prefix_end: prepared.prefix_end,
        };
        let commit = activity
            .begin_commit()
            .map_err(|_| OpFail::Domain(SessionError::Unavailable))?;
        let committed = self.store_mut()?.commit_branch(&request);
        drop(commit);
        let charge = committed.map_err(map_store)?;
        let new_branch = prepared.new_branch.clone();
        self.record_branch(session, &prepared, charge);
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
        let meta = {
            let ledger = self
                .sessions
                .get(session)
                .ok_or(OpFail::Domain(SessionError::NotFound))?;
            let branch_state = ledger
                .branches
                .get(branch)
                .ok_or(OpFail::Domain(SessionError::NotFound))?;
            branch_state
                .events
                .iter()
                .find(|row| &row.event_id == event)
                .cloned()
                .ok_or(OpFail::Domain(SessionError::NotFound))?
        };
        let payload = self.verified_payload(session, branch, &meta)?;
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
        let (metas, next) = {
            let ledger = self
                .sessions
                .get(session)
                .ok_or(OpFail::Domain(SessionError::NotFound))?;
            let window = ledger
                .page_window(branch, snapshot_head, after, limit)
                .map_err(OpFail::Domain)?;
            let branch_state = ledger
                .branches
                .get(branch)
                .ok_or(OpFail::Domain(SessionError::NotFound))?;
            let metas = branch_state.events[window.start..window.end].to_vec();
            (metas, window.next)
        };
        let items = self.load_range(session, branch, &metas)?;
        Ok(SessionPull::Complete(SessionResult::Payloads(
            PayloadPage { items, next },
        )))
    }

    pub(super) fn action_read_window(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
        snapshot_head: &HeadStamp,
        before: Option<&SessionEventId>,
        limit: u16,
    ) -> Result<SessionPull, OpFail> {
        let (metas, older) = {
            let ledger = self
                .sessions
                .get(session)
                .ok_or(OpFail::Domain(SessionError::NotFound))?;
            let window = ledger
                .window_before(branch, snapshot_head, before, limit)
                .map_err(OpFail::Domain)?;
            let branch_state = ledger
                .branches
                .get(branch)
                .ok_or(OpFail::Domain(SessionError::NotFound))?;
            let metas = branch_state.events[window.start..window.end].to_vec();
            (metas, window.older)
        };
        let items = self.load_range(session, branch, &metas)?;
        Ok(SessionPull::Complete(SessionResult::PayloadWindow(
            PayloadWindow { items, older },
        )))
    }

    /// Consumes one event reservation and checks the in-memory head.
    ///
    /// The reservation is single-use even when the later compare-and-swap
    /// loses.
    fn take_event_reservation(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
        expected_head: &HeadStamp,
        reservation: &EventReservationView,
    ) -> Result<PreparedEvent, OpFail> {
        let ledger = self
            .sessions
            .get_mut(session)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        let Some(row) = ledger.event_reservations.remove(&reservation.event_id) else {
            return Err(OpFail::Domain(SessionError::NotFound));
        };
        if row.view != *reservation || &row.view.branch_id != branch {
            return Err(OpFail::Domain(SessionError::InvalidArgument));
        }
        let branch_state = ledger
            .branches
            .get(branch)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        if branch_state.head != *expected_head || row.view.expected_head != *expected_head {
            return Err(OpFail::Domain(SessionError::Conflict(ConflictResult {
                actual: branch_state.head.clone(),
            })));
        }
        if format_digest(&payload_digest(&row.payload)) != reservation.payload_digest {
            return Err(OpFail::Domain(SessionError::Corrupt));
        }
        let upper = store::encoded_record_len(row.call_id.is_some(), row.payload.len())
            .ok_or(OpFail::Domain(SessionError::Limit))?;
        let total = ledger
            .total_bytes
            .checked_add(upper)
            .ok_or(OpFail::Domain(SessionError::Limit))?;
        if total > MAX_SESSION_TOTAL_BYTES {
            return Err(OpFail::Domain(SessionError::Limit));
        }
        Ok(PreparedEvent {
            expected_count: u64::try_from(branch_state.events.len())
                .map_err(|_| OpFail::Domain(SessionError::Limit))?,
            expected_committed: branch_state.committed_bytes,
            event_id: reservation.event_id.clone(),
            kind: row.kind,
            call_id: row.call_id,
            payload: row.payload,
            digest: reservation.payload_digest.clone(),
        })
    }

    fn record_append(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
        prepared: &PreparedEvent,
        location: store::AppendedLocation,
    ) {
        let Some(ledger) = self.sessions.get_mut(session) else {
            return;
        };
        let Some(branch_state) = ledger.branches.get_mut(branch) else {
            return;
        };
        match prepared.kind {
            EventKind::ToolCall | EventKind::ToolResult => {
                let call_id = prepared.call_id.clone().expect("tool event carries an id");
                if prepared.kind == EventKind::ToolCall {
                    branch_state.open_calls.insert(call_id);
                } else {
                    branch_state.open_calls.remove(&call_id);
                }
            }
            EventKind::Message | EventKind::Usage | EventKind::Task => {}
        }
        branch_state.head = HeadStamp::Event(prepared.event_id.clone());
        branch_state.committed_bytes = location.offset.saturating_add(location.record_len);
        branch_state.events.push(EventMeta {
            digest: prepared.digest.clone(),
            bytes: prepared.payload.len() as u64,
            event_id: prepared.event_id.clone(),
            kind: prepared.kind,
            call_id: prepared.call_id.clone(),
            offset: location.offset,
            record_len: location.record_len,
            external: location.external,
        });
        ledger.total_bytes = ledger.total_bytes.saturating_add(location.charge);
    }

    fn take_branch_reservation(
        &mut self,
        session: &SessionId,
        kind: BranchMutationKind,
        source_branch: &BranchId,
        target_event: &SessionEventId,
        reservation: &BranchReservationView,
    ) -> Result<PreparedBranch, OpFail> {
        let ledger = self
            .sessions
            .get_mut(session)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
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
        let prefix_end = boundary
            .offset
            .checked_add(boundary.record_len)
            .ok_or(OpFail::Domain(SessionError::Limit))?;
        let mut charge = 0_u64;
        for event in &branch_state.events[..=position] {
            let part = store::charge(event.record_len, event.external, event.bytes)
                .ok_or(OpFail::Domain(SessionError::Limit))?;
            charge = charge
                .checked_add(part)
                .ok_or(OpFail::Domain(SessionError::Limit))?;
        }
        let total = ledger
            .total_bytes
            .checked_add(charge)
            .ok_or(OpFail::Domain(SessionError::Limit))?;
        if total > MAX_SESSION_TOTAL_BYTES {
            return Err(OpFail::Domain(SessionError::Limit));
        }
        Ok(PreparedBranch {
            new_branch,
            position: u64::try_from(position).map_err(|_| OpFail::Domain(SessionError::Limit))?,
            prefix_end,
            source_count: u64::try_from(branch_state.events.len())
                .map_err(|_| OpFail::Domain(SessionError::Limit))?,
            source_committed: branch_state.committed_bytes,
            events: branch_state.events[..=position].to_vec(),
        })
    }

    fn record_branch(&mut self, session: &SessionId, prepared: &PreparedBranch, charge: u64) {
        let Some(ledger) = self.sessions.get_mut(session) else {
            return;
        };
        let mut open_calls = std::collections::HashSet::new();
        for row in &prepared.events {
            match row.kind {
                EventKind::ToolCall => {
                    open_calls.insert(row.call_id.clone().expect("tool call carries an id"));
                }
                EventKind::ToolResult => {
                    open_calls.remove(&row.call_id.clone().expect("tool result carries an id"));
                }
                EventKind::Message | EventKind::Usage | EventKind::Task => {}
            }
        }
        let head = prepared
            .events
            .last()
            .map(|event| HeadStamp::Event(event.event_id.clone()))
            .unwrap_or(HeadStamp::Empty);
        ledger.branches.insert(
            prepared.new_branch.clone(),
            BranchLedger {
                head,
                committed_bytes: prepared.prefix_end,
                open_calls,
                events: prepared.events.clone(),
            },
        );
        ledger.total_bytes = ledger.total_bytes.saturating_add(charge);
    }

    fn verified_payload(
        &self,
        session: &SessionId,
        branch: &BranchId,
        meta: &EventMeta,
    ) -> Result<Vec<u8>, OpFail> {
        let stored = StoredEvent {
            event_id: meta.event_id.clone(),
            digest: meta.digest.clone(),
            bytes: meta.bytes,
            kind: meta.kind,
            call_id: meta.call_id.clone(),
            offset: meta.offset,
            record_len: meta.record_len,
            external: meta.external,
        };
        self.store_ref()?
            .read_payload(session, branch, &stored)
            .map_err(map_store)
    }

    fn load_range(
        &self,
        session: &SessionId,
        branch: &BranchId,
        metas: &[EventMeta],
    ) -> Result<Vec<LoadedEvent>, OpFail> {
        let mut items = Vec::with_capacity(metas.len());
        for meta in metas {
            let payload = self.verified_payload(session, branch, meta)?;
            items.push(LoadedEvent {
                event: meta.to_session_event(),
                payload,
            });
        }
        Ok(items)
    }
}

fn map_store(error: store::StoreError) -> OpFail {
    match error {
        store::StoreError::Storage => OpFail::Storage,
        store::StoreError::Corrupt => OpFail::Domain(SessionError::Corrupt),
        store::StoreError::NotFound => OpFail::Domain(SessionError::NotFound),
        store::StoreError::Limit => OpFail::Domain(SessionError::Limit),
        store::StoreError::Unavailable => OpFail::Domain(SessionError::Unavailable),
        store::StoreError::Conflict(actual) => {
            OpFail::Domain(SessionError::Conflict(ConflictResult { actual }))
        }
    }
}
