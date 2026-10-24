//! Composer submit, steer, and stop controls. The product logic —
//! welcome-desk starts, steer-send, interrupt-send, cancels — lives in the
//! frontend driver (`submit`, `steer_send`, `interrupt_send`); this file
//! keeps the gpui key handling and click propagation.
use gpui_kit::{Context, Window};

use super::Workspace;
use crate::view_model::FrontendAction;

impl Workspace {
    pub(crate) fn on_send(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // A model can be switched after the picture was picked; the send is
        // the last point where the promise "images only go to a vision
        // model" can be kept.
        if !self.attachments.images.is_empty() && !self.selected_model_takes_images() {
            self.push_toast(
                crate::i18n::t(
                    "The selected model cannot see images; remove them or pick a vision model.",
                    "当前模型看不了图片，请去掉图片或选择支持图片的模型。",
                ),
                super::toasts::ToastKind::Warning,
                cx,
            );
            return;
        }
        let draft = self.composer.read(cx).value().to_string();
        // The driver decides whether the message goes out and hands the
        // attachments back when it does not, so a refused send (no folder,
        // a held welcome send, a running turn) keeps the chips on screen.
        let attachments = std::mem::take(&mut self.attachments);
        let returned = self.driver_take(|d, fx| d.submit_with(&draft, attachments, fx), cx);
        if let Some(attachments) = returned {
            self.attachments = attachments;
        }
    }

    /// Ctrl+G (Cmd+G on macOS), and the arrow beside Stop: hands the draft to
    /// the running turn without stopping it. With nothing to steer it keeps
    /// the draft and says why, so a keypress never sends by surprise.
    pub(crate) fn on_steer_send(&mut self, cx: &mut Context<Self>) {
        if !self.attachments.is_empty() {
            self.push_toast(
                crate::i18n::t(
                    "Attachments can only go with a normal send.",
                    "附件只能随普通发送一起发出。",
                ),
                super::toasts::ToastKind::Info,
                cx,
            );
            return;
        }
        let draft = self.composer.read(cx).value().to_string();
        self.driver_do(|d, fx| d.steer_send(&draft, fx), cx);
    }

    /// Interrupt-send (Ctrl+Enter on a busy turn, or the send choice's
    /// interrupt row): stops the running turn and sends the draft right
    /// after it. Attachments never ride it; the Enter path warns about
    /// them before calling this.
    pub(crate) fn on_interrupt_send(&mut self, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value().to_string();
        self.driver_do(|d, fx| d.interrupt_send(&draft, fx), cx);
    }

    /// The send choice's steer row: close the choice and steer the draft
    /// in, exactly like Ctrl+G.
    pub(crate) fn on_send_choice_steer(&mut self, cx: &mut Context<Self>) {
        self.send_choice_open = false;
        self.on_steer_send(cx);
    }

    /// The send choice's interrupt row: close the choice and interrupt.
    pub(crate) fn on_send_choice_interrupt(&mut self, cx: &mut Context<Self>) {
        self.send_choice_open = false;
        self.on_interrupt_send(cx);
    }

    /// Takes back the pending steer at `index` in the strip above the
    /// composer. The strip drops it only when the core confirms the turn
    /// had not read it yet; the text then returns to the composer.
    pub(crate) fn on_retract_steer(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(text) = self.vm().pending_steers.get(index).cloned() else {
            return;
        };
        self.driver_do(|d, fx| d.retract_steer(&text, fx), cx);
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

    /// Stops one running subagent. The parent turn and the other children stay.
    pub(crate) fn on_cancel_subagent(&mut self, call_id: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.cancel_subagent(call_id, fx), cx);
    }

    /// Aborts the in-flight turn; its terminal event resets the sending state.
    /// Steers the turn never read come back to the composer.
    pub(crate) fn on_cancel_chat(&mut self, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.cancel_chat(fx), cx);
    }
}
