//! Destructive conversation operations, delegated to the driver: a recall
//! walks older pages through a scoped reply until it finds the rewind
//! target, and a session delete forgets its bindings only after the core
//! confirms the row went away.
use gpui_kit::{ClipboardItem, Context};

use super::Workspace;
use super::toasts::ToastKind;
use crate::i18n::t;

impl Workspace {
    /// Copies an assistant reply from its card's copy action. The text
    /// arrives already converted to plain text by the transcript, so the
    /// clipboard never carries Markdown markers or rich formatting.
    pub(crate) fn on_copy_plain_text(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.push_toast(
            t("Copied as plain text", "已复制为纯文本"),
            ToastKind::Info,
            cx,
        );
    }

    pub(crate) fn can_rewind_message(&self, index: usize) -> bool {
        self.driver.can_rewind_message(index)
    }

    /// Rewinds to before this message and restores its text to the composer.
    pub(crate) fn on_recall_message(&mut self, index: usize, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.recall_message(index, fx), cx);
    }

    /// Deletes one session from its row's trash button on the second click.
    ///
    /// The first click arms the button for [`DELETE_CONFIRM_WINDOW`]. A
    /// delete cannot be undone, and one stray click on the hover target used
    /// to remove the whole conversation. Still no dialog: the second click
    /// lands on the same spot.
    pub(crate) fn on_delete_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.confirm_delete(&session_delete_key(session_id), cx) {
            self.driver.delete_session(session_id);
            cx.notify();
        }
    }

    /// The session whose trash button is waiting for its second click.
    pub(crate) fn armed_session_delete(&self) -> Option<&str> {
        self.armed_delete
            .as_deref()
            .and_then(|key| key.strip_prefix(SESSION_DELETE_PREFIX))
    }

    /// Whether a trash button keyed `key` is waiting for its second click.
    pub(crate) fn delete_armed(&self, key: &str) -> bool {
        self.armed_delete.as_deref() == Some(key)
    }

    /// Two-click confirmation shared by the irreversible row deletes.
    ///
    /// Returns `true` on the second click within [`DELETE_CONFIRM_WINDOW`];
    /// the first click only arms `key` and repaints.
    pub(crate) fn confirm_delete(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        if self.armed_delete.as_deref() == Some(key) {
            self.armed_delete = None;
            return true;
        }
        self.armed_delete = Some(key.to_owned());
        let armed = key.to_owned();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DELETE_CONFIRM_WINDOW).await;
            let _ = this.update(cx, |workspace, cx| {
                if workspace.armed_delete.as_deref() == Some(armed.as_str()) {
                    workspace.armed_delete = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
        false
    }
}

const SESSION_DELETE_PREFIX: &str = "session:";

fn session_delete_key(session_id: &str) -> String {
    format!("{SESSION_DELETE_PREFIX}{session_id}")
}

/// How long an armed session delete waits for the confirming click.
const DELETE_CONFIRM_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);
