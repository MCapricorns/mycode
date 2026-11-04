//! GPUI windows, rendering and native desktop integration.
//!
//! Application behavior lives in `mycode`. The desktop translates input into
//! `mycode::frontend` actions and renders its state; `mycode::app::CoreBridge`
//! owns the thread that executes sessions, model turns and tools.
pub mod instance;
pub mod ui;
pub mod workspace;

pub use mycode::frontend::{git_status, i18n, launch, view_model};
