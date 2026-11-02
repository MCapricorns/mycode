//! MYCode runtime and application state, independent of GPUI.
//!
//! Modules own the conversation model, configuration, provider protocols,
//! tools, agent loop, application services and presentation state. Callers
//! import the owning module directly; there is no transitive facade chain.

// Keep the Windows CRT link-search build script active for every consumer.
#[cfg(windows)]
use vc_ltl as _;

/// One allocator for the runtime, desktop and their test executables.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub mod agent;
pub mod app;
pub mod config;
pub mod core;
pub mod frontend;
pub mod providers;
pub mod tools;
