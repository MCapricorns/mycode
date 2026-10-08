//! GPUI desktop frontend for MYCode.
//!
//! This crate renders and nothing else. The application core lives in
//! `mycode-app`: a background [`mycode_app::CoreBridge`] thread owns the
//! sessions, model turns, tools, and credentials, the pure [`view_model`]
//! holds every UI state transition, and the GPUI layer in [`ui`] turns that
//! state into elements and sends commands back. No session, provider, tool,
//! file path, or credential is handled here.
mod git_status;
pub(crate) mod i18n;
pub mod ui;
pub mod view_model;
pub mod workspace;
