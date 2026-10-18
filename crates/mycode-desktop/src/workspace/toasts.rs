//! Transient notices above the window: info and warnings expire, errors
//! stay until dismissed.
use std::time::Duration;

use gpui_kit::Context;

use super::Workspace;

/// Notice severity. Errors remain visible until explicitly dismissed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastKind {
    Info,
    /// A cache collapse or another condition the user should notice.
    Warning,
    Error,
}

pub(crate) struct Toast {
    pub(crate) id: u64,
    pub(crate) text: String,
    pub(crate) kind: ToastKind,
}

impl Workspace {
    pub(crate) fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    /// Keeps errors until dismissal; other notices expire after a readable delay.
    pub(crate) fn push_toast(
        &mut self,
        text: impl Into<String>,
        kind: ToastKind,
        cx: &mut Context<Self>,
    ) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        if self
            .toasts
            .iter()
            .any(|toast| toast.kind == kind && toast.text == text)
        {
            return;
        }
        let id = self.next_toast_id;
        self.next_toast_id = self.next_toast_id.wrapping_add(1);
        self.toasts.push(Toast { id, text, kind });
        // A click handler repaints on its own. A toast pushed from the event
        // pump (compaction finishing, a while after "Compacting…") does not,
        // so the row was removed by its timer before the window ever drew it.
        cx.notify();
        let Some(lifetime) = toast_lifetime(kind) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(lifetime).await;
            let _ = this.update(cx, |workspace, cx| {
                let before = workspace.toasts.len();
                workspace.toasts.retain(|toast| toast.id != id);
                // A toast closed by hand already repainted.
                if workspace.toasts.len() != before {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn on_dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        self.toasts.retain(|toast| toast.id != id);
        cx.notify();
    }
}

fn toast_lifetime(kind: ToastKind) -> Option<Duration> {
    match kind {
        ToastKind::Error => None,
        ToastKind::Warning => Some(Duration::from_secs(10)),
        ToastKind::Info => Some(Duration::from_secs(5)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_have_no_expiry_and_other_notices_have_reading_time() {
        assert_eq!(toast_lifetime(ToastKind::Error), None);
        assert_eq!(
            toast_lifetime(ToastKind::Info),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            toast_lifetime(ToastKind::Warning),
            Some(Duration::from_secs(10))
        );
    }
}
