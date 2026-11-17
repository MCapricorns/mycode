//! Shared admission budget for the model requests in one user turn.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::core::MycodeError;

/// Default combined request allowance for a parent turn and its children.
pub const DEFAULT_MAX_MODEL_REQUESTS: u32 = 64;

/// Counts provider invocations across one parent turn and every child run.
///
/// Admission is atomic: concurrent children cannot each spend the last slot.
/// Completed requests never refund their slots, including failures and context
/// overflow retries. A new user turn receives a fresh budget. Provider-internal
/// HTTP retries and auxiliary compaction/title requests are not separate agent
/// invocations and do not consume additional slots.
#[derive(Debug)]
pub struct TurnBudget {
    max_model_requests: u32,
    started: AtomicU32,
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_MODEL_REQUESTS)
    }
}

impl TurnBudget {
    /// Creates an allowance. Zero is treated as one, never as unlimited.
    pub fn new(max_model_requests: u32) -> Self {
        Self {
            max_model_requests: max_model_requests.max(1),
            started: AtomicU32::new(0),
        }
    }

    /// Total provider invocations admitted by this turn, children included.
    pub fn model_requests_started(&self) -> u32 {
        self.started.load(Ordering::Relaxed)
    }

    /// Reserves one provider invocation before it starts.
    ///
    /// # Errors
    ///
    /// Returns [`MycodeError::BudgetExceeded`] when the shared allowance is
    /// spent. The caller must preserve prior results and end the turn without
    /// submitting another request. This never cancels the user's parent token.
    pub fn try_start_request(&self) -> Result<(), MycodeError> {
        let mut started = self.started.load(Ordering::Relaxed);
        while started < self.max_model_requests {
            match self.started.compare_exchange_weak(
                started,
                started + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(()),
                Err(current) => started = current,
            }
        }
        Err(MycodeError::BudgetExceeded(format!(
            "the shared limit of {} model requests was reached. Completed work is \
preserved; continue the task to start a new turn with a fresh allowance.",
            self.max_model_requests
        )))
    }
}
