//! Folding core events into actions. Streamed deltas pace themselves in the
//! frontend; the driver only decides which action each event becomes.
use mycode_app::{BridgeCommand, BridgeEvent};

use super::{Effect, FrontendDriver, ToastKind};
use crate::view_model::{
    CHAT_CANCELLED, CacheBreak, FrontendAction, UpdateState, cache_alert_text,
};

impl FrontendDriver {
    /// Folds one core event into the view-model state, emitting the effects
    /// the frontend must perform (toasts, update dialog, scroll hints).
    pub fn on_event(&mut self, event: BridgeEvent, out: &mut Vec<Effect>) {
        let active_session = self.open_session_id();
        let matches_active = |session_id: &str| active_session.as_deref() == Some(session_id);
        let action = match event {
            BridgeEvent::ChatText { session_id, delta } => {
                if !matches_active(&session_id) {
                    return;
                }
                FrontendAction::ChatDelta(delta)
            }
            BridgeEvent::ChatThinking { session_id, delta } => {
                if !matches_active(&session_id) {
                    return;
                }
                FrontendAction::ChatThinkingDelta(delta)
            }
            BridgeEvent::AssistantStep { session_id, entry } => {
                if !matches_active(&session_id) {
                    return;
                }
                FrontendAction::AssistantStepCommitted(entry)
            }
            BridgeEvent::ChatDone {
                session_id,
                head,
                entry,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                // Refresh the sidebar so the session title picks up the turn.
                self.dispatch(BridgeCommand::ListSessions);
                self.act(FrontendAction::ChatDone { head, entry }, out);
                self.resync_after_turn(&session_id);
                self.pump_queued_send(out);
                return;
            }
            BridgeEvent::ChatFailed {
                session_id,
                message,
                head,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                let cancelled = message == CHAT_CANCELLED;
                self.act(FrontendAction::ChatFailed { message, head }, out);
                self.resync_after_turn(&session_id);
                // A user interrupt frees the turn; queued follow-ups start next.
                // Provider errors keep the queue so a failed retry cannot loop.
                if cancelled {
                    self.pump_queued_send(out);
                }
                return;
            }
            BridgeEvent::SteersReturned { session_id, texts } => {
                if !matches_active(&session_id) {
                    return;
                }
                self.act(FrontendAction::SteersReturned(texts), out);
                // Steers handed back after a user stop join a held queue.
                if self.queue_hold_on_return {
                    self.act(FrontendAction::QueuePaused(true), out);
                }
                self.pump_queued_send(out);
                return;
            }
            BridgeEvent::SteerCommitted {
                session_id,
                head,
                entry,
            } => {
                if !matches_active(&session_id) {
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
                if !matches_active(&session_id) {
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
            BridgeEvent::PromptCacheWatch { session_id, cause } => {
                if !matches_active(&session_id) {
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
                if !matches_active(&session_id) {
                    return;
                }
                FrontendAction::AskRequested(questions)
            }
            BridgeEvent::ToolStarted {
                session_id,
                call_id,
                name,
                target,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                FrontendAction::ToolStarted {
                    call_id,
                    name,
                    target,
                }
            }
            BridgeEvent::ToolProgress {
                session_id,
                call_id,
                name,
                message,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                FrontendAction::ToolProgress {
                    call_id,
                    name,
                    message,
                }
            }
            BridgeEvent::ToolCompleted { session_id, entry } => {
                if !matches_active(&session_id) {
                    return;
                }
                FrontendAction::ToolResultAppended(entry)
            }
            BridgeEvent::SessionTitle { session_id, title } => {
                FrontendAction::SessionTitled { session_id, title }
            }
            BridgeEvent::CatalogUpdated { .. } => {
                if self.catalog_requested || self.vm.catalog.is_some() || self.catalog_needed() {
                    self.catalog_requested = true;
                    self.dispatch(BridgeCommand::GetCatalog);
                }
                return;
            }
            BridgeEvent::CopilotSignedIn => {
                // The provider entry and key landed; refresh the settings
                // projection so the row and keyed badge appear immediately.
                // The catalog stays a lazy load: model picks read it anyway.
                self.dispatch(BridgeCommand::LoadSettings);
                if self.catalog_requested || self.vm.catalog.is_some() || self.catalog_needed() {
                    self.catalog_requested = true;
                    self.dispatch(BridgeCommand::GetCatalog);
                }
                FrontendAction::CopilotSignInFinished(Ok(()))
            }
            BridgeEvent::CopilotSignInFailed { message } => {
                FrontendAction::CopilotSignInFinished(Err(message))
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
                if !matches_active(&session_id) {
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
                if !matches_active(&session_id) {
                    return;
                }
                self.act(FrontendAction::CompactFinished { ok, head }, out);
                // Follow-ups queued while the composer was locked start now.
                self.pump_queued_send(out);
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
                // used to drop the error. Toast it. About keeps the short
                // status line and does not paint this string in red.
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

    /// Reloads a chat that was opened mid-turn once that turn has ended,
    /// so steps committed while the open was in flight show up. A queued
    /// follow-up about to start the next turn defers the reload to that
    /// turn's end, so the reopen never races the send.
    fn resync_after_turn(&mut self, session_id: &str) {
        if self.resync_on_turn_end.as_deref() != Some(session_id) {
            return;
        }
        if !self.vm.queued.is_empty() && !self.vm.queue_paused {
            return;
        }
        self.resync_on_turn_end = None;
        if self.pending_open.is_none() {
            self.request_open_session(session_id);
        }
    }
}
