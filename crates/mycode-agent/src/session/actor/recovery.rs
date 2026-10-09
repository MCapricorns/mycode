//! Session load: the SQLite offset index is the commit authority.
//!
//! Opening a session reads branch heads and event offsets. It does not scan
//! the JSONL log. A torn tail past the committed length is truncated; a log
//! shorter than that length is corruption. Payload bytes are read later, one
//! line at a time, when a caller asks for them.

use std::collections::HashSet;

use super::super::dto::HeadStamp;
use super::super::ids::SessionId;
use super::super::ledger::{BranchLedger, EventMeta, SessionLedger};
use super::super::store::{StoreError, charge};
use super::{OpFail, PlanError, SessionCore};

impl SessionCore {
    /// Loads one session's offset index into the actor when it is not already
    /// resident. Payload bytes stay on disk.
    pub(super) fn ensure_loaded(&mut self, session: &SessionId) -> Result<(), PlanError> {
        if self.sessions.contains_key(session) {
            return Ok(());
        }
        if self.store.is_none() {
            self.store = super::super::store::SessionStore::open(&self.home).ok();
        }
        let store = self.store.as_ref().ok_or(PlanError::Storage)?;
        let loaded = store.load_session(session).map_err(plan_error)?;
        let mut total = 0_u64;
        let mut branches = std::collections::BTreeMap::new();
        for branch in loaded {
            let mut open_calls = HashSet::new();
            let mut seen_calls = HashSet::new();
            let mut events = Vec::with_capacity(branch.events.len());
            for event in branch.events {
                match event.kind {
                    super::super::dto::EventKind::ToolCall => {
                        let call = event.call_id.clone().ok_or(PlanError::Corrupt)?;
                        if !seen_calls.insert(call.clone()) {
                            return Err(PlanError::Corrupt);
                        }
                        open_calls.insert(call);
                    }
                    super::super::dto::EventKind::ToolResult => {
                        let call = event.call_id.clone().ok_or(PlanError::Corrupt)?;
                        if !open_calls.remove(&call) {
                            return Err(PlanError::Corrupt);
                        }
                    }
                    super::super::dto::EventKind::Message
                    | super::super::dto::EventKind::Usage
                    | super::super::dto::EventKind::Task => {}
                }
                let charge = charge(event.record_len, event.external, event.bytes)
                    .ok_or(PlanError::Corrupt)?;
                total = total.checked_add(charge).ok_or(PlanError::Corrupt)?;
                events.push(EventMeta {
                    event_id: event.event_id,
                    digest: event.digest,
                    bytes: event.bytes,
                    kind: event.kind,
                    call_id: event.call_id,
                    offset: event.offset,
                    record_len: event.record_len,
                    external: event.external,
                });
            }
            if !matches!(
                (&branch.head, events.last()),
                (HeadStamp::Empty, None) | (HeadStamp::Event(_), Some(_))
            ) {
                return Err(PlanError::Corrupt);
            }
            branches.insert(
                branch.branch_id,
                BranchLedger {
                    head: branch.head,
                    events,
                    committed_bytes: branch.committed_bytes,
                    open_calls,
                },
            );
        }
        self.sessions.insert(
            session.clone(),
            SessionLedger {
                branches,
                total_bytes: total,
                ..SessionLedger::default()
            },
        );
        Ok(())
    }

    pub(super) fn store_mut(&mut self) -> Result<&mut super::super::store::SessionStore, OpFail> {
        if self.store.is_none() {
            self.store = super::super::store::SessionStore::open(&self.home).ok();
        }
        self.store
            .as_mut()
            .ok_or(OpFail::Domain(super::super::dto::SessionError::Unavailable))
    }

    pub(super) fn store_ref(&self) -> Result<&super::super::store::SessionStore, OpFail> {
        self.store
            .as_ref()
            .ok_or(OpFail::Domain(super::super::dto::SessionError::Unavailable))
    }
}

fn plan_error(error: StoreError) -> PlanError {
    match error {
        StoreError::NotFound => PlanError::NotFound,
        StoreError::Corrupt | StoreError::Conflict(_) => PlanError::Corrupt,
        StoreError::Storage | StoreError::Limit | StoreError::Unavailable => PlanError::Storage,
    }
}
