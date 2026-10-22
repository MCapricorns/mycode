//! GPUI desktop frontend for MYCode.
//!
//! The application core lives in `mycode-app` (a [`mycode_frontend::CoreBridge`]
//! thread owns sessions and turns); the shared product layer in
//! `mycode-frontend`. This crate owns windows and chrome; it never runs a
//! model turn or a tool itself.
pub mod instance;
pub mod ui;
pub mod workspace;

pub use mycode_frontend::{git_status, i18n, launch, view_model};

pub(crate) use mycode_frontend::unix_now;
