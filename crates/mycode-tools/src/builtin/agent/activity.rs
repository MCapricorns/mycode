//! Wait for scoped blocking workers and detached code dispatchers after cancellation.
use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Notify;

#[derive(Default)]
pub struct AgentActivity {
    live: AtomicUsize,
    changed: Notify,
}
tokio::task_local! { static CURRENT: Arc<AgentActivity>; }
impl AgentActivity {
    pub async fn scope<F: Future>(self: &Arc<Self>, future: F) -> F::Output {
        CURRENT.scope(self.clone(), future).await
    }
    pub async fn settled(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.live.load(Ordering::Acquire) == 0 {
                return;
            }
            changed.await;
        }
    }
}
/// Await native workers spawned in the current run before worktree handoff.
pub async fn settle_agent_activity() {
    if let Ok(activity) = CURRENT.try_with(Arc::clone) {
        activity.settled().await;
    }
}

pub(crate) struct ActivityGuard(Arc<AgentActivity>);
impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}
pub(crate) fn current_guard() -> Option<ActivityGuard> {
    CURRENT
        .try_with(|activity| {
            activity.live.fetch_add(1, Ordering::AcqRel);
            ActivityGuard(activity.clone())
        })
        .ok()
}
/// Capture the current activity before spawning a task. The guard also covers
/// dispatcher startup, so nested workers cannot appear after settlement.
pub fn inherit_agent_activity<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let guard = current_guard();
    async move {
        match guard {
            Some(guard) => {
                let activity = guard.0.clone();
                let result = activity.scope(future).await;
                drop(guard);
                result
            }
            None => future.await,
        }
    }
}
