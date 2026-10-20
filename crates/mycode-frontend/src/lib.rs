//! `mycode-frontend` — the UI-agnostic product layer shared by MYCode
//! frontends.
//!
//! Everything that is not pixels lives here: the view-model state machine
//! ([`view_model::WorkspaceState`] plus the [`view_model::FrontendAction`]
//! reducer), interface localization, read-only git status for the changes
//! panel, the keyboard-shortcut list both settings pages show, and the
//! pre-window CLI argument vocabulary. A frontend renders the
//! state and feeds actions back; when product behavior changes, this crate
//! is where the change lands so every frontend picks it up at once.
//!
//! The bridge to the application core is [`mycode_app::CoreBridge`]: commands
//! in, replies and streaming events out. Frontends never talk to sessions,
//! providers, tools, or credentials directly.

pub mod cli;
pub mod driver;
pub mod forms;
pub mod git_status;
pub mod highlight;
pub mod i18n;
pub mod picker;
pub mod shortcuts;
pub mod stream;
mod theme;
pub mod view_model;

pub use theme::{normalize_font_size, normalize_theme};

/// Returns the wall-clock duration since the Unix epoch, saturating at zero
/// when the system clock predates the epoch.
pub use mycode_app::unix_now;
