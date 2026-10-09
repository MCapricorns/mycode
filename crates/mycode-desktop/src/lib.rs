//! GPUI desktop frontend for MYCode.
//!
//! The application core lives in `mycode-app`: a background
//! [`mycode_app::CoreBridge`] thread owns the sessions, model turns, tools,
//! and credentials. The pure [`view_model`] holds every UI state transition,
//! and the GPUI layer in [`ui`] turns that state into elements and sends
//! commands back. This crate still chooses folders, reads git status for the
//! changes panel, and forwards settings edits, including secrets. It does not
//! run a model turn or a tool itself.
mod git_status;
pub(crate) mod i18n;
pub mod ui;
pub mod view_model;
pub mod workspace;
