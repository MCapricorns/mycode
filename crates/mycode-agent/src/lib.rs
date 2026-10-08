//! `mycode-agent` — the UI-free agent runtime: the double loop plus the
//! durable session ledger a conversation is recorded in.
//!
//! The two layers do not depend on each other. The loop below runs happily
//! against no storage at all; [`session`]
//! is the event-sourced ledger a host writes turns into, on its own task
//! runtime inside a generation fence. They share a crate because they share
//! a lifetime — one conversation, one durable history — and nothing else in
//! the workspace needs one without the other.
//!
//! ```text
//! caller ──Message──► Agent::prompt(msg, &TurnEnv)
//!                        │
//!                        ▼ loop
//!        build request → provider.stream → mirror deltas as
//!        AgentEvent stream → dispatch registered tools → write
//!        results back, until the model stops calling tools
//!                        │
//!                        ▼
//!        env.cancel ends the turn with TurnOutcome::Aborted
//! ```
//!
//! * [`Agent`] owns the conversation state.
//! * [`TurnEnv`] injects everything ambient — provider, tool registry,
//!   hooks, cancellation, and the event bus. Registered schema-valid
//!   tools execute directly; no permission callback is required.
//! * [`HookRunner`] owns the before-request rewrite (history compaction).
//!   There is no before-tool observer and no file snapshot.

pub mod agent;
pub mod env;
pub mod hooks;
mod prompt;
pub mod session;
mod turn;

pub use agent::{Agent, AgentConfig};
pub use env::TurnEnv;
pub use hooks::HookRunner;
pub use prompt::build_system_prompt;
