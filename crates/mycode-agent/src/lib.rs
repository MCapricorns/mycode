//! `mycode-agent` — the UI-free agent runtime: the agent loop plus the
//! durable session ledger a conversation is recorded in.
//!
//! The two layers do not depend on each other. [`Agent`] owns the
//! conversation state; [`TurnEnv`] injects provider, tool registry, hooks,
//! cancellation, and the event bus; [`HookRunner`] owns the before-request
//! history-compaction rewrite.

pub mod agent;
pub mod env;
pub mod hooks;
mod prompt;
pub mod session;
mod turn;

// Chain re-export: successors see every lower layer through this crate only.
pub use mycode_tools::*;

pub use agent::{Agent, AgentConfig};
pub use env::{SteerInbox, TurnEnv};
pub use hooks::HookRunner;
pub use prompt::{build_system_prompt, shell_tool_name};
