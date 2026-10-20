//! Composer submit and queue controls. The product logic — welcome-desk
//! starts, the follow-up queue, interrupt-send, steers, cancels — lives in
//! the frontend driver (`submit`, `interrupt_send`, `steer_send`, the queue
//! verbs); this file keeps the gpui key handling and click propagation.
use gpui_kit::{Context, Window};

use super::Workspace;
use crate::view_model::FrontendAction;

impl Workspace {
    pub(crate) fn on_send(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value().to_string();
        self.driver_do(|d, fx| d.submit(&draft, fx), cx);
    }

    /// Ctrl+G (Cmd+G on macOS): hands the draft to the running turn without
    /// stopping it. With no turn running it sends like Enter.
    pub(crate) fn on_steer_send(&mut self, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value().to_string();
        self.driver_do(|d, fx| d.steer_send(&draft, fx), cx);
    }

    pub(crate) fn on_remove_queued(&mut self, index: usize, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.driver_do(|d, fx| d.remove_queued(index, fx), cx);
    }

    /// Takes queued follow-ups back into the composer so they can be edited
    /// or dropped: one item, or the whole queue when `index` is `None`. The
    /// driver emits `ComposerSet` with focus; the composer paint applies it.
    pub(crate) fn on_recall_queued(
        &mut self,
        index: Option<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.driver_do(|d, fx| d.recall_queued(index, fx), cx);
    }

    /// Sends a held queue now. While a turn still runs, the queue is only
    /// released and starts when that turn ends.
    pub(crate) fn on_resume_queued(&mut self, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.driver_do(|d, fx| d.resume_queued(fx), cx);
    }

    /// Opens or closes the detail card for one running subagent.
    pub(crate) fn on_open_subagent(&mut self, call_id: &str, cx: &mut Context<Self>) {
        let next = if call_id.is_empty() || self.vm().subagent_window.as_deref() == Some(call_id) {
            None
        } else {
            Some(call_id.to_owned())
        };
        self.apply_action(FrontendAction::SubagentWindowChanged(next), cx);
    }

    /// Opens or closes the full working-tree changes drawer.
    pub(crate) fn on_toggle_changes_panel(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(FrontendAction::ChangesPanelToggled(open), cx);
    }

    /// Stops the in-flight turn and sends one queued follow-up immediately.
    pub(crate) fn on_interrupt_queued(&mut self, index: usize, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.driver_do(|d, fx| d.interrupt_queued(index, fx), cx);
    }

    /// Stops one running subagent. The parent turn and the other children stay.
    pub(crate) fn on_cancel_subagent(&mut self, call_id: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.cancel_subagent(call_id, fx), cx);
    }

    /// Aborts the in-flight turn; its terminal event resets the sending state.
    ///
    /// A plain stop holds the queue: the follow-ups stay listed, and steers
    /// the turn hands back join them, but nothing is sent until the user
    /// sends or recalls them.
    pub(crate) fn on_cancel_chat(&mut self, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.cancel_chat(fx), cx);
    }
}
