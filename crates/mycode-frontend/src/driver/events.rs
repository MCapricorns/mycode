//! Folding core events into actions. Streamed deltas pace themselves in the
//! frontend; the driver only decides which action each event becomes.
use mycode_app::{BridgeCommand, BridgeEvent};

use super::{Effect, FrontendDriver, HeldInterrupt, ToastKind};
use crate::view_model::{
    CHAT_CANCELLED, CacheBreak, FrontendAction, UpdateState, cache_alert_text,
};

impl FrontendDriver {
    /// Whether `session_id` names the chat currently on screen. Borrowed,
    /// not copied: streamed deltas route through here once per fragment.
    fn is_open_session(&self, session_id: &str) -> bool {
        self.vm
            .active
            .as_ref()
            .is_some_and(|active| active.session_id == session_id)
    }

    /// Folds one core event into the view-model state, emitting the effects
    /// the frontend must perform (toasts, update dialog, scroll hints).
    pub fn on_event(&mut self, event: BridgeEvent, out: &mut Vec<Effect>) {
        // A held interrupt belongs to one turn and ends with it, on screen
        // or not.
        let held = self.take_held_interrupt(&event);
        let action = match event {
            BridgeEvent::ChatText { session_id, delta } => {
                if !self.is_open_session(&session_id) {
                    return;
                }
                FrontendAction::ChatDelta(delta)
            }
            BridgeEvent::ChatThinking { session_id, delta } => {
                if !self.is_open_session(&session_id) {
                    return;
                }
                FrontendAction::ChatThinkingDelta(delta)
            }
            BridgeEvent::AssistantStep { session_id, entry } => {
                if !self.is_open_session(&session_id) {
                    return;
                }
                FrontendAction::AssistantStepCommitted(entry)
            }
            BridgeEvent::ChatDone {
                session_id,
                head,
                entry,
            } => {
                if !self.is_open_session(&session_id) {
                    self.park_held_interrupt(held, &session_id, out);
                    return;
                }
                // Refresh the sidebar so the session title picks up the turn.
                self.dispatch(BridgeCommand::ListSessions);
                self.act(FrontendAction::ChatDone { head, entry }, out);
                self.resync_after_turn(&session_id);
                // The turn answered before the stop landed: the message was
                // never interrupted, so it goes out now.
                if let Some(held) = held {
                    self.send_held_interrupt(held, out);
                }
                return;
            }
            BridgeEvent::ChatFailed {
                session_id,
                message,
                head,
            } => {
                let cancelled = message == CHAT_CANCELLED;
                if !self.is_open_session(&session_id) {
                    self.park_held_interrupt(held, &session_id, out);
                    return;
                }
                self.act(FrontendAction::ChatFailed { message, head }, out);
                self.resync_after_turn(&session_id);
                if cancelled {
                    // The stop landed: the held message is the next turn.
                    if let Some(held) = held {
                        self.send_held_interrupt(held, out);
                    }
                } else if let Some(held) = held {
                    // The turn failed on its own; the message never went
                    // out, so it goes back to the composer.
                    self.restore_held_interrupt(held, out);
                }
                return;
            }
            BridgeEvent::SteersReturned { session_id, texts } => {
                self.return_steers(&session_id, texts, out);
                return;
            }
            BridgeEvent::SteerCommitted {
                session_id,
                head,
                entry,
            } => {
                if !self.is_open_session(&session_id) {
                    return;
                }
                FrontendAction::SteerCommitted {
                    session_id,
                    head,
                    entry,
                }
            }
            BridgeEvent::UsageSnapshot {
                session_id,
                model,
                input,
                context,
                context_cache,
                output,
                cache,
                requests,
                elapsed_ms,
            } => {
                if !self.is_open_session(&session_id) {
                    return;
                }
                let previous = self.vm.cache_alert;
                self.act(
                    FrontendAction::UsageSnapshot {
                        model,
                        input,
                        context,
                        context_cache,
                        output,
                        cache,
                        requests,
                        elapsed_ms,
                    },
                    out,
                );
                if self.vm.cache_alert != previous
                    && let Some(alert) = self.vm.cache_alert
                {
                    self.toast(cache_alert_text(&alert), ToastKind::Warning, out);
                }
                return;
            }
            BridgeEvent::ContextCompacting { session_id, active } => {
                // A held interrupt belongs to the session it was armed for,
                // whether or not that chat is on screen. When the summary
                // ends, the cancel goes to that session directly — the
                // core indexes turn tokens per session, so a background
                // chat is cancelled without touching the open one.
                let interrupt = !active
                    && self
                        .held_interrupt
                        .as_ref()
                        .is_some_and(|held| held.session_id == session_id);
                if !self.is_open_session(&session_id) {
                    if interrupt {
                        self.dispatch(BridgeCommand::CancelChat { session_id });
                    }
                    return;
                }
                self.act(FrontendAction::ContextCompacting(active), out);
                if interrupt {
                    self.cancel_turn(out);
                }
                return;
            }
            BridgeEvent::PromptCacheWatch { session_id, cause } => {
                if !self.is_open_session(&session_id) {
                    return;
                }
                let cause = match cause {
                    mycode_app::CacheDisruption::Compaction => CacheBreak::Compaction,
                    mycode_app::CacheDisruption::Steer => CacheBreak::Steer,
                };
                FrontendAction::CacheBreakArmed(cause)
            }
            BridgeEvent::AskRequested {
                session_id,
                questions,
            } => {
                if !self.is_open_session(&session_id) {
                    return;
                }
                FrontendAction::AskRequested(questions)
            }
            BridgeEvent::ToolPreparing {
                session_id,
                provider_call_id,
                name,
                target,
            } => {
                if !self.is_open_session(&session_id) {
                    return;
                }
                FrontendAction::ToolPreparing {
                    provider_call_id,
                    name,
                    target,
                }
            }
            BridgeEvent::ToolStarted {
                session_id,
                call_id,
                provider_call_id,
                name,
                target,
            } => {
                if !self.is_open_session(&session_id) {
                    return;
                }
                FrontendAction::ToolStarted {
                    call_id,
                    provider_call_id,
                    name,
                    target,
                }
            }
            BridgeEvent::ToolProgress {
                session_id,
                call_id,
                provider_call_id,
                name,
                message,
            } => {
                if !self.is_open_session(&session_id) {
                    return;
                }
                FrontendAction::ToolProgress {
                    call_id,
                    provider_call_id,
                    name,
                    message,
                }
            }
            BridgeEvent::ToolCompleted { session_id, entry } => {
                if !self.is_open_session(&session_id) {
                    return;
                }
                FrontendAction::ToolResultAppended(entry)
            }
            BridgeEvent::SessionTitle { session_id, title } => {
                FrontendAction::SessionTitled { session_id, title }
            }
            BridgeEvent::CatalogUpdated { .. } => {
                // The first binding may have been written after bootstrap,
                // before any model chip or catalog surface existed.
                self.catalog_requested = true;
                self.dispatch(BridgeCommand::GetCatalog);
                return;
            }
            BridgeEvent::UpdateAvailable { offer } => {
                // A second offer must not knock a download or a staged
                // package back to "available" — that both lies about the
                // in-flight work and starts another download.
                if matches!(
                    self.vm.update,
                    UpdateState::Downloading { .. } | UpdateState::Ready { .. }
                ) {
                    return;
                }
                let version = offer.version.clone();
                self.act(FrontendAction::UpdateOfferFound(offer), out);
                self.toast(
                    format!(
                        "{} v{version}{}",
                        crate::i18n::t("New version", "发现新版本"),
                        crate::i18n::t(", downloading…", "，正在下载…")
                    ),
                    ToastKind::Info,
                    out,
                );
                // An offer on the wire starts the download right away; the
                // install itself always waits for the user's confirm.
                self.download_update(out);
                return;
            }
            BridgeEvent::SummaryShown { session_id, entry } => {
                if !self.is_open_session(&session_id) {
                    return;
                }
                self.act(FrontendAction::SummaryShown(entry), out);
                return;
            }
            BridgeEvent::CompactFinished {
                session_id,
                message,
                ok,
                head,
            } => {
                if !self.is_open_session(&session_id) {
                    return;
                }
                self.act(FrontendAction::CompactFinished { ok, head }, out);
                self.resync_after_turn(&session_id);
                let text = match message.as_str() {
                    "compacted" => crate::i18n::t(
                        "Context compacted. The next turn uses the summary.",
                        "上下文已压缩。下一轮会使用摘要。",
                    )
                    .to_owned(),
                    "empty" => {
                        crate::i18n::t("Nothing to compact yet.", "现在没有可压缩的上下文。")
                            .to_owned()
                    }
                    "covered" => crate::i18n::t(
                        "Already compacted. New messages can be folded in with another /compact.",
                        "已经压缩过了。有新消息时可以再 /compact。",
                    )
                    .to_owned(),
                    other => other.to_owned(),
                };
                self.toast(
                    text,
                    if ok {
                        ToastKind::Info
                    } else {
                        ToastKind::Error
                    },
                    out,
                );
                return;
            }
            BridgeEvent::UpdateCheckFailed { message } => {
                // Startup (and any later automatic check that reports here)
                // toasts the error. About keeps the short status line and does
                // not paint this string in red.
                if matches!(
                    self.vm.update,
                    UpdateState::Downloading { .. } | UpdateState::Ready { .. }
                ) {
                    return;
                }
                let brief = mycode_app::brief_error(&message);
                self.act(
                    FrontendAction::UpdateStateChanged(UpdateState::Failed(brief.clone())),
                    out,
                );
                self.toast(brief, ToastKind::Error, out);
                return;
            }
        };
        self.act(action, out);
    }

    /// Takes the held interrupt this terminal event ends, if any. A held
    /// interrupt for another session stays put.
    fn take_held_interrupt(&mut self, event: &BridgeEvent) -> Option<HeldInterrupt> {
        let (BridgeEvent::ChatDone { session_id, .. } | BridgeEvent::ChatFailed { session_id, .. }) =
            event
        else {
            return None;
        };
        let held = self.held_interrupt.take()?;
        if &held.session_id != session_id {
            self.held_interrupt = Some(held);
            return None;
        }
        Some(held)
    }

    /// Keeps a held draft for a chat that left the screen: its composer is
    /// not here, so it waits with that chat's parked steers and comes back
    /// when the chat opens.
    fn park_held_interrupt(
        &mut self,
        held: Option<HeldInterrupt>,
        session_id: &str,
        out: &mut Vec<Effect>,
    ) {
        let Some(held) = held else {
            return;
        };
        if let Some(text) = held.send {
            self.act(
                FrontendAction::SteersParked {
                    session_id: session_id.to_owned(),
                    texts: vec![text],
                },
                out,
            );
        }
    }

    /// Reloads a chat that was opened mid-turn (or mid-`/compact`) once
    /// that work has ended, so rows committed while the open was in flight
    /// show up.
    pub(super) fn resync_after_turn(&mut self, session_id: &str) {
        if self.resync_on_turn_end.as_deref() != Some(session_id) {
            return;
        }
        self.resync_on_turn_end = None;
        if self.pending_open.is_none() {
            self.request_open_session(session_id);
        }
    }
}
