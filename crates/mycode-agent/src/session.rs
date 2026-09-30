//! First-party built-in Session service.
//!
//! Event-sourced branch/resume/rewind over a durable per-session ledger:
//! strict manifests are published through the hardened owned-file
//! transaction, branch logs are append-only framed records with payload
//! digests, and staged payloads make reservations single-use under
//! expected-head compare-and-swap. The service runs on one session actor
//! inside a generation fence; recovery is chunked per pull and reports
//! `recovering` / `replaying` before the requested action runs.
//! The typed surface is documented in `docs/agent.md`.
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
#[doc(inline)]
pub use store::{MAX_MANIFEST_BYTES, MAX_SESSION_TOTAL_BYTES};

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
    /// Every branch, ordered by branch-ID bytes.
    pub branches: Vec<BranchSnapshot>,
}

/// Read-only session listing for session discovery.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionsListing {
    /// Every strictly valid session snapshot, ordered by session ID.
    pub sessions: Vec<SessionSnapshot>,
    /// Sessions whose directory exists but whose manifest could not be read
    /// or validated (a torn create, or an interrupted delete). Entries are
    /// raw directory names: a torn directory may not even spell a valid
    /// session id. They stay visible so a frontend can offer deletion
    /// instead of losing the whole listing to one stray directory.
    pub corrupt: Vec<String>,
}

/// Lists every stored session by strictly decoding each manifest.
///
/// The listing performs no recovery and mutates nothing; manifests are the
/// authority and atomically replaced, so a snapshot is always consistent.
/// A session directory that is unreadable or fails strict validation is
/// reported through [`SessionsListing::corrupt`] instead of failing the
/// listing; only an unreadable sessions directory itself, or an owned-path
/// violation, fails the call.
///
/// # Errors
///
/// Returns [`SessionError::Corrupt`] for an unreadable sessions directory
/// and [`SessionError::Unavailable`] for owned-path violations.
pub fn inspect_sessions(home: &mycode_config::HomeLayout) -> Result<SessionsListing, SessionError> {
    let sessions_root = home
        .owned_join(store::SESSIONS_RELATIVE_DIR)
        .map_err(|_| SessionError::Unavailable)?;
    let entries = match std::fs::read_dir(&sessions_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SessionsListing::default());
        }
        Err(_) => return Err(SessionError::Corrupt),
    };
    let mut listing = SessionsListing::default();
    for entry in entries {
        let entry = entry.map_err(|_| SessionError::Corrupt)?;
        if !entry
            .file_type()
            .map_err(|_| SessionError::Corrupt)?
            .is_dir()
        {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(session_id) = SessionId::parse(name) else {
            listing.corrupt.push(name.to_owned());
            continue;
        };
        let snapshot = session_snapshot(home, &session_id);
        match snapshot {
            Ok(snapshot) => listing.sessions.push(snapshot),
            Err(SessionError::Corrupt) | Err(SessionError::Unavailable) => {
                listing.corrupt.push(name.to_owned());
            }
            Err(_) => unreachable!("session_snapshot fails closed on Corrupt or Unavailable"),
        }
    }
    listing
        .sessions
        .sort_by(|a, b| a.session_id.cmp(&b.session_id));
    listing.corrupt.sort();
    Ok(listing)
}

/// Decodes one session's manifest into a snapshot; fails closed with
/// `Corrupt` (bad data) or `Unavailable` (substrate) so the caller can
/// classify the directory.
fn session_snapshot(
    home: &mycode_config::HomeLayout,
    session_id: &SessionId,
) -> Result<SessionSnapshot, SessionError> {
    let paths = store::SessionPaths::new(home, session_id);
    let manifest_bytes =
        mycode_config::read_owned_file(home, paths.manifest(), store::MAX_MANIFEST_BYTES)
            .map_err(|_| SessionError::Unavailable)?
            .ok_or(SessionError::Corrupt)?
            .to_vec();
    let manifest = store::decode_manifest(&manifest_bytes).map_err(|_| SessionError::Corrupt)?;
    let mut branches = Vec::with_capacity(manifest.branches.len());
    for row in &manifest.branches {
        branches.push(BranchSnapshot {
            branch_id: BranchId::parse(&row.branch_id).ok_or(SessionError::Corrupt)?,
            head: store::decode_head(&row.head).ok_or(SessionError::Corrupt)?,
            event_count: row.event_count,
        });
    }
    branches.sort_by(|a, b| a.branch_id.cmp(&b.branch_id));
    Ok(SessionSnapshot {
        session_id: session_id.clone(),
        branches,
    })
}
