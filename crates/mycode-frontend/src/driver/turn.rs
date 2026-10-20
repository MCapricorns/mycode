//! Driving one model turn: compaction, the chat turn itself, and the send
//! queue that feeds it.
use mycode_app::{BranchId, BridgeCommand, SessionId};

use super::{Effect, FrontendDriver, ToastKind};
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
                mycode_config::session_model(&self.vm.session_models, &active.session_id)
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

    /// The composer's submit path: welcome-desk start, queue while a turn
    /// runs, a normal send, or an explicit send of the held queue on an
    /// empty draft. `draft` is the composer text the frontend just read.
    pub fn submit(&mut self, draft: &str, out: &mut Vec<Effect>) {
        // The widget's text is the source of truth: mirror it so mentions,
        // the queue, and a later recall all work on what is on screen.
        if draft != self.vm.composer_draft {
            self.act(FrontendAction::ComposerChanged(draft.to_owned()), out);
        }
        if self.welcome_send_session.is_some() || self.vm.pending_welcome_send.is_some() {
            return;
        }
        if self.vm.active.is_none() {
            self.submit_from_welcome(draft, out);
            return;
        }
        if self.vm.sending || self.vm.compacting {
            // A submit on a busy turn or compact queues the follow-up;
            // the interrupt path is `interrupt_send`.
            if !draft.trim().is_empty() {
                self.enqueue_follow_up(draft.to_owned(), out);
            }
            return;
        }
        if !draft.trim().is_empty() {
            self.send_text(draft.to_owned(), true, out);
            return;
        }
        // Send with an empty composer is an explicit send of the queue,
        // so it also releases a queue held by a stop.
        self.queue_hold_on_return = false;
        self.act(FrontendAction::QueuePaused(false), out);
        self.pump_queued_send(out);
    }

    /// Interrupt-submit (the desktop's Ctrl+Enter): stop the running turn
    /// and send this draft right away. With an empty draft it behaves like
    /// the queue's interrupt — the held queue runs after the cancel lands.
    ///
    /// While the turn waits on an automatic compaction summary, the stop
    /// waits for the summary to end. Stopping it threw the summary away,
    /// and the send that followed summarized the same history again.
    pub fn interrupt_send(&mut self, draft: &str, out: &mut Vec<Effect>) {
        if draft.trim().is_empty() {
            self.queue_hold_on_return = false;
            self.act(FrontendAction::QueuePaused(false), out);
            if self.vm.sending || self.vm.compacting {
                self.interrupt_turn(out);
            } else {
                self.pump_queued_send(out);
            }
            return;
        }
        let before = self.vm.queued.len();
        self.enqueue_follow_up(draft.to_owned(), out);
        // A capped queue dropped the draft: interrupt anyway so the held
        // queue runs instead of leaving the keypress a no-op.
        let index = if self.vm.queued.len() > before {
            self.vm.queued.len() - 1
        } else {
            0
        };
        self.interrupt_queued(index, out);
    }

    /// Enter or send on the welcome desk starts a task with that text.
    ///
    /// The session is created the same way as New task. The draft is sent
    /// once that session's conversation is open.
    fn submit_from_welcome(&mut self, draft: &str, out: &mut Vec<Effect>) {
        if self.vm.pending_welcome_send.is_some() {
            return;
        }
        match crate::view_model::composer_submit(&self.vm, draft) {
            crate::view_model::ComposerSubmit::StartTask(text) => {
                self.act(FrontendAction::WelcomeSendHeld(text), out);
                if !self.new_session(out) {
                    self.act(FrontendAction::WelcomeSendConsumed, out);
                }
            }
            crate::view_model::ComposerSubmit::NeedFolder => {
                self.toast(
                    crate::i18n::t("Open a folder to get started.", "打开一个目录即可开始。"),
                    ToastKind::Info,
                    out,
                );
            }
            crate::view_model::ComposerSubmit::Send | crate::view_model::ComposerSubmit::Ignore => {
            }
        }
    }

    /// Removes one queued follow-up.
    pub fn remove_queued(&mut self, index: usize, out: &mut Vec<Effect>) {
        self.act(FrontendAction::QueuedMessageRemoved(index), out);
    }

    /// Takes queued follow-ups back into the composer so they can be edited
    /// or dropped: one item, or the whole queue when `index` is `None`.
    /// Recalled text goes above any draft already in the composer.
    pub fn recall_queued(&mut self, index: Option<usize>, out: &mut Vec<Effect>) {
        let recalled: Vec<String> = match index {
            Some(index) => self.vm.queued.get(index).cloned().into_iter().collect(),
            None => self.vm.queued.clone(),
        };
        if recalled.is_empty() {
            return;
        }
        let removed = match index {
            Some(index) => FrontendAction::QueuedMessageRemoved(index),
            None => FrontendAction::QueueCleared,
        };
        self.act(removed, out);
        self.restore_above_draft(recalled.join("\n\n"), out);
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

    /// Sends a held queue now. While a turn still runs, the queue is only
    /// released and starts when that turn ends.
    pub fn resume_queued(&mut self, out: &mut Vec<Effect>) {
        self.queue_hold_on_return = false;
        self.act(FrontendAction::QueuePaused(false), out);
        self.pump_queued_send(out);
    }

    /// Stops the in-flight turn and sends one queued follow-up immediately.
    pub fn interrupt_queued(&mut self, index: usize, out: &mut Vec<Effect>) {
        self.queue_hold_on_return = false;
        self.act(FrontendAction::QueuedMessagePromoted(index), out);
        self.act(FrontendAction::QueuePaused(false), out);
        if self.vm.sending || self.vm.compacting {
            self.interrupt_turn(out);
            return;
        }
        self.pump_queued_send(out);
    }

    /// Stops the running turn for an interrupt, or once its automatic
    /// compaction ends when it is summarizing now.
    fn interrupt_turn(&mut self, out: &mut Vec<Effect>) {
        if self.vm.auto_compacting {
            self.interrupt_after_compaction = self.open_session_id();
            return;
        }
        self.cancel_turn(out);
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
    ///
    /// A plain stop holds the queue: the follow-ups stay listed, and steers
    /// the turn hands back join them, but nothing is sent until the user
    /// sends or recalls them.
    pub fn cancel_chat(&mut self, out: &mut Vec<Effect>) {
        if !self.vm.sending && !self.vm.compacting {
            return;
        }
        self.act(FrontendAction::QueuePaused(true), out);
        self.queue_hold_on_return = true;
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

    /// Enqueues a follow-up while a turn is running; the composer stays free
    /// for the next draft. The queue is capped so a stuck turn cannot grow
    /// without bound.
    pub(crate) fn enqueue_follow_up(&mut self, draft: String, out: &mut Vec<Effect>) {
        if self.vm.queued.len() >= crate::view_model::MAX_QUEUED_MESSAGES {
            return;
        }
        self.act(FrontendAction::MessageQueued(draft), out);
        out.push(Effect::ComposerSet {
            text: String::new(),
            focus: false,
        });
    }

    /// Starts the next queued follow-up once the in-flight turn is idle.
    /// Does not wipe the composer: the user may already be typing another
    /// message behind the queue.
    pub(crate) fn pump_queued_send(&mut self, out: &mut Vec<Effect>) {
        if self.vm.sending
            || self.vm.compacting
            || self.vm.queued.is_empty()
            || self.vm.queue_paused
        {
            return;
        }
        let draft = self.vm.queued[0].clone();
        self.act(FrontendAction::QueuedMessageTaken, out);
        self.send_text(draft, false, out);
    }

    /// Sends the welcome-desk draft once its new session is the open one.
    pub(crate) fn flush_welcome_send(&mut self, session_id: &str, out: &mut Vec<Effect>) {
        if self.welcome_send_session.as_deref() != Some(session_id) {
            return;
        }
        let Some(text) = self.vm.pending_welcome_send.clone() else {
            self.welcome_send_session = None;
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
        self.act(FrontendAction::WelcomeSendConsumed, out);
        self.act(FrontendAction::ComposerChanged(String::new()), out);
        self.pending_composer_prefill = Some(String::new());
        self.send_text(text, true, out);
    }

    fn send_text(&mut self, draft: String, clear_composer: bool, out: &mut Vec<Effect>) {
        if draft.trim().is_empty() {
            return;
        }
        let Some((session, branch, expected_head)) = self.open_turn_target() else {
            return;
        };
        self.queue_hold_on_return = false;
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
        };
        if self.reopen_in_flight() {
            // That reopen's snapshot predates this message. Its reply can
            // land after the send's and wipe the message off the screen, so
            // the send goes out once the snapshot is on screen.
            self.held_send = Some(command);
        } else {
            self.dispatch(command);
        }
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
