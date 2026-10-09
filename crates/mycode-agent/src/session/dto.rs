//! Typed session requests and results.
//!
//! `create`, `open`, `append`, `read`, `fork`, and `rewind` are the durable
//! operations. `reserve-event`, `reserve-branch`, and `load-event` issue
//! reservations and read payloads inside the session actor. See `docs/agent.md`.
use super::ids::{BranchId, BranchReservationId, SessionCallId, SessionEventId, SessionId};

/// Maximum number of branches in one session ledger.
pub const MAX_BRANCHES: usize = 64;
/// Maximum event payload size: 8 MiB.
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
/// Maximum usage event payload size: 64 KiB.
pub const MAX_USAGE_PAYLOAD_BYTES: usize = 64 * 1024;
/// Maximum page size accepted by one read request.
pub const MAX_READ_LIMIT: u16 = 256;

/// Committed head of one branch: empty or one event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeadStamp {
    /// The branch holds no committed event.
    Empty,
    /// The branch head is exactly this event.
    Event(SessionEventId),
}

/// Classifies one session event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// A conversation message payload.
    Message,
    /// An issued tool call; `call_id` is `Some`.
    ToolCall,
    /// A completed tool result; `call_id` is `Some`.
    ToolResult,
    /// A bounded usage record.
    Usage,
    /// Legacy wire tag 4. Existing ledgers still open; replay skips the
    /// payload. New reservations of this kind are rejected.
    Task,
}

impl EventKind {
    /// Returns the wire tag used by the durable record codec.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Message => 0,
            Self::ToolCall => 1,
            Self::ToolResult => 2,
            Self::Usage => 3,
            Self::Task => 4,
        }
    }

    /// Parses the wire tag used by the durable record codec.
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::Message),
            1 => Some(Self::ToolCall),
            2 => Some(Self::ToolResult),
            3 => Some(Self::Usage),
            4 => Some(Self::Task),
            _ => None,
        }
    }

    /// Returns the payload byte bound for this kind.
    #[must_use]
    pub const fn payload_bound(self) -> usize {
        match self {
            Self::Usage => MAX_USAGE_PAYLOAD_BYTES,
            Self::Message | Self::ToolCall | Self::ToolResult | Self::Task => {
                MAX_EVENT_PAYLOAD_BYTES
            }
        }
    }
}

/// One committed event's ledger metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEvent {
    /// The event identity.
    pub event_id: SessionEventId,
    /// Lowercase `sha256:` digest of the payload bytes.
    pub digest: String,
    /// Payload byte length.
    pub bytes: u64,
    /// Event classification.
    pub kind: EventKind,
    /// Present only for `tool-call` and `tool-result`.
    pub call_id: Option<SessionCallId>,
}

/// One branch head in an `open` result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchHead {
    /// The branch identity.
    pub branch_id: BranchId,
    /// The branch's committed head.
    pub head: HeadStamp,
}

/// Host-issued single-use event reservation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventReservationView {
    /// The reserved event identity.
    pub event_id: SessionEventId,
    /// Lowercase `sha256:` digest of the reserved payload.
    pub payload_digest: String,
    /// The branch the payload was staged onto.
    pub branch_id: BranchId,
    /// The head the reservation binds for its commit.
    pub expected_head: HeadStamp,
}

/// Distinguishes fork from rewind mutations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BranchMutationKind {
    /// Copy a source branch prefix at one event.
    Fork,
    /// Copy the current branch prefix through one event.
    Rewind,
}

impl BranchMutationKind {
    /// Returns the zero-based digest tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Fork => 0,
            Self::Rewind => 1,
        }
    }
}

/// Host-issued single-use branch mutation reservation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchReservationView {
    /// The reservation identity (`sbr1-`).
    pub reservation_id: BranchReservationId,
    /// Fork or rewind.
    pub kind: BranchMutationKind,
    /// The branch whose prefix is copied.
    pub source_branch_id: BranchId,
    /// The source head bound by the reservation.
    pub source_head: HeadStamp,
    /// The prefix boundary event.
    pub target_event_id: SessionEventId,
    /// The branch the mutation creates.
    pub new_branch_id: BranchId,
    /// Lowercase `sha256:` mutation digest over the frozen framing.
    pub mutation_digest: String,
}

/// One request to the session service.
#[derive(Debug, Clone)]
pub enum SessionRequest {
    /// Creates a fresh session; both root identifiers are Host-minted.
    Create,
    /// Recovers one session and returns every branch head.
    Open {
        /// The session to recover.
        session: SessionId,
    },
    /// Drops one session's in-memory ledger without touching disk.
    ///
    /// Idempotent; a host deletes a session's durable footprint under this
    /// cover so no later in-flight write resurrects the directory.
    Evict {
        /// The session to forget.
        session: SessionId,
    },
    /// Validates, durably stages a payload, and issues an event reservation.
    ReserveEvent {
        /// The session to append into.
        session: SessionId,
        /// The branch to append onto.
        branch: BranchId,
        /// The event classification.
        kind: EventKind,
        /// Required exactly for tool-call and tool-result.
        call_id: Option<SessionCallId>,
        /// The payload bytes (`1..=8 MiB`; usage `1..=64 KiB`).
        payload: Vec<u8>,
    },
    /// Consumes one event reservation under expected-head CAS.
    Append {
        /// The session to append into.
        session: SessionId,
        /// The branch to append onto.
        branch: BranchId,
        /// The head the caller observed.
        expected_head: HeadStamp,
        /// The single-use reservation.
        reservation: EventReservationView,
    },
    /// Reads one bounded page of one immutable snapshot.
    Read {
        /// The session to read.
        session: SessionId,
        /// The branch to read.
        branch: BranchId,
        /// The snapshot boundary head.
        snapshot_head: HeadStamp,
        /// Exclusive start cursor; `None` starts at the first event.
        after: Option<SessionEventId>,
        /// Page size (`1..=256`).
        limit: u16,
    },
    /// Issues one single-use branch mutation reservation.
    ReserveBranch {
        /// The session to mutate.
        session: SessionId,
        /// Fork or rewind.
        kind: BranchMutationKind,
        /// The branch whose prefix is copied.
        source_branch: BranchId,
        /// The prefix boundary event on the source branch.
        target_event: SessionEventId,
    },
    /// Consumes one fork reservation under source-head CAS.
    Fork {
        /// The session to mutate.
        session: SessionId,
        /// The branch whose prefix is copied.
        from_branch: BranchId,
        /// The prefix boundary event.
        at_event: SessionEventId,
        /// The single-use reservation.
        reservation: BranchReservationView,
    },
    /// Consumes one rewind reservation under source-head CAS.
    Rewind {
        /// The session to mutate.
        session: SessionId,
        /// The branch whose prefix is copied.
        branch: BranchId,
        /// The prefix boundary event.
        to_event: SessionEventId,
        /// The single-use reservation.
        reservation: BranchReservationView,
    },
    /// Reads one page of committed events together with their payloads.
    ///
    /// One actor round-trip covers the metadata page and every payload in
    /// it, so replaying a branch does not pay a separate invoke per event.
    ReadPayloads {
        /// The session to read.
        session: SessionId,
        /// The branch to read.
        branch: BranchId,
        /// The snapshot boundary head.
        snapshot_head: HeadStamp,
        /// Exclusive start cursor; `None` starts at the first event.
        after: Option<SessionEventId>,
        /// Page size (`1..=256`).
        limit: u16,
    },
    /// Reads the newest `limit` events at or before `before`.
    ///
    /// `before` absent means the snapshot head. `older` on the result names
    /// the first returned event when still-earlier events exist.
    ReadPayloadWindow {
        /// The session to read.
        session: SessionId,
        /// The branch to read.
        branch: BranchId,
        /// The snapshot boundary head.
        snapshot_head: HeadStamp,
        /// Exclusive end cursor; `None` ends at the snapshot head.
        before: Option<SessionEventId>,
        /// Page size (`1..=256`).
        limit: u16,
    },
    /// Loads one committed event with its verified payload.
    LoadEvent {
        /// The session to read.
        session: SessionId,
        /// The branch holding the event.
        branch: BranchId,
        /// The event to load.
        event: SessionEventId,
    },
}

/// One `pull` observation of a running session operation.
///
/// The actor returns exactly one terminal pull. Index load finishes inside
/// that pull, so there is no separate progress observation.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionPull {
    /// The operation finished successfully.
    Complete(SessionResult),
    /// The operation failed; the error is terminal for this operation.
    Failed(SessionError),
}

/// Successful terminal payloads.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionResult {
    /// `create` finished.
    Created(CreatedResult),
    /// `open` finished.
    Opened(OpenedResult),
    /// `evict` finished; nothing to return.
    Evicted,
    /// `append` finished.
    Appended(AppendedResult),
    /// `read` finished.
    Events(EventsResult),
    /// `fork`/`rewind` finished.
    Branched(BranchedResult),
    /// `reserve-event` finished (first-party only).
    ReservedEvent(EventReservationView),
    /// `reserve-branch` finished (first-party only).
    ReservedBranch(BranchReservationView),
    /// `load-event` finished (first-party only).
    Loaded(LoadedEvent),
    /// `read-payloads` finished (first-party only).
    Payloads(PayloadPage),
    /// `read-payload-window` finished (first-party only).
    PayloadWindow(PayloadWindow),
}

/// One backward page of committed events with their verified payloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadWindow {
    /// Events in ledger order, newest window last.
    pub items: Vec<LoadedEvent>,
    /// Oldest returned event when earlier events remain; otherwise `None`.
    pub older: Option<SessionEventId>,
}

/// One page of committed events with their verified payloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadPage {
    /// Events in ledger order, each with its payload.
    pub items: Vec<LoadedEvent>,
    /// Cursor of the last returned event; `None` only at snapshot EOF.
    pub next: Option<SessionEventId>,
}

/// `create` result: both identifiers are Host-minted and the head is empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedResult {
    /// The fresh session identity.
    pub session_id: SessionId,
    /// The fresh root branch identity.
    pub branch_id: BranchId,
}

/// `open` result: every branch head in branch-ID byte order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedResult {
    /// All branch heads, ordered by branch ID bytes.
    pub heads: Vec<BranchHead>,
}

/// `append` result: exactly `event(reservation.event-id)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppendedResult {
    /// The branch head after the commit.
    pub head: HeadStamp,
}

/// `read` result page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventsResult {
    /// Metadata rows in ledger order.
    pub items: Vec<SessionEvent>,
    /// Cursor of the last returned event; `None` only at snapshot EOF.
    pub next: Option<SessionEventId>,
}

/// `fork`/`rewind` result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchedResult {
    /// Exactly the reservation's `new-branch-id`.
    pub branch_id: BranchId,
    /// Exactly `event(target-event-id)`.
    pub head: HeadStamp,
}

/// `load-event` result: metadata plus the verified payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedEvent {
    /// The event metadata.
    pub event: SessionEvent,
    /// The payload bytes, digest-verified.
    pub payload: Vec<u8>,
}

/// Terminal session operation failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionError {
    /// The request shape, identifier, or cross-field relation is invalid.
    InvalidArgument,
    /// The session, branch, event, or reservation does not exist.
    NotFound,
    /// An expected-head compare-and-swap lost; the actual head is included.
    Conflict(ConflictResult),
    /// Durable content failed validation and is never silently repaired.
    Corrupt,
    /// A fixed bound was reached (payload, page, or branch count).
    Limit,
    /// The operation was cancelled through its close signal or outlived its
    /// per-operation deadline; the service itself stays available for
    /// further operations.
    Cancelled,
    /// The service or its storage substrate is unavailable.
    Unavailable,
}

/// Reports the observed head that made a compare-and-swap lose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictResult {
    /// The actual branch head at the failed commit.
    pub actual: HeadStamp,
}
