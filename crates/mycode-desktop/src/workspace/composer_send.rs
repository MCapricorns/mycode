//! Composer submit, steer, and stop controls. The product logic —
//! welcome-desk starts, steer-send, interrupt-send, cancels — lives in the
//! frontend driver (`submit`, `steer_send`, `interrupt_send`); this file
//! keeps the gpui key handling and click propagation.
use gpui_kit::{Context, Window};

use super::Workspace;
use crate::view_model::FrontendAction;

/// Resolve Enter against live turn state. A choice opened during an earlier
/// turn must not turn an idle Enter into a failed steer, and attachments added
/// while it was open must still use the ordinary send path.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum EnterAction {
    Submit,
    OpenChoice,
    Steer,
    Interrupt,
}

pub(super) fn enter_action(
    sending: bool,
    choice_open: bool,
    draft: &str,
    has_attachments: bool,
    secondary: bool,
) -> EnterAction {
    if secondary && sending {
        EnterAction::Interrupt
    } else if crate::ui::busy_enter_opens_choice(sending, draft, has_attachments) {
        if choice_open {
            EnterAction::Steer
        } else {
            EnterAction::OpenChoice
        }
    } else {
        EnterAction::Submit
    }
}

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
    /// after it. Attachments stay in the composer for the next normal send.
    pub(crate) fn on_interrupt_send(&mut self, cx: &mut Context<Self>) {
        if !self.attachments.is_empty() {
            self.push_toast(
                crate::i18n::t(
                    "Attachments wait for the next send.",
                    "附件等下一次发送再带上。",
                ),
                super::toasts::ToastKind::Info,
                cx,
            );
        }
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

#[cfg(test)]
mod tests {
    use super::{EnterAction, enter_action};

    #[test]
    fn stale_busy_choice_does_not_intercept_an_idle_send() {
        for secondary in [false, true] {
            assert_eq!(
                enter_action(false, true, "next question", false, secondary),
                EnterAction::Submit
            );
        }
    }

    #[test]
    fn attachments_added_to_an_open_choice_use_the_normal_send_path() {
        assert_eq!(
            enter_action(true, true, "read this file", true, false),
            EnterAction::Submit
        );
        // The explicit interrupt shortcut still works and warns that attachments wait.
        assert_eq!(
            enter_action(true, true, "read this file", true, true),
            EnterAction::Interrupt
        );
    }

    #[test]
    fn busy_text_enter_opens_then_confirms_the_choice() {
        assert_eq!(
            enter_action(true, false, "continue here", false, false),
            EnterAction::OpenChoice
        );
        assert_eq!(
            enter_action(true, true, "continue here", false, false),
            EnterAction::Steer
        );
        assert_eq!(
            enter_action(true, true, "continue here", false, true),
            EnterAction::Interrupt
        );
        assert_eq!(
            enter_action(true, true, "  ", false, false),
            EnterAction::Submit
        );
    }
}
