//! Serialized first-party session actor over the typed task runtime.
//!
//! [`SessionActor`] implements [`PackTaskActor`]: every request is admitted
//! through the Host admission ledger, and durable effects run inside the
//! generation fence. The actor owns the only SQLite writer. Opening a
//! session loads offset rows from the index; JSONL payloads are read by
//! offset when a caller asks for them.
//!
//! The implementation is split by responsibility: this module owns the
//! actor state machine (admission, staging, pull loop, action dispatch),
//! [`recovery`] loads the offset index, and [`actions`] owns the durable
//! effects of every terminal action.
mod actions;
mod recovery;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mycode_config::HomeLayout;

use crate::session::generation::GenerationFence;
use crate::session::runtime::admission::AdmissionLedger;
use crate::session::runtime::{AdmissionError, PackTaskActor, TaskOperationAdmission};

use super::digest::is_canonical_digest;
use super::dto::{
    BranchMutationKind, EventKind, EventReservationView, HeadStamp, MAX_READ_LIMIT, SessionError,
    SessionPull, SessionRequest,
};
use super::ids::{BranchId, SessionCallId, SessionEventId, SessionId};
use super::ledger::SessionLedger;
use super::store::SessionStore;

/// Infrastructure-level actor failures; domain failures travel inside
/// [`SessionPull::Failed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionTaskError {
    /// The Host admission ledger is saturated.
    Admission,
    /// The storage substrate failed; the actor is permanently unavailable.
    Storage,
}

/// Durable session state. Methods run on the blocking pool.
pub(crate) struct SessionCore {
    fence: Arc<GenerationFence>,
    admission: AdmissionLedger,
    sessions: HashMap<SessionId, SessionLedger>,
    /// `None` when the index could not be opened. Every durable action then
    /// fails closed instead of writing beside a missing database.
    store: Option<SessionStore>,
}

/// Serialized session owner. Storage work is `spawn_blocking` so the caller
/// runtime is not stalled and no private thread or `block_on` is required.
pub(crate) struct SessionActor {
    core: Arc<Mutex<SessionCore>>,
}

impl SessionActor {
    pub(crate) fn new(home: HomeLayout, fence: Arc<GenerationFence>) -> Self {
        Self {
            core: Arc::new(Mutex::new(SessionCore::new(home, fence))),
        }
    }
}

/// One admitted session operation.
///
/// The stage lives behind a mutex shared with in-flight blocking pulls, so a
/// cancelled await still observes a completed commit on the next pull.
pub(crate) struct SessionOperation {
    inner: Arc<Mutex<SessionOpInner>>,
}

struct SessionOpInner {
    admission: Option<TaskOperationAdmission>,
    stage: Stage,
}

impl SessionOperation {
    fn new(admission: Option<TaskOperationAdmission>, stage: Stage) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionOpInner { admission, stage })),
        }
    }
}

enum Stage {
    /// The operation failed with a domain error.
    Failed(SessionError),
    /// The operation completed; repeated pulls replay the terminal.
    Terminal(SessionPull),
    /// The action is ready to run.
    Run(Action),
}

enum Action {
    Create {
        session: SessionId,
        branch: BranchId,
    },
    Heads {
        session: SessionId,
    },
    Evict {
        session: SessionId,
    },
    ReserveEvent {
        session: SessionId,
        branch: BranchId,
        kind: EventKind,
        call_id: Option<SessionCallId>,
        payload: Vec<u8>,
    },
    Append {
        session: SessionId,
        branch: BranchId,
        expected_head: HeadStamp,
        reservation: EventReservationView,
    },
    Read {
        session: SessionId,
        branch: BranchId,
        snapshot_head: HeadStamp,
        after: Option<SessionEventId>,
        limit: u16,
    },
    ReserveBranch {
        session: SessionId,
        kind: BranchMutationKind,
        source_branch: BranchId,
        target_event: SessionEventId,
    },
    BranchCommit {
        session: SessionId,
        kind: BranchMutationKind,
        source_branch: BranchId,
        target_event: SessionEventId,
        reservation: super::dto::BranchReservationView,
    },
    LoadEvent {
        session: SessionId,
        branch: BranchId,
        event: SessionEventId,
    },
    ReadPayloads {
        session: SessionId,
        branch: BranchId,
        snapshot_head: HeadStamp,
        after: Option<SessionEventId>,
        limit: u16,
    },
    ReadWindow {
        session: SessionId,
        branch: BranchId,
        snapshot_head: HeadStamp,
        before: Option<SessionEventId>,
        limit: u16,
    },
}

/// Why a session index could not be loaded.
pub(super) enum PlanError {
    NotFound,
    Corrupt,
    Storage,
}

/// Classifies one action-path failure.
enum OpFail {
    /// Durable substrate failure; fatal for the actor.
    Storage,
    /// Terminal domain failure for this operation.
    Domain(SessionError),
}

impl SessionCore {
    fn new(home: HomeLayout, fence: Arc<GenerationFence>) -> Self {
        Self {
            fence,
            admission: AdmissionLedger::new(),
            sessions: HashMap::new(),
            store: SessionStore::open(&home).ok(),
        }
    }

    fn mint_admission(&self) -> Result<TaskOperationAdmission, SessionTaskError> {
        let operation = self.admission.open_operation().map_err(admission_error)?;
        let resource = self.admission.admit_resource().map_err(admission_error)?;
        Ok(TaskOperationAdmission::new(operation, resource))
    }

    fn invoke_sync(
        &mut self,
        request: &SessionRequest,
    ) -> Result<SessionOperation, SessionTaskError> {
        fn failed(
            admission: Option<TaskOperationAdmission>,
            error: SessionError,
        ) -> Result<SessionOperation, SessionTaskError> {
            Ok(SessionOperation::new(admission, Stage::Failed(error)))
        }
        let admission = Some(self.mint_admission()?);
        let stage = match request {
            SessionRequest::Create => match (SessionId::generate(), BranchId::generate()) {
                (Some(session), Some(branch)) => Stage::Run(Action::Create { session, branch }),
                _ => return failed(admission, SessionError::Unavailable),
            },
            SessionRequest::Open { session } => self.stage_for(
                session.clone(),
                Action::Heads {
                    session: session.clone(),
                },
            )?,
            // Eviction never loads: a missing index row must still be
            // evictable, and an unknown session is already evicted.
            SessionRequest::Evict { session } => Stage::Run(Action::Evict {
                session: session.clone(),
            }),
            SessionRequest::ReserveEvent {
                session,
                branch,
                kind,
                call_id,
                payload,
            } => {
                // Tag 4 stays readable so old ledgers open. Nothing writes it.
                if matches!(kind, EventKind::Task) {
                    return failed(admission, SessionError::InvalidArgument);
                }
                if payload.is_empty() || payload.len() > kind.payload_bound() {
                    return failed(admission, SessionError::Limit);
                }
                if matches!(kind, EventKind::ToolCall | EventKind::ToolResult) != call_id.is_some()
                {
                    return failed(admission, SessionError::InvalidArgument);
                }
                self.stage_for(
                    session.clone(),
                    Action::ReserveEvent {
                        session: session.clone(),
                        branch: branch.clone(),
                        kind: *kind,
                        call_id: call_id.clone(),
                        payload: payload.clone(),
                    },
                )?
            }
            SessionRequest::Append {
                session,
                branch,
                expected_head,
                reservation,
            } => {
                if !is_canonical_digest(&reservation.payload_digest) {
                    return failed(admission, SessionError::InvalidArgument);
                }
                self.stage_for(
                    session.clone(),
                    Action::Append {
                        session: session.clone(),
                        branch: branch.clone(),
                        expected_head: expected_head.clone(),
                        reservation: reservation.clone(),
                    },
                )?
            }
            SessionRequest::Read {
                session,
                branch,
                snapshot_head,
                after,
                limit,
            } => {
                if *limit == 0 || *limit > MAX_READ_LIMIT {
                    return failed(admission, SessionError::Limit);
                }
                self.stage_for(
                    session.clone(),
                    Action::Read {
                        session: session.clone(),
                        branch: branch.clone(),
                        snapshot_head: snapshot_head.clone(),
                        after: after.clone(),
                        limit: *limit,
                    },
                )?
            }
            SessionRequest::ReserveBranch {
                session,
                kind,
                source_branch,
                target_event,
            } => self.stage_for(
                session.clone(),
                Action::ReserveBranch {
                    session: session.clone(),
                    kind: *kind,
                    source_branch: source_branch.clone(),
                    target_event: target_event.clone(),
                },
            )?,
            SessionRequest::Fork {
                session,
                from_branch,
                at_event,
                reservation,
            } => {
                if !is_canonical_digest(&reservation.mutation_digest) {
                    return failed(admission, SessionError::InvalidArgument);
                }
                self.stage_for(
                    session.clone(),
                    Action::BranchCommit {
                        session: session.clone(),
                        kind: BranchMutationKind::Fork,
                        source_branch: from_branch.clone(),
                        target_event: at_event.clone(),
                        reservation: reservation.clone(),
                    },
                )?
            }
            SessionRequest::Rewind {
                session,
                branch,
                to_event,
                reservation,
            } => {
                if !is_canonical_digest(&reservation.mutation_digest) {
                    return failed(admission, SessionError::InvalidArgument);
                }
                self.stage_for(
                    session.clone(),
                    Action::BranchCommit {
                        session: session.clone(),
                        kind: BranchMutationKind::Rewind,
                        source_branch: branch.clone(),
                        target_event: to_event.clone(),
                        reservation: reservation.clone(),
                    },
                )?
            }
            SessionRequest::LoadEvent {
                session,
                branch,
                event,
            } => self.stage_for(
                session.clone(),
                Action::LoadEvent {
                    session: session.clone(),
                    branch: branch.clone(),
                    event: event.clone(),
                },
            )?,
            SessionRequest::ReadPayloads {
                session,
                branch,
                snapshot_head,
                after,
                limit,
            } => {
                if *limit == 0 || *limit > MAX_READ_LIMIT {
                    return failed(admission, SessionError::Limit);
                }
                self.stage_for(
                    session.clone(),
                    Action::ReadPayloads {
                        session: session.clone(),
                        branch: branch.clone(),
                        snapshot_head: snapshot_head.clone(),
                        after: after.clone(),
                        limit: *limit,
                    },
                )?
            }
            SessionRequest::ReadPayloadWindow {
                session,
                branch,
                snapshot_head,
                before,
                limit,
            } => {
                if *limit == 0 || *limit > MAX_READ_LIMIT {
                    return failed(admission, SessionError::Limit);
                }
                self.stage_for(
                    session.clone(),
                    Action::ReadWindow {
                        session: session.clone(),
                        branch: branch.clone(),
                        snapshot_head: snapshot_head.clone(),
                        before: before.clone(),
                        limit: *limit,
                    },
                )?
            }
        };
        Ok(SessionOperation::new(admission, stage))
    }

    /// Routes an action for a known session, loading its offset index first
    /// when the ledger is not already resident.
    fn stage_for(&mut self, session: SessionId, next: Action) -> Result<Stage, SessionTaskError> {
        if let Err(error) = self.ensure_loaded(&session) {
            return match error {
                PlanError::NotFound => Ok(Stage::Failed(SessionError::NotFound)),
                PlanError::Corrupt => Ok(Stage::Failed(SessionError::Corrupt)),
                PlanError::Storage => Err(SessionTaskError::Storage),
            };
        }
        Ok(Stage::Run(next))
    }

    fn pull_sync(
        &mut self,
        operation: &mut SessionOpInner,
    ) -> Result<SessionPull, SessionTaskError> {
        {
            match &mut operation.stage {
                Stage::Terminal(pull) => Ok(pull.clone()),
                Stage::Failed(error) => Ok(SessionPull::Failed(error.clone())),
                Stage::Run(_) => {
                    let stage = std::mem::replace(
                        &mut operation.stage,
                        Stage::Failed(SessionError::Unavailable),
                    );
                    let Stage::Run(action) = stage else {
                        unreachable!("the stage was just replaced from Run");
                    };
                    let pull = self.execute(action)?;
                    operation.stage = match &pull {
                        SessionPull::Complete(_) => Stage::Terminal(pull.clone()),
                        SessionPull::Failed(error) => Stage::Failed(error.clone()),
                        _ => unreachable!("execute always returns a terminal"),
                    };
                    Ok(pull)
                }
            }
        }
    }

    /// Executes one terminal action.
    fn execute(&mut self, action: Action) -> Result<SessionPull, SessionTaskError> {
        match self.run(action) {
            Ok(pull) => Ok(pull),
            Err(OpFail::Storage) => Err(SessionTaskError::Storage),
            Err(OpFail::Domain(error)) => Ok(SessionPull::Failed(error)),
        }
    }

    fn run(&mut self, action: Action) -> Result<SessionPull, OpFail> {
        match action {
            Action::Create { session, branch } => self.action_create(&session, &branch),
            Action::Heads { session } => self.action_heads(&session),
            Action::Evict { session } => self.action_evict(&session),
            Action::ReserveEvent {
                session,
                branch,
                kind,
                call_id,
                payload,
            } => self.action_reserve_event(&session, &branch, kind, call_id, &payload),
            Action::Append {
                session,
                branch,
                expected_head,
                reservation,
            } => self.action_append(&session, &branch, &expected_head, &reservation),
            Action::Read {
                session,
                branch,
                snapshot_head,
                after,
                limit,
            } => self.action_read(&session, &branch, &snapshot_head, after.as_ref(), limit),
            Action::ReserveBranch {
                session,
                kind,
                source_branch,
                target_event,
            } => self.action_reserve_branch(&session, kind, &source_branch, &target_event),
            Action::BranchCommit {
                session,
                kind,
                source_branch,
                target_event,
                reservation,
            } => self.action_branch_commit(
                &session,
                kind,
                &source_branch,
                &target_event,
                &reservation,
            ),
            Action::LoadEvent {
                session,
                branch,
                event,
            } => self.action_load_event(&session, &branch, &event),
            Action::ReadPayloads {
                session,
                branch,
                snapshot_head,
                after,
                limit,
            } => {
                self.action_read_payloads(&session, &branch, &snapshot_head, after.as_ref(), limit)
            }
            Action::ReadWindow {
                session,
                branch,
                snapshot_head,
                before,
                limit,
            } => self.action_read_window(&session, &branch, &snapshot_head, before.as_ref(), limit),
        }
    }
}

fn admission_error(error: AdmissionError) -> SessionTaskError {
    match error {
        AdmissionError::ResourceCapacity | AdmissionError::OperationCapacity => {
            SessionTaskError::Admission
        }
    }
}

impl PackTaskActor for SessionActor {
    type Request = SessionRequest;
    type Operation = SessionOperation;
    type Pull = SessionPull;
    type Error = SessionTaskError;

    fn is_available(&self) -> bool {
        true
    }

    fn is_fatal(error: Self::Error) -> bool {
        matches!(error, SessionTaskError::Storage)
    }

    async fn invoke(&mut self, request: &Self::Request) -> Result<Self::Operation, Self::Error> {
        let request = request.clone();
        let core = Arc::clone(&self.core);
        blocking_session(move || core.lock().expect("session actor").invoke_sync(&request)).await?
    }

    async fn pull(&mut self, operation: &mut Self::Operation) -> Result<Self::Pull, Self::Error> {
        let core = Arc::clone(&self.core);
        let inner = Arc::clone(&operation.inner);
        blocking_session(move || {
            let mut op = inner.lock().expect("session operation");
            core.lock().expect("session actor").pull_sync(&mut op)
        })
        .await?
    }

    async fn drop_operation(&mut self, _operation: Self::Operation) -> Result<(), Self::Error> {
        Ok(())
    }

    fn take_admission(operation: &mut Self::Operation) -> Option<TaskOperationAdmission> {
        operation
            .inner
            .lock()
            .expect("session operation")
            .admission
            .take()
    }
}

fn blocking_session<T: Send + 'static>(
    job: impl FnOnce() -> T + Send + 'static,
) -> impl std::future::Future<Output = Result<T, SessionTaskError>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let value = job();
        let _ = tx.send(value);
    });
    async move { rx.await.map_err(|_| SessionTaskError::Storage) }
}
