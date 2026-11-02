//! Atomic Host admission and the serialized first-party task-actor protocol.
//!
//! The session actor runs as the only task family on this substrate: durable
//! work is admitted through the admission ledger and every operation drives
//! to exactly one terminal pull under a bounded deadline.
pub(crate) mod admission;
/// Serialized first-party task actor protocol shared by built-in services.
pub(crate) mod task_worker;

pub(crate) use admission::{AdmissionError, MAX_OPEN_OPERATIONS, ResourcePermit};
pub(crate) use task_worker::{
    PackTaskActor, TaskActorClient, TaskActorError, TaskCloseSignal, TaskOperationAdmission,
};
