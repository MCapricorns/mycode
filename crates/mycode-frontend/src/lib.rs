//! `mycode-frontend` — the UI-agnostic product layer shared by MYCode frontends.
//!
//! Everything that is not pixels: the view-model state machine
//! ([`view_model`]), i18n, read-only git status, shortcuts, and the pre-window
//! launch steps. A frontend renders the state and feeds actions back; product
//! behavior changes land here so every frontend picks them up at once.

pub mod driver;
pub mod forms;
pub mod git_status;
pub mod highlight;
pub mod i18n;
pub mod launch;
pub mod picker;
pub mod shortcuts;
pub mod stream;
mod theme;
pub mod view_model;

// Chain re-export: successors see every lower layer through this crate only.
pub use mycode_app::*;

pub use theme::{normalize_font_size, normalize_theme};
