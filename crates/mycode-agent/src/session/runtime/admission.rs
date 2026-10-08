//! Atomic Host-visible resource and operation admission.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Maximum live Host-visible resources admitted by one owner.
pub const MAX_LIVE_RESOURCES: usize = 4_096;
/// Maximum open operations admitted by one owner.
pub const MAX_OPEN_OPERATIONS: usize = 1_024;

/// Reports a saturated Host admission class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    /// The live Host-visible resource limit was reached.
    #[error("Host-visible resource capacity is exhausted")]
    ResourceCapacity,
    /// The open operation limit was reached.
    #[error("open operation capacity is exhausted")]
    OperationCapacity,
}

#[derive(Clone, Debug)]
pub(crate) struct AdmissionLedger {
    counters: Arc<AdmissionCounters>,
}

impl AdmissionLedger {
    pub(crate) fn new() -> Self {
        Self {
            counters: Arc::new(AdmissionCounters::default()),
        }
    }

    pub(crate) fn admit_resource(&self) -> Result<ResourcePermit, AdmissionError> {
        if !reserve(&self.counters.live_resources, MAX_LIVE_RESOURCES) {
            return Err(AdmissionError::ResourceCapacity);
        }
        Ok(ResourcePermit {
            counters: Arc::clone(&self.counters),
        })
    }

    pub(crate) fn open_operation(&self) -> Result<OperationPermit, AdmissionError> {
        if !reserve(&self.counters.open_operations, MAX_OPEN_OPERATIONS) {
            return Err(AdmissionError::OperationCapacity);
        }
        Ok(OperationPermit {
            counters: Arc::clone(&self.counters),
        })
    }
}

/// Holds one live Host-visible resource admission.
#[derive(Debug)]
#[must_use = "dropping the permit releases its resource admission"]
pub struct ResourcePermit {
    counters: Arc<AdmissionCounters>,
}

impl Drop for ResourcePermit {
    fn drop(&mut self) {
        release(&self.counters.live_resources);
    }
}

#[derive(Debug)]
pub(crate) struct OperationPermit {
    counters: Arc<AdmissionCounters>,
}

impl Drop for OperationPermit {
    fn drop(&mut self) {
        release(&self.counters.open_operations);
    }
}

#[derive(Debug, Default)]
struct AdmissionCounters {
    live_resources: AtomicUsize,
    open_operations: AtomicUsize,
}

fn reserve(counter: &AtomicUsize, maximum: usize) -> bool {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1).filter(|next| *next <= maximum)
        })
        .is_ok()
}

fn release(counter: &AtomicUsize) {
    let previous = counter.fetch_sub(1, Ordering::AcqRel);
    assert!(
        previous > 0,
        "an admission permit must release exactly once"
    );
}
