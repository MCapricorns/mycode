//! First-party session service facade.
//!
//! [`SessionService`] owns one publication generation: the actor runs on the
//! T8 typed task runtime, durable work is admitted through the Host
//! generation fence, and every facade call drives its operation to exactly
//! one terminal pull under a bounded deadline.
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use tokio::time::Instant;

use mycode_config::HomeLayout;

use crate::session::generation::GenerationFence;
use crate::session::runtime::{TaskActorClient, TaskActorError, TaskCloseSignal};

use super::actor::{SessionActor, SessionTaskError};
use super::dto::{
    AppendedResult, BranchMutationKind, BranchedResult, CreatedResult, EventKind,
    EventReservationView, EventsResult, HeadStamp, LoadedEvent, OpenedResult, SessionError,
    SessionRequest, SessionResult,
};
use super::ids::{BranchId, SessionCallId, SessionEventId, SessionId};

/// Default per-operation deadline.
const DEFAULT_DEADLINE: Duration = Duration::from_secs(30);

/// The first-party session service.
///
/// The service is cheap to construct and owns one serialized actor; clones
/// share one inner owner, so dropping an individual clone is harmless —
/// only the last drop retires the publication, closes live operations, and
/// aborts the worker. Use [`SessionService::shutdown`] to additionally
/// await quiescence before reclaiming the store.
#[derive(Clone)]
pub struct SessionService {
    inner: Arc<ServiceInner>,
}

/// Shared owner behind every `SessionService` clone.
struct ServiceInner {
    client: TaskActorClient<SessionActor>,
    fence: Arc<GenerationFence>,
}

impl Drop for ServiceInner {
    fn drop(&mut self) {
        self.fence.mark_retired();
        self.fence.close_publication();
    }
}

impl SessionService {
    /// Starts the service over one owned home without creating any object.
    ///
    /// Must run on a Tokio runtime: the session actor is a task on that
    /// runtime, and its storage work uses `spawn_blocking`.
    #[must_use]
    pub fn new(home: &HomeLayout) -> Self {
        let fence = Arc::new(GenerationFence::new(Arc::new(AtomicU64::new(0))));
        fence.mark_current();
        let actor = SessionActor::new(home.clone(), Arc::clone(&fence));
        let client = TaskActorClient::start(actor);
        Self {
            inner: Arc::new(ServiceInner { client, fence }),
        }
    }

    /// Returns the actor client.
    fn client(&self) -> &TaskActorClient<SessionActor> {
        &self.inner.client
    }

    /// Creates a fresh session with a Host-minted root branch.
    ///
    /// # Errors
    ///
    /// Returns the actor's terminal error; `Unavailable` when the storage
    /// substrate or the generation fence rejects the publication.
    pub async fn create(&self) -> Result<CreatedResult, SessionError> {
        match self.run(SessionRequest::Create).await? {
            SessionResult::Created(created) => Ok(created),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Recovers one session and returns every branch head.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::NotFound`] for an unknown session,
    /// [`SessionError::Corrupt`] when recovery fails validation, and the
    /// actor's terminal error otherwise.
    pub async fn open(&self, session: &SessionId) -> Result<OpenedResult, SessionError> {
        match self
            .run(SessionRequest::Open {
                session: session.clone(),
            })
            .await?
        {
            SessionResult::Opened(opened) => Ok(opened),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Drops one session's in-memory ledger so later writes cannot
    /// resurrect durable data the host is about to delete.
    ///
    /// Idempotent: evicting an unknown or already-evicted session succeeds.
    ///
    /// # Errors
    ///
    /// Returns the actor's terminal error only; the eviction itself cannot
    /// fail at the domain level.
    pub async fn forget(&self, session: &SessionId) -> Result<(), SessionError> {
        match self
            .run(SessionRequest::Evict {
                session: session.clone(),
            })
            .await?
        {
            SessionResult::Evicted => Ok(()),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Validates one event, durably stages its payload, and issues the
    /// single-use reservation.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Limit`] for payload or session bounds,
    /// [`SessionError::InvalidArgument`] when the call/result ordering check
    /// rejects the event, and the actor's terminal error otherwise.
    pub async fn reserve_event(
        &self,
        session: &SessionId,
        branch: &BranchId,
        kind: EventKind,
        call_id: Option<SessionCallId>,
        payload: &[u8],
    ) -> Result<EventReservationView, SessionError> {
        match self
            .run(SessionRequest::ReserveEvent {
                session: session.clone(),
                branch: branch.clone(),
                kind,
                call_id,
                payload: payload.to_vec(),
            })
            .await?
        {
            SessionResult::ReservedEvent(view) => Ok(view),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Commits one reserved event under expected-head compare-and-swap.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Conflict`] with the actual head when the CAS
    /// lost, [`SessionError::NotFound`] for a missing or replayed
    /// reservation, and the actor's terminal error otherwise.
    pub async fn append(
        &self,
        session: &SessionId,
        branch: &BranchId,
        expected_head: &HeadStamp,
        reservation: &EventReservationView,
    ) -> Result<AppendedResult, SessionError> {
        match self
            .run(SessionRequest::Append {
                session: session.clone(),
                branch: branch.clone(),
                expected_head: expected_head.clone(),
                reservation: reservation.clone(),
            })
            .await?
        {
            SessionResult::Appended(appended) => Ok(appended),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Reads one bounded page of one immutable branch snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Limit`] for a page size outside `1..=256`,
    /// [`SessionError::InvalidArgument`] for stale or foreign cursors, and
    /// the actor's terminal error otherwise.
    pub async fn read(
        &self,
        session: &SessionId,
        branch: &BranchId,
        snapshot_head: &HeadStamp,
        after: Option<&SessionEventId>,
        limit: u16,
    ) -> Result<EventsResult, SessionError> {
        match self
            .run(SessionRequest::Read {
                session: session.clone(),
                branch: branch.clone(),
                snapshot_head: snapshot_head.clone(),
                after: after.cloned(),
                limit,
            })
            .await?
        {
            SessionResult::Events(events) => Ok(events),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Issues one single-use branch mutation reservation.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::NotFound`] for an unknown branch or target
    /// event, [`SessionError::Limit`] at the branch bound, and the actor's
    /// terminal error otherwise.
    pub async fn reserve_branch(
        &self,
        session: &SessionId,
        kind: BranchMutationKind,
        source_branch: &BranchId,
        target_event: &SessionEventId,
    ) -> Result<super::dto::BranchReservationView, SessionError> {
        match self
            .run(SessionRequest::ReserveBranch {
                session: session.clone(),
                kind,
                source_branch: source_branch.clone(),
                target_event: target_event.clone(),
            })
            .await?
        {
            SessionResult::ReservedBranch(view) => Ok(view),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Consumes one fork reservation under source-head compare-and-swap.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Conflict`] when the source head moved,
    /// [`SessionError::InvalidArgument`] for a crossed reservation, and the
    /// actor's terminal error otherwise.
    pub async fn fork(
        &self,
        session: &SessionId,
        from_branch: &BranchId,
        at_event: &SessionEventId,
        reservation: &super::dto::BranchReservationView,
    ) -> Result<BranchedResult, SessionError> {
        match self
            .run(SessionRequest::Fork {
                session: session.clone(),
                from_branch: from_branch.clone(),
                at_event: at_event.clone(),
                reservation: reservation.clone(),
            })
            .await?
        {
            SessionResult::Branched(branched) => Ok(branched),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Consumes one rewind reservation under source-head compare-and-swap.
    ///
    /// # Errors
    ///
    /// Mirrors [`SessionService::fork`].
    pub async fn rewind(
        &self,
        session: &SessionId,
        branch: &BranchId,
        to_event: &SessionEventId,
        reservation: &super::dto::BranchReservationView,
    ) -> Result<BranchedResult, SessionError> {
        match self
            .run(SessionRequest::Rewind {
                session: session.clone(),
                branch: branch.clone(),
                to_event: to_event.clone(),
                reservation: reservation.clone(),
            })
            .await?
        {
            SessionResult::Branched(branched) => Ok(branched),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Loads one committed event with its digest-verified payload.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::NotFound`] for an unknown event and the
    /// actor's terminal error otherwise.
    pub async fn load_event(
        &self,
        session: &SessionId,
        branch: &BranchId,
        event: &SessionEventId,
    ) -> Result<LoadedEvent, SessionError> {
        match self
            .run(SessionRequest::LoadEvent {
                session: session.clone(),
                branch: branch.clone(),
                event: event.clone(),
            })
            .await?
        {
            SessionResult::Loaded(loaded) => Ok(loaded),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Reads one page of committed events and their verified payloads.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::NotFound`] for an unknown session or branch
    /// and the actor's terminal error otherwise.
    pub async fn read_payloads(
        &self,
        session: &SessionId,
        branch: &BranchId,
        snapshot_head: &HeadStamp,
        after: Option<&SessionEventId>,
        limit: u16,
    ) -> Result<super::dto::PayloadPage, SessionError> {
        match self
            .run(SessionRequest::ReadPayloads {
                session: session.clone(),
                branch: branch.clone(),
                snapshot_head: snapshot_head.clone(),
                after: after.cloned(),
                limit,
            })
            .await?
        {
            SessionResult::Payloads(page) => Ok(page),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Retires the publication, closes live operations, and awaits drain.
    ///
    /// After this call the service rejects every further operation; other
    /// clones observe the same retirement because the fence is shared.
    pub async fn shutdown(self) {
        self.inner.fence.mark_retired();
        self.inner.client.shutdown();
        self.inner.fence.wait_drained().await;
        self.inner.fence.close_publication();
    }

    async fn run(&self, request: SessionRequest) -> Result<SessionResult, SessionError> {
        let close = TaskCloseSignal::new();
        let deadline = Instant::now() + DEFAULT_DEADLINE;
        let operation = self
            .client()
            .invoke(request, deadline, close)
            .await
            .map_err(map_task_error)?;
        loop {
            // Refresh the deadline per pull: recovery replays in 1 MiB
            // chunks, so a large session needs many sequential pulls and
            // each one gets its own full window instead of sharing the
            // invoke's budget.
            let deadline = Instant::now() + DEFAULT_DEADLINE;
            match self.client().pull(operation, deadline).await {
                Ok(pull) => match pull {
                    super::dto::SessionPull::Complete(result) => {
                        self.client().close(operation);
                        return Ok(result);
                    }
                    super::dto::SessionPull::Failed(error) => {
                        self.client().close(operation);
                        return Err(error);
                    }
                    super::dto::SessionPull::Progress(_) => {
                        continue;
                    }
                },
                Err(error) => {
                    self.client().close(operation);
                    return Err(map_task_error(error));
                }
            }
        }
    }
}

fn map_task_error(error: TaskActorError<SessionTaskError>) -> SessionError {
    match error {
        TaskActorError::UnknownOperation | TaskActorError::Unavailable => SessionError::Unavailable,
        TaskActorError::Cancelled => SessionError::Cancelled,
        TaskActorError::Pack(SessionTaskError::Admission) => SessionError::Limit,
        TaskActorError::Pack(SessionTaskError::Storage) => SessionError::Unavailable,
    }
}
