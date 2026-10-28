//! The live handles one turn publishes: the cancel token Escape fires and
//! the inbox steers are pushed into. Both leave their registries before the
//! turn reports how it ended.

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::protocol::CHAT_CANCELLED;
use crate::state::{CoreState, LiveWork, WorkKind};

/// The live handles one turn publishes for the life of the turn: the cancel
/// token Escape fires, and the inbox steers are pushed into.
pub(crate) struct TurnControl {
    pub(super) cancel: CancellationToken,
    pub(super) steer: Arc<mycode_agent::SteerInbox>,
    cancel_guard: Option<CancelGuard>,
    steer_guard: Option<SteerGuard>,
}

impl TurnControl {
    /// Publishes a fresh token and inbox for `session_id`, replacing any
    /// earlier turn's entries. The caller cancels that earlier turn.
    pub(crate) fn register(state: &CoreState, session_id: &str) -> Self {
        let cancel = CancellationToken::new();
        let cancel_guard = CancelGuard::register(
            state.turn_cancels.clone(),
            session_id,
            &cancel,
            WorkKind::Turn,
        );
        let steer = Arc::new(mycode_agent::SteerInbox::new());
        let steer_guard = SteerGuard::register(
            Arc::clone(&state.steer_inboxes),
            session_id,
            Arc::clone(&steer),
        );
        Self {
            cancel,
            steer,
            cancel_guard: Some(cancel_guard),
            steer_guard: Some(steer_guard),
        }
    }

    /// Unpublishes the cancel token and the steer inbox, then takes whatever
    /// is still in the inbox.
    ///
    /// Runs before the terminal event goes out. The UI reopens the session
    /// when it hears the turn end, and a token still published then reads
    /// as a turn still running: the composer stuck on Stop, every send
    /// steering a turn that is gone.
    ///
    /// `Steer` pushes while holding the inbox map lock, and the guard's drop
    /// removes the entry under that lock, so no steer lands after the drain.
    pub(super) fn retire(&mut self) -> Vec<String> {
        drop(self.cancel_guard.take());
        drop(self.steer_guard.take());
        self.steer.drain()
    }

    /// Publishes only the cancel token for `/compact` (no steer inbox).
    pub(crate) fn register_cancel_only(state: &CoreState, session_id: &str) -> Self {
        let cancel = CancellationToken::new();
        let cancel_guard = CancelGuard::register(
            state.turn_cancels.clone(),
            session_id,
            &cancel,
            WorkKind::Compaction,
        );
        Self {
            cancel,
            steer: Arc::new(mycode_agent::SteerInbox::new()),
            cancel_guard: Some(cancel_guard),
            steer_guard: None,
        }
    }

    /// Fails a setup step with the quiet cancel once the user has cancelled.
    pub(super) fn check(&self) -> Result<(), String> {
        if self.cancel.is_cancelled() {
            Err(CHAT_CANCELLED.to_owned())
        } else {
            Ok(())
        }
    }

    /// Runs one setup step unless the user cancels first. Only steps with no
    /// durable effect go through here, so dropping them loses nothing.
    pub(super) async fn unless_cancelled<T>(
        &self,
        step: impl std::future::Future<Output = T>,
    ) -> Result<T, String> {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(CHAT_CANCELLED.to_owned()),
            value = step => Ok(value),
        }
    }
}

impl Drop for TurnControl {
    fn drop(&mut self) {
        // A task dropped before end_turn never retires its controls. Stop
        // its tools and children before the guards unpublish the handles.
        if self.cancel_guard.is_some() {
            self.cancel.cancel();
        }
    }
}

/// Removes one session's turn token from the cancel registry on scope exit.
struct CancelGuard {
    cancels: Arc<parking_lot::Mutex<HashMap<String, LiveWork>>>,
    session_id: String,
    token: Arc<CancellationToken>,
}

impl CancelGuard {
    fn register(
        cancels: Arc<parking_lot::Mutex<HashMap<String, LiveWork>>>,
        session_id: &str,
        token: &CancellationToken,
        kind: WorkKind,
    ) -> Self {
        // The registry is keyed by session id only, so a second turn for the
        // same session replaces the entry; the guard keeps its own token to
        // avoid removing the successor's live token on drop.
        let token = Arc::new(token.clone());
        let mut map = cancels.lock();
        map.insert(
            session_id.to_owned(),
            LiveWork {
                token: token.clone(),
                kind,
            },
        );
        drop(map);
        Self {
            cancels,
            session_id: session_id.to_owned(),
            token,
        }
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        // Remove the entry only while it still maps to this turn's token.
        let mut map = self.cancels.lock();
        if map
            .get(&self.session_id)
            .is_some_and(|live| Arc::ptr_eq(&live.token, &self.token))
        {
            map.remove(&self.session_id);
        }
    }
}

/// Publishes one session's steer inbox for the life of its turn.
struct SteerGuard {
    inboxes: Arc<parking_lot::Mutex<HashMap<String, Arc<mycode_agent::SteerInbox>>>>,
    session_id: String,
    inbox: Arc<mycode_agent::SteerInbox>,
}

impl SteerGuard {
    fn register(
        inboxes: Arc<parking_lot::Mutex<HashMap<String, Arc<mycode_agent::SteerInbox>>>>,
        session_id: &str,
        inbox: Arc<mycode_agent::SteerInbox>,
    ) -> Self {
        let mut map = inboxes.lock();
        map.insert(session_id.to_owned(), Arc::clone(&inbox));
        drop(map);
        Self {
            inboxes,
            session_id: session_id.to_owned(),
            inbox,
        }
    }
}

impl Drop for SteerGuard {
    fn drop(&mut self) {
        let mut map = self.inboxes.lock();
        if map
            .get(&self.session_id)
            .is_some_and(|live| Arc::ptr_eq(live, &self.inbox))
        {
            map.remove(&self.session_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropping_unretired_controls_cancels_children_and_preserves_successor_handles() {
        let root = std::env::temp_dir().join(format!(
            "mycode-control-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        let home = mycode_agent::HomeLayout::from_root(root.clone()).unwrap();
        let state = CoreState::new(home, Vec::new());
        let first = TurnControl::register(&state, "session");
        let child = first.cancel.child_token();
        let mut successor = TurnControl::register(&state, "session");
        drop(first);
        assert!(child.is_cancelled());
        assert!(!successor.cancel.is_cancelled());
        assert!(state.turn_cancels.lock().contains_key("session"));
        assert!(Arc::ptr_eq(
            state.steer_inboxes.lock().get("session").unwrap(),
            &successor.steer
        ));
        successor.steer.push("unread steer".to_owned());
        let late_child = successor.cancel.child_token();
        assert_eq!(successor.retire(), ["unread steer"]);
        drop(successor);
        assert!(
            !late_child.is_cancelled(),
            "a normally retired turn keeps late results"
        );
        assert!(state.turn_cancels.lock().is_empty());
        assert!(state.steer_inboxes.lock().is_empty());
        let service = state.service.clone();
        drop(state);
        service.shutdown().await;
        for _ in 0..100 {
            if !root.exists() || std::fs::remove_dir_all(&root).is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the session actor kept the temporary home open");
    }
}
