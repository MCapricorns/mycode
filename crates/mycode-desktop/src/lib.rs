//! GPUI desktop frontend for MYCode.
//!
//! The application core lives in `mycode-app`: a background
//! [`mycode_app::CoreBridge`] thread owns the sessions, model turns, tools,
//! and credentials. The shared UI-agnostic product layer lives in
//! `mycode-frontend`: [`view_model`] holds every state transition, and the
//! GPUI layer in [`ui`] turns that state into elements and sends commands
//! back. This crate still chooses folders and owns the window chrome. It
//! does not run a model turn or a tool itself.
pub mod instance;
pub mod ui;
pub mod workspace;

pub use mycode_frontend::{cli, git_status, i18n, view_model};

/// Returns the wall-clock duration since the Unix epoch, saturating at zero
/// when the system clock predates the epoch.
pub(crate) use mycode_app::unix_now;
