//! Presentation state and interaction logic without a GPUI dependency.
//!
//! The driver translates UI actions into application commands; reducers apply
//! replies and events to the view model. Launch handling, localization, Git
//! status, shortcuts and syntax highlighting are shared with the desktop.

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

pub use theme::{normalize_font_size, normalize_theme};
