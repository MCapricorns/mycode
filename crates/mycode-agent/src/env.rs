//! The per-turn environment: everything one turn of the agent loop needs
//! from its surroundings. See `docs/agent.md`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use mycode_core::Provider;
use mycode_core::events::AgentEvent;
use mycode_tools::ToolRegistry;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::hooks::HookRunner;

/// Texts the user steered into a running turn.
///
/// The desktop pushes while the model is streaming or a tool is running.
/// The agent drains the inbox only at a loop boundary, so a steer never
/// cancels the in-flight response or its subagents.
#[derive(Debug, Default)]
pub struct SteerInbox {
    pending: Mutex<Vec<String>>,
}

impl SteerInbox {
    /// An empty inbox.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues one non-empty steer. Blank text is ignored.
    pub fn push(&self, text: impl Into<String>) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(text);
    }

    /// Takes every queued steer, leaving the inbox empty.
    #[must_use]
    pub fn drain(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

/// Everything one turn needs: the model provider, tools, hooks,
/// cancellation, and the event bus.
///
/// The agent itself stays UI-free and session-free: all ambient
/// dependencies flow in through this struct, freshly borrowable per
/// `prompt()` call. `cancel` is the *caller's* token; the agent derives
/// a child token from it per turn so cancelling that child does not
/// cancel the parent.
pub struct TurnEnv<'a> {
    /// Host-backed provider port to stream from.
    pub provider: &'a dyn Provider,
    /// Tool registry the model's calls dispatch through.
    pub tools: &'a ToolRegistry,
    /// Request hook. Compaction rewrites the provider request before send.
    pub hooks: &'a HookRunner,
    /// Cooperative turn cancellation. Firing it aborts the in-flight
    /// turn: the current stream terminates with `Cancelled`, thinking
    /// and text already received are kept with an interruption line,
    /// the turn ends with [`TurnOutcome::Aborted`], and state stays
    /// consistent. Incomplete tool calls are not executed.
    ///
    /// [`TurnOutcome::Aborted`]: mycode_core::events::TurnOutcome::Aborted
    pub cancel: CancellationToken,
    /// Fan-out bus for Agent events (UI and telemetry subscribe).
    pub events: broadcast::Sender<AgentEvent>,
    /// Working directory tools resolve relative paths against.
    pub cwd: PathBuf,
    /// Additional workspace folders. Absolute tool paths under one of
    /// these roots are prepared against that root instead of `cwd`.
    pub extra_roots: Vec<PathBuf>,
    /// Host-published read-only anchor roots outside the workspace (the
    /// skills catalog). Only read-only file tools anchor here; `edit` and
    /// `write` refuse them.
    pub read_roots: Vec<PathBuf>,
    /// Steers to fold into this turn at the next loop boundary.
    /// `None` for a turn that does not accept them (a nested agent).
    pub steer: Option<Arc<SteerInbox>>,
    /// Session key for the warm `run_code` interpreter. `None` cold-starts
    /// Python on every call. Scout and the main agent still use separate
    /// processes: isolation is part of the pool key, not this string.
    /// OS sandboxing ignores this key and cold-starts every call when the
    /// effective flag is on.
    pub ptc_session: Option<String>,
    /// Effective OS helper for `shell` and `run_code`.
    ///
    /// The settings default is on. The host sets this from the effective
    /// value, which is off on Windows so shell and `run_code` still run.
    /// `true` still fail-closes when the helper is missing.
    pub os_sandbox: bool,
}

impl<'a> TurnEnv<'a> {
    /// Wire up an environment with safe defaults: a fresh cancellation
    /// token, a private 256-slot event channel, and the process cwd.
    /// Override with the `with_*` builders.
    ///
    /// Registered schema-valid tools dispatch directly. No permission
    /// callback or grant state is required.
    pub fn new(provider: &'a dyn Provider, tools: &'a ToolRegistry, hooks: &'a HookRunner) -> Self {
        Self {
            provider,
            tools,
            hooks,
            cancel: CancellationToken::new(),
            events: broadcast::channel(256).0,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            extra_roots: Vec::new(),
            read_roots: Vec::new(),
            steer: None,
            ptc_session: None,
            os_sandbox: false,
        }
    }

    /// Enable or disable the best-effort OS helper for `shell` and `run_code`.
    pub fn with_os_sandbox(mut self, enabled: bool) -> Self {
        self.os_sandbox = enabled;
        self
    }

    /// Use this cancellation token (builder style).
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Publish Agent events on this broadcast sender (builder style).
    pub fn with_events(mut self, events: broadcast::Sender<AgentEvent>) -> Self {
        self.events = events;
        self
    }

    /// Set the tool working directory (builder style).
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = cwd.into();
        self
    }

    /// Allow absolute tool paths under these extra workspace folders.
    pub fn with_extra_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.extra_roots = roots;
        self
    }

    /// Let read-only file tools anchor absolute paths under these roots.
    pub fn with_read_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.read_roots = roots;
        self
    }

    /// Accept steers into this turn. Drained at loop boundaries only.
    pub fn with_steer(mut self, steer: Option<Arc<SteerInbox>>) -> Self {
        self.steer = steer;
        self
    }

    /// Reuse one warm Python interpreter for `run_code` in this session.
    ///
    /// An empty key leaves calls on the cold-start path.
    pub fn with_ptc_session(mut self, session: impl Into<String>) -> Self {
        let session = session.into();
        self.ptc_session = if session.is_empty() {
            None
        } else {
            Some(session)
        };
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steer_inbox_survives_a_poisoned_lock() {
        let inbox = Arc::new(SteerInbox::new());
        inbox.push("before");
        let poisoner = Arc::clone(&inbox);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.pending.lock().unwrap();
            panic!("poison the inbox");
        })
        .join();
        assert!(inbox.pending.is_poisoned());

        inbox.push("after");
        inbox.push("   ");
        assert_eq!(inbox.drain(), vec!["before".to_owned(), "after".to_owned()]);
        assert!(inbox.drain().is_empty());
    }
}
