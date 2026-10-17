//! Driving one model turn: compaction, the chat turn itself, and the send
//! queue that feeds it.
use gpui_kit::{Context, Window};

use mycode_app::{BranchId, BridgeCommand, SessionId};

use super::failed_start;
use crate::view_model::{DesktopAction, EntryKind};
use crate::workspace::Workspace;

impl Workspace {
    /// Compacts the active conversation with the picked provider and model.
    ///
    /// A model missing from that provider's list uses the provider's first
    /// model. Unlike a chat turn, this does not fall back to another provider.
    pub(in crate::workspace) fn on_compact(&mut self, cx: &mut Context<Self>) {
        if self.vm.sending {
            self.push_toast(
                crate::i18n::t(
                    "Wait for the current turn to finish, then /compact.",
                    "等当前这一轮结束再 /compact。",
                )
                .to_owned(),
                crate::workspace::ToastKind::Info,
                cx,
            );
            return;
        }
        // A second `/compact` would cancel the one already running on the
        // bridge and retry against a stale head — one at a time.
        if self.vm.compacting {
            self.push_toast(
                crate::i18n::t(
                    "Compaction is already running; wait for it to finish.",
                    "正在压缩中，等它结束。",
                )
                .to_owned(),
                crate::workspace::ToastKind::Info,
                cx,
            );
            return;
        }
        if self.vm.active.is_none() {
            self.push_toast(
                crate::i18n::t("Open a chat before compacting.", "先打开一个对话再压缩。")
                    .to_owned(),
                crate::workspace::ToastKind::Info,
                cx,
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
            self.push_toast(
                crate::i18n::t(
                    "Choose a model before compacting.",
                    "压缩前先选择一个模型。",
                )
                .to_owned(),
                crate::workspace::ToastKind::Info,
                cx,
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
        self.apply_action(DesktopAction::CompactArmed, cx);
        self.dispatch(
            BridgeCommand::CompactSession {
                session,
                branch,
                expected_head,
                provider_id,
                model,
            },
            cx,
        );
    }

    pub(super) fn begin_chat_turn(&mut self, cx: &mut Context<Self>) {
        if self.vm.active.is_none() {
            self.apply_action(failed_start("no open session".to_owned()), cx);
            return;
        }
        let Some((session, branch, _)) = self.open_turn_target() else {
            self.apply_action(
                failed_start("the open session could not be read".to_owned()),
                cx,
            );
            return;
        };
        let Some(settings) = self.vm.settings.as_ref() else {
            self.apply_action(failed_start("settings are still loading".to_owned()), cx);
            return;
        };
        let selected_provider = self.vm.selected_provider.clone();
        let selected_model = self.vm.selected_model.clone();
        let provider = settings
            .providers
            .iter()
            .find(|provider| provider.enabled && Some(&provider.id) == selected_provider.as_ref())
            .or_else(|| settings.providers.iter().find(|provider| provider.enabled));
        let Some(provider) = provider else {
            self.apply_action(
                failed_start(
                    "no enabled provider — add one with its API key in Settings".to_owned(),
                ),
                cx,
            );
            return;
        };
        let model = selected_model
            .filter(|model| provider.models.contains(model))
            .or_else(|| provider.models.first().cloned());
        let Some(model) = model else {
            self.apply_action(
                failed_start("the provider has no models configured".to_owned()),
                cx,
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
            crate::view_model::reasoning_levels_for(self.vm(), Some(&provider.id), Some(&model));
        let effective =
            crate::view_model::effective_reasoning_level(&advertised, reasoning.as_deref());
        let reasoning = (effective != "default").then_some(effective);
        // The session's very first send also gets a generated sidebar title:
        // an out-of-band request summarizes that message while the real turn
        // proceeds. `older_before` absent means every event is resident, so
        // one user message truly is the first send.
        if let Some((first_session, seed)) = self.first_user_message_seed() {
            self.dispatch(
                BridgeCommand::SummarizeTitle {
                    session: first_session,
                    provider_id: provider.id.clone(),
                    model: model.clone(),
                    seed,
                },
                cx,
            );
        }
        // The bridge rebuilds the turn history from the ledger's typed
        // events, so tool_use/tool_result pairing survives replay.
        self.dispatch(
            BridgeCommand::ChatTurn {
                session,
                branch,
                provider_id: provider.id.clone(),
                model,
                reasoning,
            },
            cx,
        );
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

    pub(in crate::workspace) fn send(
        &mut self,
        draft: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.send_text(draft, true, Some(window), cx);
    }

    /// Enqueues a follow-up while a turn is running; the composer stays free
    /// for the next draft. The queue is capped so a stuck turn cannot grow
    /// without bound.
    pub(in crate::workspace) fn enqueue_follow_up(
        &mut self,
        draft: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vm.queued.len() >= crate::view_model::MAX_QUEUED_MESSAGES {
            return;
        }
        self.apply_action(DesktopAction::MessageQueued(draft), cx);
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    /// Starts the next queued follow-up once the in-flight turn is idle.
    /// Does not wipe the composer: the user may already be typing another
    /// message behind the queue.
    pub(in crate::workspace) fn pump_queued_send(&mut self, cx: &mut Context<Self>) {
        if self.vm.sending
            || self.vm.compacting
            || self.vm.queued.is_empty()
            || self.vm.queue_paused
        {
            return;
        }
        let draft = self.vm.queued[0].clone();
        self.apply_action(DesktopAction::QueuedMessageTaken, cx);
        self.send_text(draft, false, None, cx);
    }

    /// Sends the welcome-desk draft once its new session is the open one.
    pub(super) fn flush_welcome_send(&mut self, session_id: &str, cx: &mut Context<Self>) {
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
        self.apply_action(DesktopAction::WelcomeSendConsumed, cx);
        self.apply_action(DesktopAction::ComposerChanged(String::new()), cx);
        self.pending_composer_prefill = Some(String::new());
        self.send_text(text, true, None, cx);
    }

    fn send_text(
        &mut self,
        draft: String,
        clear_composer: bool,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if draft.trim().is_empty() {
            return;
        }
        let Some((session, branch, expected_head)) = self.open_turn_target() else {
            return;
        };
        self.queue_hold_on_return = false;
        self.apply_action(DesktopAction::TurnArmed, cx);
        if clear_composer {
            self.vm.composer_draft.clear();
            if let Some(window) = window {
                self.composer
                    .update(cx, |state, cx| state.set_value("", window, cx));
            }
        }
        cx.notify();
        self.dispatch(
            BridgeCommand::SendMessage {
                session,
                branch,
                expected_head,
                text: draft,
            },
            cx,
        );
    }
}
