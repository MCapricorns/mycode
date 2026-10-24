//! Driving one model turn: compaction, the chat turn itself, and the steer
//! and interrupt paths that reach a running turn.
use mycode_app::{BranchId, BridgeCommand, SessionId};

use super::{Effect, FrontendDriver, HeldInterrupt, ReplyScope, ToastKind};
use crate::view_model::{EntryKind, FrontendAction};

fn failed_start(message: String) -> FrontendAction {
    FrontendAction::ChatFailed {
        message,
        head: String::new(),
    }
}

impl FrontendDriver {
    /// Compacts the active conversation with the picked provider and model.
    ///
    /// A model missing from that provider's list uses the provider's first
    /// model. Unlike a chat turn, this does not fall back to another provider.
    pub fn compact(&mut self, out: &mut Vec<Effect>) {
        if self.vm.sending {
            self.toast(
                crate::i18n::t(
                    "Wait for the current turn to finish, then /compact.",
                    "等当前这一轮结束再 /compact。",
                ),
                ToastKind::Info,
                out,
            );
            return;
        }
        // A second `/compact` would cancel the one already running on the
        // bridge and retry against a stale head — one at a time.
        if self.vm.compacting {
            self.toast(
                crate::i18n::t(
                    "Compaction is already running; wait for it to finish.",
                    "正在压缩中，等它结束。",
                ),
                ToastKind::Info,
                out,
            );
            return;
        }
        if self.vm.active.is_none() {
            self.toast(
                crate::i18n::t("Open a chat before compacting.", "先打开一个对话再压缩。"),
                ToastKind::Info,
                out,
            );
            return;
        }
        let Some((session, branch, expected_head)) = self.open_turn_target() else {
            return;
        };
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let provider = settings.providers.iter().find(|provider| {
            provider.enabled && Some(&provider.id) == self.vm.selected_provider.as_ref()
        });
        let Some(provider) = provider else {
            self.toast(
                crate::i18n::t(
                    "Choose a model before compacting.",
                    "压缩前先选择一个模型。",
                ),
                ToastKind::Info,
                out,
            );
            return;
        };
        let Some(model) = self
            .vm
            .selected_model
            .clone()
            .filter(|model| provider.models.contains(model))
            .or_else(|| provider.models.first().cloned())
        else {
            return;
        };
        let provider_id = provider.id.clone();
        // Locks the composer and shows the waiting card until
        // `CompactFinished` lands (success, failure, or a cancelled run).
        self.act(FrontendAction::CompactArmed, out);
        self.dispatch(BridgeCommand::CompactSession {
            session,
            branch,
            expected_head,
            provider_id,
            model,
        });
    }

    pub(crate) fn begin_chat_turn(&mut self, out: &mut Vec<Effect>) {
        if self.vm.active.is_none() {
            self.act(failed_start("no open session".to_owned()), out);
            return;
        }
        let Some((session, branch, _)) = self.open_turn_target() else {
            self.act(
                failed_start("the open session could not be read".to_owned()),
                out,
            );
            return;
        };
        let selected_provider = self.vm.selected_provider.clone();
        let selected_model = self.vm.selected_model.clone();
        let provider_id = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| {
                settings
                    .providers
                    .iter()
                    .find(|provider| {
                        provider.enabled && Some(&provider.id) == selected_provider.as_ref()
                    })
                    .or_else(|| settings.providers.iter().find(|provider| provider.enabled))
            })
            .map(|provider| (provider.id.clone(), provider.models.clone()));
        let Some((provider_id, provider_models)) = provider_id else {
            self.act(
                failed_start(
                    crate::i18n::t(
                        "No enabled provider. Add one with its API key in Settings.",
                        "没有启用的服务商。请在设置中添加服务商和 API 密钥。",
                    )
                    .to_owned(),
                ),
                out,
            );
            return;
        };
        let model = selected_model
            .filter(|model| provider_models.contains(model))
            .or_else(|| provider_models.first().cloned());
        let Some(model) = model else {
            self.act(
                failed_start(
                    crate::i18n::t(
                        "The provider has no models configured.",
                        "该服务商没有配置模型。",
                    )
                    .to_owned(),
                ),
                out,
            );
            return;
        };
        let reasoning = self
            .vm
            .active
            .as_ref()
            .and_then(|active| {
                mycode_app::session_model(&self.vm.session_models, &active.session_id)
                    .and_then(|pin| pin.reasoning.clone())
            })
            .or_else(|| {
                self.vm
                    .settings
                    .as_ref()
                    .and_then(|settings| settings.reasoning.clone())
            });
        // The stored effort survives model switches, so it may name a rung
        // the resolved model does not advertise. An unset or unadvertised
        // pick runs at the strongest advertised rung, matching the thinking
        // chip's projection; a model with no rungs keeps the provider default.
        let advertised =
            crate::view_model::reasoning_levels_for(&self.vm, Some(&provider_id), Some(&model));
        let effective =
            crate::view_model::effective_reasoning_level(&advertised, reasoning.as_deref());
        let reasoning = (effective != "default").then_some(effective);
        // The session's very first send also gets a generated sidebar title:
        // an out-of-band request summarizes that message while the real turn
        // proceeds. `older_before` absent means every event is resident, so
        // one user message truly is the first send.
        if let Some((first_session, seed)) = self.first_user_message_seed() {
            self.dispatch(BridgeCommand::SummarizeTitle {
                session: first_session,
                provider_id: provider_id.clone(),
                model: model.clone(),
                seed,
            });
        }
        // The bridge rebuilds the turn history from the ledger's typed
        // events, so tool_use/tool_result pairing survives replay.
        self.dispatch(BridgeCommand::ChatTurn {
            session,
            branch,
            provider_id,
            model,
            reasoning,
        });
    }

    /// Session id plus the first user message's text when the open
    /// conversation holds exactly one committed user message and no older
    /// history. `None` for every later send.
    fn first_user_message_seed(&self) -> Option<(SessionId, String)> {
        let active = self.vm.active.as_ref()?;
        if active.older_before.is_some() {
            return None;
        }
        let mut users = active
            .entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::UserMessage);
        let first = users.next()?;
        if users.next().is_some() {
            return None;
        }
        let session = SessionId::parse(&active.session_id)?;
        Some((session, first.text.to_string()))
    }

    /// The composer's submit path: welcome-desk start, a steer into the
    /// running turn, or a normal send. `draft` is the composer text the
    /// frontend just read.
    pub fn submit(&mut self, draft: &str, out: &mut Vec<Effect>) {
        let _ = self.submit_with(draft, mycode_app::Attachments::default(), out);
    }

    /// [`Self::submit`] with images and code files picked in the composer.
    ///
    /// Returns the attachments when the message did not go out, so the
    /// frontend can put them back instead of losing them. A busy turn takes
    /// text only: attachments cannot ride a steer, so they wait in the
    /// composer for the turn to end instead of being dropped.
    pub fn submit_with(
        &mut self,
        draft: &str,
        attachments: mycode_app::Attachments,
        out: &mut Vec<Effect>,
    ) -> Option<mycode_app::Attachments> {
        // The widget's text is the source of truth: mirror it so mentions
        // and a later restore work on what is on screen.
        if draft != self.vm.composer_draft {
            self.act(FrontendAction::ComposerChanged(draft.to_owned()), out);
        }
        if self.welcome_send_session.is_some() || self.vm.pending_welcome_send.is_some() {
            return Some(attachments);
        }
        if self.vm.active.is_none() {
            return self.submit_from_welcome(draft, attachments, out);
        }
        if self.vm.sending || self.vm.compacting {
            // A submit on a busy turn steers the running one; the stop and
            // send path is `interrupt_send`.
            if !attachments.is_empty() {
                self.toast(
                    crate::i18n::t(
                        "Attachments wait for the current turn to end.",
                        "附件等当前回合结束后再发送。",
                    ),
                    ToastKind::Info,
                    out,
                );
                return Some(attachments);
            }
            self.steer_send(draft, out);
            return None;
        }
        if !draft.trim().is_empty() || !attachments.is_empty() {
            let sent = self.send_text(draft.to_owned(), true, attachments.clone(), out);
            return (!sent).then_some(attachments);
        }
        None
    }

    /// Interrupt-submit (Ctrl+Enter, Cmd+Enter on the macOS desktop): stop
    /// the running turn and send this draft right after it. With an empty
    /// draft it is a plain stop.
    ///
    /// The draft leaves the composer and waits in
    /// [`FrontendDriver::held_interrupt`] until the turn is gone: a send
    /// dispatched beside the cancel can hand that cancel the turn it just
    /// started, which is how a message went out while the model stopped.
    ///
    /// While the turn waits on an automatic compaction summary, the stop
    /// waits for the summary to end. Stopping it threw the summary away,
    /// and the send that followed summarized the same history again.
    pub fn interrupt_send(&mut self, draft: &str, out: &mut Vec<Effect>) {
        let text = draft.to_owned();
        if !self.vm.sending && !self.vm.compacting {
            // Nothing is running to interrupt: the draft is an ordinary send.
            if !text.trim().is_empty() {
                let _ = self.send_text(text, true, mycode_app::Attachments::default(), out);
            }
            return;
        }
        let send = !text.trim().is_empty();
        if send {
            out.push(Effect::ComposerSet {
                text: String::new(),
                focus: false,
            });
            self.act(FrontendAction::ComposerChanged(String::new()), out);
            self.toast(
                crate::i18n::t(
                    "Stopping this turn. The message goes out right after.",
                    "正在停止本轮，紧接着发送这条消息。",
                ),
                ToastKind::Info,
                out,
            );
        }
        self.stop_turn_with(send.then_some(text), out);
    }

    /// Stops the running turn for an interrupt, or once its automatic
    /// compaction ends when it is summarizing now. `send` waits in
    /// [`FrontendDriver::held_interrupt`] until the turn is gone.
    fn stop_turn_with(&mut self, send: Option<String>, out: &mut Vec<Effect>) {
        let Some(session_id) = self.open_session_id() else {
            return;
        };
        let held = HeldInterrupt { session_id, send };
        if !self.vm.auto_compacting {
            self.cancel_turn(out);
        }
        self.held_interrupt = Some(held);
    }

    /// Sends the draft held by an interrupt now that its turn is gone.
    pub(crate) fn send_held_interrupt(
        &mut self,
        held: HeldInterrupt,
        out: &mut Vec<Effect>,
    ) {
        if let Some(text) = held.send {
            // The composer was cleared when the interrupt was armed; a
            // draft typed since must survive the send.
            let _ = self.send_text(text, false, mycode_app::Attachments::default(), out);
        }
    }

    /// Hands a held draft back when the turn ended on its own: nothing was
    /// interrupted, so the message was never sent.
    pub(crate) fn restore_held_interrupt(
        &mut self,
        held: HeldInterrupt,
        out: &mut Vec<Effect>,
    ) {
        if let Some(text) = held.send {
            self.restore_above_draft(text, out);
        }
    }

    /// Steer-submit (Enter while a turn runs, Ctrl+G, Cmd+G on the macOS
    /// desktop): hands this draft to the running turn without stopping it.
    /// The model reads it before its next step; until the ledger has it, it
    /// shows as pending. A turn that ends before reading it hands it back to
    /// the composer.
    ///
    /// With no turn running it does not fall back to send: the draft would
    /// race a `/compact`, which reads as "the key queued it".
    pub fn steer_send(&mut self, draft: &str, out: &mut Vec<Effect>) {
        let session_id = self.open_session_id();
        let Some(session_id) = session_id.filter(|_| self.vm.sending) else {
            if !draft.trim().is_empty() {
                out.push(Effect::Toast {
                    kind: ToastKind::Warning,
                    text: if self.vm.compacting {
                        crate::i18n::t(
                            "Compacting context. The message waits in the composer.",
                            "正在压缩上下文，消息先留在输入框里。",
                        )
                    } else {
                        crate::i18n::t(
                            "Nothing is running to steer. Press Enter to send.",
                            "没有正在运行的任务可引导。按回车发送。",
                        )
                    }
                    .to_owned(),
                });
            }
            return;
        };
        if draft.trim().is_empty() {
            return;
        }
        let text = draft.to_owned();
        self.act(FrontendAction::SteerSent(text.clone()), out);
        out.push(Effect::ComposerSet {
            text: String::new(),
            focus: false,
        });
        self.dispatch(BridgeCommand::Steer { session_id, text });
    }

    /// Takes back one pending steer before the running turn reads it. The
    /// core removes it from the session's steer inbox; only on its
    /// confirmation does the reply path drop it from the pending strip and
    /// put the text back in the composer (see `replies.rs`). A steer the
    /// turn already read cannot be taken back and stays sent.
    pub fn retract_steer(&mut self, text: &str, out: &mut Vec<Effect>) {
        let _ = out;
        let Some(session_id) = self.open_session_id() else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        let text = text.to_owned();
        self.dispatch_scoped(
            BridgeCommand::RetractSteer {
                session_id: session_id.clone(),
                text: text.clone(),
            },
            ReplyScope::RetractSteer {
                session: session_id,
                text,
            },
        );
    }

    /// Steers a turn did not take go back to the composer, above the draft
    /// the user may be typing. A chat that is not on screen keeps them
    /// parked for its next open.
    pub(crate) fn return_steers(
        &mut self,
        session_id: &str,
        texts: Vec<String>,
        out: &mut Vec<Effect>,
    ) {
        self.act(FrontendAction::SteersReturned(texts.clone()), out);
        if self.open_session_id().as_deref() != Some(session_id) {
            self.act(
                FrontendAction::SteersParked {
                    session_id: session_id.to_owned(),
                    texts,
                },
                out,
            );
            return;
        }
        let texts: Vec<String> = texts
            .into_iter()
            .filter(|text| !text.trim().is_empty())
            .collect();
        if texts.is_empty() {
            return;
        }
        self.restore_above_draft(texts.join("\n\n"), out);
        self.toast(
            crate::i18n::t(
                "The turn ended before reading this; it is back in the composer.",
                "本轮在读到之前就结束了，这条已放回输入框。",
            ),
            ToastKind::Info,
            out,
        );
    }

    /// Enter or send on the welcome desk starts a task with that text.
    ///
    /// The session is created the same way as New task. The draft and any
    /// attachments are sent once that session's conversation is open.
    /// Attachments alone start a task too: their note stands in for the
    /// prompt so the title has a text anchor, while the durable message keeps
    /// the note only in its attachment list.
    fn submit_from_welcome(
        &mut self,
        draft: &str,
        attachments: mycode_app::Attachments,
        out: &mut Vec<Effect>,
    ) -> Option<mycode_app::Attachments> {
        if self.vm.pending_welcome_send.is_some() {
            return Some(attachments);
        }
        let prompt = if draft.trim().is_empty() && !attachments.is_empty() {
            attachments.transcript_note()
        } else {
            draft.to_owned()
        };
        match crate::view_model::composer_submit(&self.vm, &prompt) {
            crate::view_model::ComposerSubmit::StartTask(text) => {
                self.welcome_attachments = attachments.clone();
                // The note stands in for an empty draft so the first message
                // has a title; the durable message adds the note itself.
                self.welcome_send_is_note = draft.trim().is_empty() && !attachments.is_empty();
                self.act(FrontendAction::WelcomeSendHeld(text), out);
                if !self.new_session(out) {
                    self.welcome_send_is_note = false;
                    self.welcome_attachments = mycode_app::Attachments::default();
                    self.act(FrontendAction::WelcomeSendConsumed, out);
                    return Some(attachments);
                }
                None
            }
            crate::view_model::ComposerSubmit::NeedFolder => {
                self.toast(
                    crate::i18n::t("Open a folder to get started.", "打开一个目录即可开始。"),
                    ToastKind::Info,
                    out,
                );
                Some(attachments)
            }
            crate::view_model::ComposerSubmit::Send | crate::view_model::ComposerSubmit::Ignore => {
                Some(attachments)
            }
        }
    }

    /// Puts `text` back in the composer, above any draft already there.
    pub(crate) fn restore_above_draft(&mut self, mut text: String, out: &mut Vec<Effect>) {
        let draft = self.vm.composer_draft.clone();
        if !draft.trim().is_empty() {
            text.push_str("\n\n");
            text.push_str(&draft);
        }
        out.push(Effect::ComposerSet {
            text: text.clone(),
            focus: true,
        });
        self.act(FrontendAction::ComposerChanged(text), out);
    }

    /// Steers a chat did not read while another chat was on screen come back
    /// to the composer now that it opens. A switch just cleared the field,
    /// so the parked text lands there alone.
    pub(crate) fn unpark_steers(&mut self, session_id: &str, out: &mut Vec<Effect>) {
        let Some(index) = self
            .vm
            .parked_steers
            .iter()
            .position(|(parked, _)| parked == session_id)
        else {
            return;
        };
        let (_, texts) = self.vm.parked_steers.remove(index);
        let text = texts
            .into_iter()
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if text.is_empty() {
            return;
        }
        out.push(Effect::ComposerSet {
            text: text.clone(),
            focus: false,
        });
        self.act(FrontendAction::ComposerChanged(text), out);
        self.toast(
            crate::i18n::t(
                "This chat could not take the message; it is back in the composer.",
                "这个消息没能送进那个对话，内容已放回输入框。",
            ),
            ToastKind::Info,
            out,
        );
    }

    /// Stops one running subagent. The parent turn and the other children stay.
    pub fn cancel_subagent(&mut self, call_id: &str, out: &mut Vec<Effect>) {
        let Some(session_id) = self.open_session_id() else {
            return;
        };
        if call_id.is_empty() {
            return;
        }
        // Rows and jobs key on the ledger identity; the core publishes the
        // child token under the provider id. Resolve before the dismiss
        // drops the job that carries it.
        let cancel_id = self
            .vm
            .live_jobs
            .iter()
            .find(|job| job.call_id == call_id)
            .map(|job| job.provider_call_id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| call_id.to_owned());
        self.act(FrontendAction::SubagentDismissed(call_id.to_owned()), out);
        self.dispatch(BridgeCommand::CancelSubagent {
            session_id,
            call_id: cancel_id,
        });
    }

    /// Aborts the in-flight turn; its terminal event resets the sending state.
    /// Steers the turn never read come back to the composer.
    pub fn cancel_chat(&mut self, out: &mut Vec<Effect>) {
        if !self.vm.sending && !self.vm.compacting {
            return;
        }
        self.cancel_turn(out);
    }

    pub(super) fn cancel_turn(&mut self, out: &mut Vec<Effect>) {
        let Some(conversation) = self.vm.active.as_ref() else {
            return;
        };
        if !self.vm.sending && !self.vm.compacting {
            return;
        }
        self.dispatch(BridgeCommand::CancelChat {
            session_id: conversation.session_id.clone(),
        });
        let _ = out;
    }

    /// Hands welcome attachments back when the start that held them failed.
    pub(crate) fn return_welcome_attachments(&mut self, out: &mut Vec<Effect>) {
        self.welcome_send_is_note = false;
        let attachments = std::mem::take(&mut self.welcome_attachments);
        if !attachments.is_empty() {
            out.push(Effect::AttachmentsRestored(attachments));
        }
    }

    /// Sends the welcome-desk draft once its new session is the open one.
    pub(crate) fn flush_welcome_send(&mut self, session_id: &str, out: &mut Vec<Effect>) {
        if self.welcome_send_session.as_deref() != Some(session_id) {
            return;
        }
        let Some(text) = self.vm.pending_welcome_send.clone() else {
            self.welcome_send_session = None;
            self.welcome_attachments = mycode_app::Attachments::default();
            self.welcome_send_is_note = false;
            return;
        };
        let ready = self.vm.active.as_ref().is_some_and(|conversation| {
            conversation.session_id == session_id
                && SessionId::parse(&conversation.session_id).is_some()
                && BranchId::parse(&conversation.branch_id).is_some()
        });
        if !ready {
            return;
        }
        self.welcome_send_session = None;
        let attachments = std::mem::take(&mut self.welcome_attachments);
        let text = if self.welcome_send_is_note {
            String::new()
        } else {
            text
        };
        self.welcome_send_is_note = false;
        self.act(FrontendAction::WelcomeSendConsumed, out);
        self.act(FrontendAction::ComposerChanged(String::new()), out);
        self.pending_composer_prefill = Some(String::new());
        let _ = self.send_text(text, true, attachments, out);
    }

    fn send_text(
        &mut self,
        draft: String,
        clear_composer: bool,
        attachments: mycode_app::Attachments,
        out: &mut Vec<Effect>,
    ) -> bool {
        if draft.trim().is_empty() && attachments.is_empty() {
            return false;
        }
        let Some((session, branch, expected_head)) = self.open_turn_target() else {
            return false;
        };
        if !attachments.is_empty() {
            // The reply may come back as an error; keep the payload beside
            // the session so the composer gets it back instead of a gap.
            self.pending_send_attachments
                .insert(session.to_string(), attachments.clone());
        }
        self.act(FrontendAction::TurnArmed, out);
        if clear_composer {
            self.vm.composer_draft.clear();
            out.push(Effect::ComposerSet {
                text: String::new(),
                focus: false,
            });
        }
        let command = BridgeCommand::SendMessage {
            session,
            branch,
            expected_head,
            text: draft,
            attachments,
        };
        if self.reopen_in_flight() {
            // That reopen's snapshot predates this message. Its reply can
            // land after the send's and wipe the message off the screen, so
            // the send goes out once the snapshot is on screen.
            self.held_send = Some(command);
        } else {
            self.dispatch(command);
        }
        true
    }

    /// A reopen of the chat on screen is in flight.
    fn reopen_in_flight(&self) -> bool {
        self.pending_open.is_some() && self.pending_open == self.open_session_id()
    }

    /// Sends the held message once no reopen of its chat is in flight.
    pub(crate) fn release_held_send(&mut self) {
        if self.reopen_in_flight() {
            return;
        }
        if let Some(command) = self.held_send.take() {
            self.dispatch(command);
        }
    }
}
