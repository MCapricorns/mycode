//! The live handles one turn publishes: the cancel token Escape fires and
//! the inbox steers are pushed into, each removed from its registry when the
//! turn ends.

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::protocol::CHAT_CANCELLED;
use crate::state::CoreState;

/// The live handles one turn publishes for the life of the turn: the cancel
/// token Escape fires, and the inbox steers are pushed into.
pub(crate) struct TurnControl {
    pub(super) cancel: CancellationToken,
    pub(super) steer: Arc<mycode_agent::SteerInbox>,
    _cancel_guard: CancelGuard,
    steer_guard: Option<SteerGuard>,
}

impl TurnControl {
    /// Publishes a fresh token and inbox for `session_id`, replacing any
    /// earlier turn's entries. The caller cancels that earlier turn.
    pub(crate) fn register(state: &CoreState, session_id: &str) -> Self {
        let cancel = CancellationToken::new();
        let cancel_guard = CancelGuard::register(state.turn_cancels.clone(), session_id, &cancel);
        let steer = Arc::new(mycode_agent::SteerInbox::new());
        let steer_guard = SteerGuard::register(
            Arc::clone(&state.steer_inboxes),
            session_id,
            Arc::clone(&steer),
        );
        Self {
            cancel,
            steer,
            _cancel_guard: cancel_guard,
            steer_guard: Some(steer_guard),
        }
    }

    /// Unpublishes the steer inbox, then takes whatever is still in it.
    ///
    /// `Steer` pushes while holding the inbox map lock, and the guard's drop
    /// removes the entry under that lock, so no steer lands after the drain.
    pub(super) fn retire_steers(&mut self) -> Vec<String> {
        drop(self.steer_guard.take());
        self.steer.drain()
    }

    /// Publishes only the cancel token for `/compact` (no steer inbox).
    pub(crate) fn register_cancel_only(state: &CoreState, session_id: &str) -> Self {
        let cancel = CancellationToken::new();
        let cancel_guard = CancelGuard::register(state.turn_cancels.clone(), session_id, &cancel);
        Self {
            cancel,
            steer: Arc::new(mycode_agent::SteerInbox::new()),
            _cancel_guard: cancel_guard,
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

/// Removes one session's turn token from the cancel registry on scope exit.
struct CancelGuard {
    cancels: Arc<parking_lot::Mutex<HashMap<String, Arc<CancellationToken>>>>,
    session_id: String,
    token: Arc<CancellationToken>,
}

impl CancelGuard {
    fn register(
        cancels: Arc<parking_lot::Mutex<HashMap<String, Arc<CancellationToken>>>>,
        session_id: &str,
        token: &CancellationToken,
    ) -> Self {
        // The registry is keyed by session id only, so a second turn for the
        // same session replaces the entry; the guard keeps its own token to
        // avoid removing the successor's live token on drop.
        let token = Arc::new(token.clone());
        let mut map = cancels.lock();
        map.insert(session_id.to_owned(), token.clone());
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
            .is_some_and(|live| Arc::ptr_eq(live, &self.token))
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
