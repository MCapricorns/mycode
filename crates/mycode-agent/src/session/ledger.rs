//! In-memory session ledger state and pure validation logic.
//!
//! One [`SessionLedger`] is the actor-owned projection of a recovered session:
//! branch heads, ordered event indexes with log offsets, open tool-call
//! tracking for the call/result ordering check, and the single-use
//! reservation tables. Every mutation is applied by the actor only after the
//! matching durable effect succeeded.
use std::collections::{BTreeMap, HashMap, HashSet};

use super::dto::{BranchHead, EventKind, EventsResult, HeadStamp, SessionError, SessionEvent};
use super::ids::{BranchId, SessionCallId, SessionEventId};

/// Half-open index range of one backward history window.
pub(crate) struct HistoryWindow {
    /// First included event index.
    pub(crate) start: usize,
    /// One past the last included event index.
    pub(crate) end: usize,
    /// Oldest returned event when earlier events remain; otherwise `None`.
    pub(crate) older: Option<SessionEventId>,
}

/// Half-open index range of one read page, plus the pagination cursor.
pub(crate) struct PageWindow {
    /// First included event index.
    pub(crate) start: usize,
    /// One past the last included event index.
    pub(crate) end: usize,
    /// Cursor of the last returned event; `None` at snapshot EOF.
    pub(crate) next: Option<SessionEventId>,
}

/// One committed event's index row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EventMeta {
    /// Event identity.
    pub(crate) event_id: SessionEventId,
    /// Canonical payload digest spelling.
    pub(crate) digest: String,
    /// Payload byte length.
    pub(crate) bytes: u64,
    /// Event classification.
    pub(crate) kind: EventKind,
    /// Present only for tool kinds.
    pub(crate) call_id: Option<SessionCallId>,
    /// Byte offset of the JSONL line inside the branch log.
    pub(crate) offset: u64,
    /// JSONL line length, newline included.
    pub(crate) record_len: u64,
    /// Payload bytes live beside the log rather than on the line.
    pub(crate) external: bool,
}

impl EventMeta {
    /// Projects one index row to its DTO metadata.
    pub(crate) fn to_session_event(&self) -> SessionEvent {
        SessionEvent {
            event_id: self.event_id.clone(),
            digest: self.digest.clone(),
            bytes: self.bytes,
            kind: self.kind,
            call_id: self.call_id.clone(),
        }
    }
}

/// One branch's in-memory state.
#[derive(Clone, Debug)]
pub(crate) struct BranchLedger {
    /// Committed head.
    pub(crate) head: HeadStamp,
    /// Ordered committed event rows.
    pub(crate) events: Vec<EventMeta>,
    /// Committed prefix length of the branch log.
    pub(crate) committed_bytes: u64,
    /// Tool calls issued without their result yet.
    pub(crate) open_calls: HashSet<SessionCallId>,
}

impl BranchLedger {
    /// Builds a fresh root branch state.
    pub(crate) fn root() -> Self {
        Self {
            head: HeadStamp::Empty,
            events: Vec::new(),
            committed_bytes: 0,
            open_calls: HashSet::new(),
        }
    }
}

/// One actor session projection.
#[derive(Debug, Default)]
pub(crate) struct SessionLedger {
    /// Branches keyed by identity (byte order).
    pub(crate) branches: BTreeMap<BranchId, BranchLedger>,
    /// Committed bytes across every branch log.
    pub(crate) total_bytes: u64,
    /// Single-use event reservations keyed by event identity.
    pub(crate) event_reservations: HashMap<SessionEventId, EventReservationRow>,
    /// Single-use branch reservations keyed by reservation identity.
    pub(crate) branch_reservations: HashMap<super::ids::BranchReservationId, BranchReservationRow>,
}

/// A live single-use event reservation.
#[derive(Clone, Debug)]
pub(crate) struct EventReservationRow {
    /// The issued view.
    pub(crate) view: super::dto::EventReservationView,
    /// The event classification bound at issuance.
    pub(crate) kind: EventKind,
    /// The call identity bound at issuance.
    pub(crate) call_id: Option<SessionCallId>,
    /// Payload bytes held until the append commits them. Reservations do not
    /// survive process restart, so the bytes stay in the actor.
    pub(crate) payload: Vec<u8>,
}

/// A live single-use branch reservation.
#[derive(Clone, Debug)]
pub(crate) struct BranchReservationRow {
    /// The issued view.
    pub(crate) view: super::dto::BranchReservationView,
}

impl SessionLedger {
    /// Returns every branch head in branch-ID byte order.
    pub(crate) fn heads(&self) -> Vec<BranchHead> {
        self.branches
            .iter()
            .map(|(branch_id, branch)| BranchHead {
                branch_id: branch_id.clone(),
                head: branch.head.clone(),
            })
            .collect()
    }

    /// Enforces the full call/result ordering check for one candidate event.
    ///
    /// A tool call must not repeat any call identity already present on the
    /// branch; a tool result must resolve one open tool call; and a call
    /// identity is legal only on tool kinds.
    pub(crate) fn check_ordering(
        branch: &BranchLedger,
        kind: EventKind,
        call_id: Option<&SessionCallId>,
    ) -> Result<(), SessionError> {
        let tool_kind = matches!(kind, EventKind::ToolCall | EventKind::ToolResult);
        match (tool_kind, call_id) {
            (false, None) => Ok(()),
            (false, Some(_)) => Err(SessionError::InvalidArgument),
            (true, None) => Err(SessionError::InvalidArgument),
            (true, Some(call)) => match kind {
                EventKind::ToolCall => {
                    let repeated = branch.events.iter().any(|event| {
                        event
                            .call_id
                            .as_ref()
                            .is_some_and(|existing| existing == call)
                    });
                    if repeated {
                        Err(SessionError::InvalidArgument)
                    } else {
                        Ok(())
                    }
                }
                EventKind::ToolResult => {
                    if branch.open_calls.contains(call) {
                        Ok(())
                    } else {
                        Err(SessionError::InvalidArgument)
                    }
                }
                other => unreachable!("tool kind {other:?} matched above"),
            },
        }
    }

    /// Computes one read page against an immutable snapshot boundary.
    ///
    /// The snapshot must name the current-empty head or one committed event
    /// of the branch; `after` must be `None` or a committed event at or
    /// before the snapshot boundary. The empty snapshot is valid only while
    /// the branch is still empty, matching the stale-cursor rejection.
    pub(crate) fn page(
        &self,
        branch: &BranchId,
        snapshot_head: &HeadStamp,
        after: Option<&SessionEventId>,
        limit: u16,
    ) -> Result<EventsResult, SessionError> {
        let window = self.page_window(branch, snapshot_head, after, limit)?;
        let branch_state = self.branches.get(branch).ok_or(SessionError::NotFound)?;
        let items = branch_state.events[window.start..window.end]
            .iter()
            .map(EventMeta::to_session_event)
            .collect();
        Ok(EventsResult {
            items,
            next: window.next,
        })
    }

    /// Index window for one read page. Payload reads use the same bounds so
    /// a page of events is loaded without a second cursor scan.
    pub(crate) fn page_window(
        &self,
        branch: &BranchId,
        snapshot_head: &HeadStamp,
        after: Option<&SessionEventId>,
        limit: u16,
    ) -> Result<PageWindow, SessionError> {
        if limit == 0 {
            return Err(SessionError::Limit);
        }
        let branch_state = self.branches.get(branch).ok_or(SessionError::NotFound)?;
        let boundary = snapshot_boundary(branch_state, snapshot_head)?;
        let start = match after {
            None => 0,
            Some(event) => Self::index_of(branch_state, event).ok_or(SessionError::NotFound)? + 1,
        };
        if start > boundary {
            return Err(SessionError::InvalidArgument);
        }
        let end = start + (boundary - start).min(usize::from(limit));
        let next = if end >= boundary {
            None
        } else {
            Some(branch_state.events[end - 1].event_id.clone())
        };
        Ok(PageWindow { start, end, next })
    }

    /// The newest `limit` events at or before `before` (or the snapshot head
    /// when `before` is absent). `older` names the first returned event when
    /// still-earlier events exist, so the next call can page backward.
    pub(crate) fn window_before(
        &self,
        branch: &BranchId,
        snapshot_head: &HeadStamp,
        before: Option<&SessionEventId>,
        limit: u16,
    ) -> Result<HistoryWindow, SessionError> {
        if limit == 0 {
            return Err(SessionError::Limit);
        }
        let branch_state = self.branches.get(branch).ok_or(SessionError::NotFound)?;
        let boundary = snapshot_boundary(branch_state, snapshot_head)?;
        let end = match before {
            None => boundary,
            Some(event) => {
                let index = Self::index_of(branch_state, event).ok_or(SessionError::NotFound)?;
                if index >= boundary {
                    return Err(SessionError::InvalidArgument);
                }
                index
            }
        };
        let start = end.saturating_sub(usize::from(limit));
        let older = if start > 0 {
            Some(branch_state.events[start].event_id.clone())
        } else {
            None
        };
        Ok(HistoryWindow { start, end, older })
    }

    fn index_of(branch: &BranchLedger, event: &SessionEventId) -> Option<usize> {
        branch.events.iter().position(|row| &row.event_id == event)
    }
}

/// Exclusive end index of the snapshot, or `InvalidArgument` when the head
/// is not the empty branch or a committed event on it.
fn snapshot_boundary(
    branch_state: &BranchLedger,
    snapshot_head: &HeadStamp,
) -> Result<usize, SessionError> {
    match snapshot_head {
        HeadStamp::Empty if branch_state.events.is_empty() => Ok(0),
        HeadStamp::Empty => Err(SessionError::InvalidArgument),
        HeadStamp::Event(event) => SessionLedger::index_of(branch_state, event)
            .map(|index| index + 1)
            .ok_or(SessionError::InvalidArgument),
    }
}
