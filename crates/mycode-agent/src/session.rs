//! First-party built-in Session service.
//!
//! Event-sourced branch/resume/rewind over one durable store: a SQLite
//! index (`sessions.db`) holds titles, branch heads, and JSONL byte
//! offsets, and `<home>/sessions/<id>/<branch>.jsonl` holds the events.
//! Reservations are single-use under expected-head compare-and-swap. The
//! service runs on one session actor inside a generation fence. Opening a
//! session loads offset rows only. The typed surface is documented in
//! `docs/agent.md`.
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
    /// Sum of committed events across branches.
    pub event_count: u64,
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

/// Indexes JSONL logs placed by an import when that session is not already
/// indexed.
///
/// Returns whether a new index row was inserted. Invalid session ids and
/// sessions already present are left untouched.
///
/// # Errors
///
/// Returns [`SessionError::Corrupt`] when a log cannot be decoded, and
/// [`SessionError::Unavailable`] when the index cannot be written.
pub fn index_imported_session(
    home: &mycode_config::HomeLayout,
    session_id: &str,
) -> Result<bool, SessionError> {
    store::index_jsonl_session(home, session_id).map_err(store_error)
}

fn snapshot_from_row(row: store::ListedSession) -> Result<SessionSnapshot, String> {
    let id = row.id.clone();
    let Some(session_id) = SessionId::parse(&row.id) else {
        return Err(id);
    };
    if row.branches.is_empty() {
        return Err(id);
    }
    let mut branches = Vec::with_capacity(row.branches.len());
    for branch in row.branches {
        let Some(branch_id) = BranchId::parse(&branch.branch_id) else {
            return Err(id);
        };
        let Some(head) = decode_listed_head(&branch.head) else {
            return Err(id);
        };
        branches.push(BranchSnapshot {
            branch_id,
            head,
            event_count: branch.event_count,
        });
    }
    Ok(SessionSnapshot {
        session_id,
        title: row.title,
        event_count: row.event_count,
        branches,
    })
}

fn decode_listed_head(value: &str) -> Option<HeadStamp> {
    if value == "empty" {
        return Some(HeadStamp::Empty);
    }
    SessionEventId::parse(value).map(HeadStamp::Event)
}

fn store_error(error: store::StoreError) -> SessionError {
    match error {
        store::StoreError::NotFound => SessionError::NotFound,
        store::StoreError::Corrupt | store::StoreError::Conflict(_) => SessionError::Corrupt,
        store::StoreError::Limit => SessionError::Limit,
        store::StoreError::Storage | store::StoreError::Unavailable => SessionError::Unavailable,
    }
}
