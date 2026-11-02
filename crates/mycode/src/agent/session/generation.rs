//! Host-owned generation fence for atomic authority publication.
//!
//! One fence owns one authority's generation lifecycle. Preparation cannot
//! admit Host work. Publication and retirement use one atomic phase-plus-count
//! state so retirement linearizes with every activity reservation before
//! quiescent Store cleanup.
//!
//! Closing the publication only rejects new admissions; every fence must
//! additionally be retired (and drained via [`GenerationFence::wait_drained`])
//! before its Store ownership is reclaimed.
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as SyncMutex, MutexGuard};

use tokio::sync::Notify;

pub(crate) struct GenerationFence {
    closed: AtomicBool,
    state: AtomicUsize,
    drained: Notify,
    commit: SyncMutex<()>,
}

// The low two bits carry the phase so retirement and activity admission share
// one CAS linearization point. All remaining bits carry the activity count.
const GENERATION_PHASE_MASK: usize = 0b11;
const GENERATION_PREPARING: usize = 0;
const GENERATION_CURRENT: usize = 1;
const GENERATION_RETIRED: usize = 2;
const GENERATION_ACTIVITY_INCREMENT: usize = GENERATION_PHASE_MASK + 1;
const MAX_GENERATION_ACTIVITIES: usize = usize::MAX >> 2;

impl GenerationFence {
    pub(crate) fn new() -> Self {
        Self {
            closed: AtomicBool::new(false),
            state: AtomicUsize::new(GENERATION_PREPARING),
            drained: Notify::new(),
            commit: SyncMutex::new(()),
        }
    }

    /// Returns whether the publication authority is finally closed.
    pub(crate) fn publication_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Closes the shared publication authority forever.
    pub(crate) fn close_publication(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    pub(crate) fn enter(self: &Arc<Self>) -> Option<GenerationActivity> {
        if self.publication_closed() {
            return None;
        }
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if generation_phase(state) != GENERATION_CURRENT
                || generation_activity_count(state) == MAX_GENERATION_ACTIVITIES
            {
                return None;
            }
            match self.state.compare_exchange_weak(
                state,
                state + GENERATION_ACTIVITY_INCREMENT,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => state = observed,
            }
        }
        if self.publication_closed() {
            self.release();
            return None;
        }
        Some(GenerationActivity {
            fence: Arc::clone(self),
        })
    }

    pub(crate) fn mark_current(&self) {
        self.state
            .compare_exchange(
                GENERATION_PREPARING,
                GENERATION_CURRENT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("a generation fence becomes current exactly once");
    }

    pub(crate) fn mark_retired(&self) {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            match generation_phase(state) {
                GENERATION_RETIRED => return,
                GENERATION_PREPARING | GENERATION_CURRENT => {}
                _ => unreachable!("generation fence phase uses two frozen bits"),
            }
            let retired = (state & !GENERATION_PHASE_MASK) | GENERATION_RETIRED;
            match self.state.compare_exchange_weak(
                state,
                retired,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => state = observed,
            }
        }
    }

    /// Waits until every admitted activity has released its reservation.
    ///
    /// Retirement only rejects new admissions; consumers must await this
    /// quiescence point before reclaiming the generation's Store ownership.
    pub(crate) async fn wait_drained(&self) {
        loop {
            // Subscribe before checking the count so a drain broadcast is
            // retained by this future even before its first poll.
            let notified = self.drained.notified();
            if generation_activity_count(self.state.load(Ordering::Acquire)) == 0 {
                return;
            }
            notified.await;
        }
    }

    fn release(&self) {
        let previous = self
            .state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (generation_activity_count(active) > 0)
                    .then(|| active - GENERATION_ACTIVITY_INCREMENT)
            })
            .expect("a generation activity releases its reservation exactly once");
        if generation_activity_count(previous) == 1 {
            // Service clones can await the same quiescence transition.
            self.drained.notify_waiters();
        }
    }
}

const fn generation_phase(state: usize) -> usize {
    state & GENERATION_PHASE_MASK
}

const fn generation_activity_count(state: usize) -> usize {
    state >> 2
}

/// One admitted activity reservation on a current generation fence.
pub(crate) struct GenerationActivity {
    fence: Arc<GenerationFence>,
}

impl GenerationActivity {
    /// Begins one exclusive commit window on the still-current generation.
    ///
    /// The returned guard linearizes with retirement: a fence cannot retire
    /// while the commit window is open.
    pub(crate) fn begin_commit(&self) -> Result<GenerationCommit<'_>, GenerationCommitError> {
        let commit = self
            .fence
            .commit
            .lock()
            .map_err(|_| GenerationCommitError::Unavailable)?;
        if self.fence.publication_closed()
            || generation_phase(self.fence.state.load(Ordering::Acquire)) != GENERATION_CURRENT
        {
            return Err(GenerationCommitError::Stale);
        }
        Ok(GenerationCommit { _commit: commit })
    }
}

/// Holds one generation's exclusive commit window.
pub(crate) struct GenerationCommit<'a> {
    _commit: MutexGuard<'a, ()>,
}

/// Reports why a generation commit window could not open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GenerationCommitError {
    /// The generation retired before the commit began.
    Stale,
    /// The commit mutex was poisoned.
    Unavailable,
}

impl Drop for GenerationActivity {
    fn drop(&mut self) {
        self.fence.release();
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};

    use super::GenerationFence;

    #[test]
    fn drain_wakes_every_waiter_only_after_the_last_activity() {
        let fence = Arc::new(GenerationFence::new());
        fence.mark_current();
        let first_activity = fence.enter().expect("first admitted activity");
        let last_activity = fence.enter().expect("last admitted activity");
        fence.mark_retired();
        assert!(fence.enter().is_none(), "retirement rejects new activity");

        let mut first_waiter = std::pin::pin!(fence.wait_drained());
        let mut second_waiter = std::pin::pin!(fence.wait_drained());
        let mut context = Context::from_waker(Waker::noop());
        assert!(first_waiter.as_mut().poll(&mut context).is_pending());
        assert!(second_waiter.as_mut().poll(&mut context).is_pending());

        drop(first_activity);
        assert!(first_waiter.as_mut().poll(&mut context).is_pending());
        assert!(second_waiter.as_mut().poll(&mut context).is_pending());

        drop(last_activity);
        assert_eq!(first_waiter.as_mut().poll(&mut context), Poll::Ready(()));
        assert_eq!(second_waiter.as_mut().poll(&mut context), Poll::Ready(()));
        let mut late_waiter = std::pin::pin!(fence.wait_drained());
        assert_eq!(late_waiter.as_mut().poll(&mut context), Poll::Ready(()));
    }
}
