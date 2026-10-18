//! Destructive conversation operations, delegated to the driver: a recall
//! walks older pages through a scoped reply until it finds the rewind
//! target, and a session delete forgets its bindings only after the core
//! confirms the row went away.
use gpui_kit::Context;

use super::Workspace;

impl Workspace {
    pub(crate) fn can_rewind_message(&self, index: usize) -> bool {
        self.driver.can_rewind_message(index)
    }

    /// Rewinds to before this message and restores its text to the composer.
    pub(crate) fn on_recall_message(&mut self, index: usize, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.recall_message(index, fx), cx);
    }

    /// Deletes one session straight from its row's trash button. The row
    /// control is a deliberate hover target, so no confirmation dialog.
    pub(crate) fn on_delete_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        self.driver.delete_session(session_id);
        cx.notify();
    }
}
