//! Composer submit: send, steer, queue, and cancel the running turn.
use gpui_kit::{Context, Window};
use mycode_app::{BridgeCommand, BridgeReply};

use super::Workspace;
use crate::view_model::DesktopAction;

impl Workspace {
    pub(crate) fn on_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value().to_string();
        if draft != self.vm.composer_draft {
            self.apply_action(DesktopAction::ComposerChanged(draft.clone()), cx);
        }
        if self.welcome_send_session.is_some() || self.vm.pending_welcome_send.is_some() {
            return;
        }
        if self.vm.active.is_none() {
            self.submit_from_welcome(&draft, cx);
            return;
        }
        if self.vm.sending {
            if !draft.trim().is_empty() {
                self.dispatch_steer(draft, window, cx);
            }
            return;
        }
        if !draft.trim().is_empty() {
            self.send(draft, window, cx);
            return;
        }
        // Send with an empty composer is an explicit send of the queue,
        // so it also releases a queue held by a stop.
        self.apply_action(DesktopAction::QueuePaused(false), cx);
        self.pump_queued_send(cx);
    }

    /// Steers the running turn. The composer clears immediately. If the turn
    /// ended before the inbox was registered, the text is queued instead.
    fn dispatch_steer(&mut self, draft: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session_id) = self.open_session_id() else {
            self.enqueue_follow_up(draft, window, cx);
            return;
        };
        self.apply_action(DesktopAction::ComposerChanged(String::new()), cx);
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        // Shown above the composer until the ledger commits it.
        self.apply_action(DesktopAction::SteerSent(draft.clone()), cx);
        let request = self.bridge.request(BridgeCommand::Steer {
            session_id,
            text: draft.clone(),
        });
        cx.spawn(async move |this, cx| {
            let reply = request.await;
            if matches!(reply, BridgeReply::Steered(Err(_))) {
                let _ = this.update(cx, |workspace, cx| {
                    workspace.apply_action(DesktopAction::SteersReturned(vec![draft]), cx);
                    // The turn may have finished between the click and the
                    // reply. Pump so the text does not sit in the queue.
                    workspace.pump_queued_send(cx);
                });
            }
        })
        .detach();
    }

    /// Enter or send on the welcome desk starts a task with that text.
    ///
    /// The session is created the same way as New task. The draft is sent
    /// once that session's conversation is open.
    fn submit_from_welcome(&mut self, draft: &str, cx: &mut Context<Self>) {
        if self.vm.pending_welcome_send.is_some() {
            return;
        }
        match crate::view_model::composer_submit(&self.vm, draft) {
            crate::view_model::ComposerSubmit::StartTask(text) => {
                self.apply_action(DesktopAction::WelcomeSendHeld(text), cx);
                if !self.on_new_session(cx) {
                    self.apply_action(DesktopAction::WelcomeSendConsumed, cx);
                }
            }
            crate::view_model::ComposerSubmit::NeedFolder => {
                self.push_toast(
                    crate::i18n::t("Open a folder to get started.", "打开一个目录即可开始。"),
                    crate::workspace::ToastKind::Info,
                    cx,
                );
            }
            crate::view_model::ComposerSubmit::Send | crate::view_model::ComposerSubmit::Ignore => {
            }
        }
    }

    pub(crate) fn on_remove_queued(&mut self, index: usize, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.apply_action(DesktopAction::QueuedMessageRemoved(index), cx);
    }

    /// Takes queued follow-ups back into the composer so they can be edited
    /// or dropped: one item, or the whole queue when `index` is `None`.
    /// Recalled text goes above any draft already in the composer.
    pub(crate) fn on_recall_queued(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        let recalled: Vec<String> = match index {
            Some(index) => self.vm.queued.get(index).cloned().into_iter().collect(),
            None => self.vm.queued.clone(),
        };
        if recalled.is_empty() {
            return;
        }
        match index {
            Some(index) => self.apply_action(DesktopAction::QueuedMessageRemoved(index), cx),
            None => {
                for _ in 0..recalled.len() {
                    self.apply_action(DesktopAction::QueuedMessageRemoved(0), cx);
                }
            }
        }
        let draft = self.composer.read(cx).value().to_string();
        let mut text = recalled.join("\n\n");
        if !draft.trim().is_empty() {
            text.push_str("\n\n");
            text.push_str(&draft);
        }
        self.composer.update(cx, |state, cx| {
            state.set_value(text.clone(), window, cx);
            state.focus(window, cx);
        });
        self.apply_action(DesktopAction::ComposerChanged(text), cx);
    }

    /// Sends a held queue now. While a turn still runs, the queue is only
    /// released and starts when that turn ends.
    pub(crate) fn on_resume_queued(&mut self, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.apply_action(DesktopAction::QueuePaused(false), cx);
        self.pump_queued_send(cx);
    }

    /// Opens or closes the detail card for one running subagent.
    pub(crate) fn on_open_subagent(&mut self, call_id: &str, cx: &mut Context<Self>) {
        let next = if call_id.is_empty() || self.vm.subagent_window.as_deref() == Some(call_id) {
            None
        } else {
            Some(call_id.to_owned())
        };
        self.apply_action(DesktopAction::SubagentWindowChanged(next), cx);
    }

    /// Opens or closes the full working-tree changes drawer.
    pub(crate) fn on_toggle_changes_panel(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ChangesPanelToggled(open), cx);
    }

    /// Stops the in-flight turn and sends one queued follow-up immediately.
    pub(crate) fn on_interrupt_queued(&mut self, index: usize, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.apply_action(DesktopAction::QueuedMessagePromoted(index), cx);
        self.apply_action(DesktopAction::QueuePaused(false), cx);
        if self.vm.sending {
            self.cancel_turn(cx);
            return;
        }
        self.pump_queued_send(cx);
    }

    /// Stops one running subagent. The parent turn and the other children stay.
    pub(crate) fn on_cancel_subagent(&mut self, call_id: &str, cx: &mut Context<Self>) {
        let Some(session_id) = self.open_session_id() else {
            return;
        };
        if call_id.is_empty() {
            return;
        }
        self.apply_action(DesktopAction::SubagentDismissed(call_id.to_owned()), cx);
        self.dispatch(
            BridgeCommand::CancelSubagent {
                session_id,
                call_id: call_id.to_owned(),
            },
            cx,
        );
    }

    /// Aborts the in-flight turn; its terminal event resets the sending state.
    ///
    /// A plain stop (Stop or Escape) holds the queue: the follow-ups stay
    /// listed, and steers the turn hands back join them, but nothing is sent
    /// until the user sends or recalls them.
    pub(crate) fn on_cancel_chat(&mut self, cx: &mut Context<Self>) {
        if !self.vm.sending {
            return;
        }
        self.apply_action(DesktopAction::QueuePaused(true), cx);
        self.queue_hold_on_return = true;
        self.cancel_turn(cx);
    }

    fn cancel_turn(&mut self, cx: &mut Context<Self>) {
        let Some(conversation) = self.vm.active.as_ref() else {
            return;
        };
        if !self.vm.sending {
            return;
        }
        self.dispatch(
            BridgeCommand::CancelChat {
                session_id: conversation.session_id.clone(),
            },
            cx,
        );
    }
}
