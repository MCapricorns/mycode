//! A submitted message belongs to its original conversation, not the visible tab.
use crate::app::{BridgeCommand, BridgeEvent, BridgeReply, ConversationEntry};
use crate::core::Attachments;
use crate::frontend::view_model::FrontendAction;

use super::{Effect, FrontendDriver, ReplyScope, ToastKind};

pub(super) struct SubmittedTurn {
    pub(super) branch: String,
    /// Taken once the commit succeeds; `None` means its turn was dispatched.
    pub(super) command: Option<Result<BridgeCommand, String>>,
    pub(super) first_message: bool,
    /// Stop during persistence waits for the commit but must not start a model.
    pub(super) cancelled: bool,
}

pub(super) struct FailedSendDraft {
    text: String,
    attachments: Attachments,
}

impl FrontendDriver {
    pub(super) fn finish_send(
        &mut self,
        session: &str,
        head: String,
        entry: ConversationEntry,
        out: &mut Vec<Effect>,
    ) {
        let Some(submitted) = self.submitted_turns.get_mut(session) else {
            return;
        };
        let Some(command) = submitted.command.take() else {
            return;
        };
        let branch = submitted.branch.clone();
        let first_message = submitted.first_message;
        let cancelled = submitted.cancelled;
        self.pending_send_attachments.remove(session);
        let seed = first_message.then(|| entry.text.to_string());
        if self.send_view_matches(session, &branch) {
            self.act(
                FrontendAction::MessageSent {
                    head: head.clone(),
                    entry,
                },
                out,
            );
        }
        if cancelled {
            self.on_event(
                BridgeEvent::ChatFailed {
                    session_id: session.to_owned(),
                    message: crate::app::CHAT_CANCELLED.to_owned(),
                    head,
                },
                out,
            );
            return;
        }
        let command = match command {
            Ok(command) => command,
            Err(message) => {
                self.finish_sent_turn_start(
                    session,
                    &branch,
                    BridgeReply::ChatStarted(Err(message)),
                    out,
                );
                return;
            }
        };
        if let Some(seed) = seed
            && let BridgeCommand::ChatTurn {
                session,
                provider_id,
                model,
                ..
            } = &command
        {
            self.dispatch(BridgeCommand::SummarizeTitle {
                session: session.clone(),
                provider_id: provider_id.clone(),
                model: model.clone(),
                seed,
            });
        }
        // Only transcript projection is view-scoped. Starting the accepted
        // request must happen even if another chat (or an empty desk) is open.
        self.dispatch_scoped(
            command,
            ReplyScope::SentTurn {
                session: session.to_owned(),
                branch,
            },
        );
    }

    pub(super) fn finish_sent_turn_start(
        &mut self,
        session: &str,
        branch: &str,
        reply: BridgeReply,
        out: &mut Vec<Effect>,
    ) {
        if let BridgeReply::ChatStarted(Err(message)) = reply {
            self.submitted_turns.remove(session);
            if self.send_view_matches(session, branch) {
                self.act(
                    FrontendAction::ChatFailed {
                        message,
                        head: String::new(),
                    },
                    out,
                );
            } else {
                self.toast(message, ToastKind::Error, out);
            }
        }
    }

    pub(super) fn fail_send(
        &mut self,
        session: &str,
        mut text: String,
        message: String,
        out: &mut Vec<Effect>,
    ) {
        let submitted = self.submitted_turns.remove(session);
        let attachments = self
            .pending_send_attachments
            .remove(session)
            .unwrap_or_default();
        let visible = self.open_session_id().as_deref() == Some(session)
            && submitted
                .as_ref()
                .is_none_or(|submitted| self.send_view_matches(session, &submitted.branch));
        if let Some(held) = self
            .held_interrupt
            .take_if(|held| held.session_id == session)
            && let Some(send) = held.send
        {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&send);
        }
        self.failed_send_drafts
            .insert(session.to_owned(), FailedSendDraft { text, attachments });
        if visible {
            self.act(
                FrontendAction::ChatFailed {
                    message,
                    head: String::new(),
                },
                out,
            );
            self.restore_failed_send(session, out);
        } else {
            // Do not inject another conversation's text or attachments into
            // the active composer. Its next open restores the whole draft.
            self.toast(message, ToastKind::Error, out);
        }
    }

    pub(super) fn restore_failed_send(&mut self, session: &str, out: &mut Vec<Effect>) {
        let Some(draft) = self.failed_send_drafts.remove(session) else {
            return;
        };
        if !draft.text.is_empty() {
            self.restore_above_draft(draft.text, out);
        }
        if !draft.attachments.is_empty() {
            out.push(Effect::AttachmentsRestored(draft.attachments));
        }
    }

    fn send_view_matches(&self, session: &str, branch: &str) -> bool {
        self.vm
            .active
            .as_ref()
            .is_some_and(|active| active.session_id == session && active.branch_id == branch)
    }
}
