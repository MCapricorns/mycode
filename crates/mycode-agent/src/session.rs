//! First-party built-in Session service.
//!
//! Event-sourced branch/resume/rewind over one durable store: a SQLite
//! index (`sessions.db`) holds titles, branch heads, and JSONL byte
//! offsets, and `<home>/sessions/<project key>/<id>/<branch>.jsonl` holds
//! the events. Reservations are single-use under expected-head
//! compare-and-swap. The service runs on one session actor inside a
//! generation fence. Opening a session loads offset rows only. The typed
//! surface is documented in `wiki/index.html`.
mod actor;
mod digest;
mod dto;
mod fs;
mod generation;
mod ids;
mod ledger;
mod runtime;
mod service;
mod store;

#[doc(inline)]
pub use dto::{
    BranchHead, BranchMutationKind, EventKind, HeadStamp, SessionError, SessionEvent,
    SessionRequest, SessionResult,
};
#[doc(inline)]
pub use ids::{BranchId, BranchReservationId, SessionCallId, SessionEventId, SessionId};
#[doc(inline)]
pub use service::SessionService;

/// Read-only branch snapshot for session listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchSnapshot {
    /// Branch identity.
    pub branch_id: BranchId,
    /// Committed head.
    pub head: HeadStamp,
    /// Committed event count.
    pub event_count: u64,
}

/// Read-only session snapshot for session listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSnapshot {
    /// Session identity.
    pub session_id: SessionId,
    /// Sidebar title stored in the index. Empty until the first user message.
    pub title: String,
    /// Last durable mutation time in Unix epoch milliseconds (`None` when
    /// unknown or invalid).
    pub updated_at_ms: Option<u64>,
    /// Sum of committed events across branches.
    pub event_count: u64,
    /// The branch the session's writes last went to — the one a reopen
    /// should show. Older rows only name the original root; it still
    /// resolves because the pointer self-corrects on the next append.
    pub current_branch: BranchId,
    /// Every branch, ordered by branch-ID bytes.
    pub branches: Vec<BranchSnapshot>,
}

/// Read-only session listing for session discovery.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionsListing {
    /// Every strictly valid session snapshot, newest first, then session ID.
    pub sessions: Vec<SessionSnapshot>,
    /// Index rows marked corrupt, or rows whose identities failed validation.
    /// They stay visible so a frontend can offer deletion instead of losing
    /// the whole listing to one bad row.
    pub corrupt: Vec<String>,
}

/// Lists sessions from the SQLite index only.
///
/// A missing database is an empty list. Session directories that are not
/// indexed are ignored. The list is newest-first. A corrupt row is reported
/// through [`SessionsListing::corrupt`] instead of failing the listing.
///
/// # Errors
///
/// Returns [`SessionError::Unavailable`] when the index cannot be read.
pub fn inspect_sessions(home: &mycode_config::HomeLayout) -> Result<SessionsListing, SessionError> {
    let listed = store::list_sessions(home).map_err(store_error)?;
    let mut listing = SessionsListing::default();
    for row in listed {
        if row.corrupt {
            listing.corrupt.push(row.id);
            continue;
        }
        match snapshot_from_row(row) {
            Ok(snapshot) => listing.sessions.push(snapshot),
            Err(id) => listing.corrupt.push(id),
        }
    }
    listing.corrupt.sort();
    Ok(listing)
}

/// Deletes one session's index rows. A missing database is success.
///
/// # Errors
///
/// Returns [`SessionError::Unavailable`] when the index cannot be updated.
pub fn delete_session_index(
    home: &mycode_config::HomeLayout,
    session_id: &str,
) -> Result<(), SessionError> {
    store::delete_indexed_session(home, session_id).map_err(store_error)
}

/// Replaces one session's sidebar title. A missing database or row is
/// success. The title is index metadata, not a ledger event, so the write
/// goes straight to the index like [`delete_session_index`].
///
/// # Errors
///
/// Returns [`SessionError::Unavailable`] when the index cannot be updated.
pub fn rename_session_index(
    home: &mycode_config::HomeLayout,
    session_id: &str,
    title: &str,
) -> Result<(), SessionError> {
    store::rename_indexed_session(home, session_id, title).map_err(store_error)
}

fn snapshot_from_row(row: store::ListedSession) -> Result<SessionSnapshot, String> {
    let id = row.id.clone();
    let Some(session_id) = SessionId::parse(&row.id) else {
        return Err(id);
    };
    if row.branches.is_empty() || row.branches.len() > dto::MAX_BRANCHES {
        return Err(id);
    }
    let mut branches = Vec::with_capacity(row.branches.len());
    let mut sum_events = 0_u64;
    for branch in row.branches {
        let Some(branch_id) = BranchId::parse(&branch.branch_id) else {
            return Err(id);
        };
        let Some(head) = store::decode_head(&branch.head) else {
            return Err(id);
        };
        if matches!(head, HeadStamp::Empty) != (branch.event_count == 0) {
            return Err(id);
        }
        let Some(next_sum) = sum_events.checked_add(branch.event_count) else {
            return Err(id);
        };
        sum_events = next_sum;
        branches.push(BranchSnapshot {
            branch_id,
            head,
            event_count: branch.event_count,
        });
    }
    if sum_events != row.event_count {
        return Err(id);
    }
    // The pointer names the branch the session's writes last went to; a
    // row predating it names the root. Either way fall back to the first
    // listed branch when it does not resolve — never corruption.
    let current_branch = BranchId::parse(&row.root_branch)
        .filter(|root| branches.iter().any(|branch| &branch.branch_id == root))
        .unwrap_or_else(|| branches[0].branch_id.clone());
    Ok(SessionSnapshot {
        session_id,
        title: row.title,
        updated_at_ms: row.updated_at_ms,
        event_count: row.event_count,
        current_branch,
        branches,
    })
}

fn store_error(error: store::StoreError) -> SessionError {
    match error {
        store::StoreError::NotFound => SessionError::NotFound,
        store::StoreError::Corrupt | store::StoreError::Conflict(_) => SessionError::Corrupt,
        store::StoreError::Limit => SessionError::Limit,
        store::StoreError::Storage | store::StoreError::Unavailable => SessionError::Unavailable,
    }
}
