//! The bridge half of the workspace: folding core events and replies into
//! actions. Driving one model turn lives in [`turn`].
use gpui_kit::Context;

use mycode_app::{BridgeCommand, BridgeEvent, BridgeReply};

use crate::view_model::{
    CHAT_CANCELLED, CacheBreak, DesktopAction, SettingsState, UpdateState, cache_alert_text,
};
use crate::workspace::Workspace;

mod turn;

#[cfg(test)]
mod tests;

/// Replies without embedded identity still belong to the request's session.
/// A late cancel, recall, or search must never mutate a newly opened chat.
#[derive(Debug)]
pub(super) enum ReplyScope {
    Global,
    Open(String),
    Session {
        session: String,
        branch: Option<String>,
    },
    Mention {
        session: String,
        query: String,
    },
}

impl ReplyScope {
    pub(super) fn for_command(command: &BridgeCommand) -> Self {
        match command {
            BridgeCommand::OpenSession(session) => Self::Open(session.to_string()),
            BridgeCommand::ChatTurn {
                session, branch, ..
            }
            | BridgeCommand::SendMessage {
                session, branch, ..
            }
            | BridgeCommand::RecallMessage {
                session, branch, ..
            }
            | BridgeCommand::LoadOlder {
                session, branch, ..
            } => Self::Session {
                session: session.to_string(),
                branch: Some(branch.to_string()),
            },
            BridgeCommand::CancelChat { session_id }
            | BridgeCommand::CancelSubagent { session_id, .. }
            | BridgeCommand::ListResources { session_id }
            | BridgeCommand::AskAnswer { session_id, .. }
            | BridgeCommand::AskDismiss { session_id } => Self::Session {
                session: session_id.clone(),
                branch: None,
            },
            BridgeCommand::SearchProjectFiles { session_id, query } => Self::Mention {
                session: session_id.clone(),
                query: query.clone(),
            },
            _ => Self::Global,
        }
    }

    pub(super) fn applies(
        &self,
        vm: &crate::view_model::WorkspaceState,
        pending_open: Option<&str>,
    ) -> bool {
        match self {
            Self::Global => true,
            Self::Open(session) => pending_open == Some(session.as_str()),
            Self::Session { session, branch } => vm.active.as_ref().is_some_and(|active| {
                &active.session_id == session
                    && branch
                        .as_ref()
                        .is_none_or(|branch| branch == &active.branch_id)
            }),
            Self::Mention { session, query } => {
                vm.active
                    .as_ref()
                    .is_some_and(|active| &active.session_id == session)
                    && vm.mention.as_ref().is_some_and(|mention| {
                        mention.kind == crate::view_model::MentionKind::File
                            && &mention.fragment == query
                    })
            }
        }
    }
}

fn failed_start(message: String) -> DesktopAction {
    DesktopAction::ChatFailed {
        message,
        head: String::new(),
    }
}

impl Workspace {
    pub(super) fn apply_event(&mut self, event: BridgeEvent, cx: &mut Context<Self>) {
        let active_session = self.open_session_id();
        let matches_active = |session_id: &str| active_session.as_deref() == Some(session_id);
        let action = match event {
            BridgeEvent::ChatText { session_id, delta } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::ChatDelta(delta)
            }
            BridgeEvent::ChatThinking { session_id, delta } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::ChatThinkingDelta(delta)
            }
            BridgeEvent::AssistantStep { session_id, entry } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::AssistantStepCommitted(entry)
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
                self.dispatch(BridgeCommand::ListSessions, cx);
                self.apply_action(DesktopAction::ChatDone { head, entry }, cx);
                self.pump_queued_send(cx);
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
                self.apply_action(DesktopAction::ChatFailed { message, head }, cx);
                // A user interrupt frees the turn; queued follow-ups start next.
                // Provider errors keep the queue so a failed retry cannot loop.
                if cancelled {
                    self.pump_queued_send(cx);
                }
                return;
            }
            BridgeEvent::SteersReturned { session_id, texts } => {
                if !matches_active(&session_id) {
                    return;
                }
                self.apply_action(DesktopAction::SteersReturned(texts), cx);
                // Steers handed back after a user stop join a held queue.
                if self.queue_hold_on_return {
                    self.apply_action(DesktopAction::QueuePaused(true), cx);
                }
                self.pump_queued_send(cx);
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
                DesktopAction::SteerCommitted {
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
                self.apply_action(
                    DesktopAction::UsageSnapshot {
                        model,
                        input,
                        context,
                        context_cache,
                        output,
                        cache,
                        requests,
                        elapsed_ms,
                    },
                    cx,
                );
                if self.vm.cache_alert != previous
                    && let Some(alert) = self.vm.cache_alert
                {
                    self.push_toast(
                        cache_alert_text(&alert),
                        crate::workspace::ToastKind::Warning,
                        cx,
                    );
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
                DesktopAction::CacheBreakArmed(cause)
            }
            BridgeEvent::AskRequested {
                session_id,
                questions,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::AskRequested(questions)
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
                DesktopAction::ToolStarted {
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
                DesktopAction::ToolProgress {
                    call_id,
                    name,
                    message,
                }
            }
            BridgeEvent::ToolCompleted { session_id, entry } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::ToolResultAppended(entry)
            }
            BridgeEvent::CatalogUpdated { .. } => {
                if self.catalog_requested || self.vm.catalog.is_some() || self.catalog_needed() {
                    self.catalog_requested = true;
                    self.dispatch(BridgeCommand::GetCatalog, cx);
                }
                return;
            }
            BridgeEvent::CopilotSignedIn => {
                // The provider entry and key landed; refresh the settings
                // projection so the row and keyed badge appear immediately,
                // and reload the catalog when in use so live Codex models
                // take effect right away.
                self.dispatch(BridgeCommand::LoadSettings, cx);
                if self.catalog_requested || self.vm.catalog.is_some() || self.catalog_needed() {
                    self.catalog_requested = true;
                    self.dispatch(BridgeCommand::GetCatalog, cx);
                }
                DesktopAction::CopilotSignInFinished(Ok(()))
            }
            BridgeEvent::CopilotSignInFailed { message } => {
                DesktopAction::CopilotSignInFinished(Err(message))
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
                self.apply_action(DesktopAction::UpdateOfferFound(offer), cx);
                self.push_toast(
                    format!(
                        "{} v{version}{}",
                        crate::i18n::t("New version", "发现新版本"),
                        crate::i18n::t(", downloading…", ",正在下载…")
                    ),
                    crate::workspace::ToastKind::Info,
                    cx,
                );
                // An offer on the wire starts the download right away; the
                // install itself always waits for the user's confirm.
                self.on_download_update(cx);
                return;
            }
            BridgeEvent::SummaryShown { session_id, entry } => {
                if !matches_active(&session_id) {
                    return;
                }
                self.apply_action(DesktopAction::SummaryShown(entry), cx);
                return;
            }
            BridgeEvent::CompactFinished {
                session_id,
                message,
                ok,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
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
                self.push_toast(
                    text,
                    if ok {
                        crate::workspace::ToastKind::Info
                    } else {
                        crate::workspace::ToastKind::Error
                    },
                    cx,
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
                self.apply_action(
                    DesktopAction::UpdateStateChanged(UpdateState::Failed(brief.clone())),
                    cx,
                );
                self.push_toast(brief, crate::workspace::ToastKind::Error, cx);
                return;
            }
        };
        self.apply_action(action, cx);
    }

    pub(super) fn apply_reply(&mut self, reply: BridgeReply, cx: &mut Context<Self>) {
        match reply {
            BridgeReply::Sessions(Ok(sessions)) => {
                self.apply_action(DesktopAction::SessionsLoaded(sessions), cx);
            }
            BridgeReply::Created(Ok(summary)) => {
                let session_id = summary.session_id.clone();
                self.apply_action(DesktopAction::SessionCreated(summary), cx);
                if self.vm.pending_welcome_send.is_some() {
                    self.welcome_send_session = Some(session_id.clone());
                }
                // A fresh session belongs to the workspace the sidebar shows.
                self.bind_session_workspace(&session_id, cx);
                self.request_open_session(&session_id, cx);
                if let Some(project) = self.pending_project.take() {
                    self.attach_project(&session_id, &project, cx);
                }
                self.dispatch(BridgeCommand::ListSessions, cx);
            }
            BridgeReply::Conversation(Ok(conversation)) => {
                let session_id = conversation.session_id.clone();
                if self.suppress_open {
                    if self.welcome_send_session.as_deref() == Some(session_id.as_str()) {
                        self.welcome_send_session = None;
                        self.apply_action(DesktopAction::WelcomeSendConsumed, cx);
                    }
                    return;
                }
                if self
                    .pending_open
                    .as_ref()
                    .is_some_and(|pending| pending != &session_id)
                {
                    return;
                }
                if let Some(focus) = self.focused_project.clone() {
                    let belongs = crate::view_model::project_of_session(
                        &self.vm.session_projects,
                        &session_id,
                    )
                    .is_some_and(|bound| crate::view_model::same_project_path(bound, &focus));
                    if !belongs {
                        self.flush_welcome_send(&session_id, cx);
                        return;
                    }
                }
                self.pending_open = None;
                let switching = self
                    .vm
                    .active
                    .as_ref()
                    .is_some_and(|active| active.session_id != conversation.session_id);
                self.apply_action(DesktopAction::ConversationOpened(conversation), cx);
                if switching {
                    self.pending_composer_prefill = Some(String::new());
                    self.mention_query = None;
                }
                self.follow_session_project(&session_id, cx);
                self.dispatch(
                    BridgeCommand::ListResources {
                        session_id: session_id.clone(),
                    },
                    cx,
                );
                self.refresh_skills(cx);
                self.flush_welcome_send(&session_id, cx);
            }
            BridgeReply::Older(Ok(page)) => {
                let applicable = self.vm.active.as_ref().is_some_and(|active| {
                    active.session_id == page.session_id
                        && active.older_before.as_deref() == Some(page.requested_before.as_str())
                });
                let pinned = applicable && self.conversation_follows_tail();
                if applicable && !pinned {
                    self.capture_scroll_hold();
                }
                self.apply_action(
                    DesktopAction::OlderLoaded {
                        session_id: page.session_id,
                        entries: page.entries,
                        older_before: page.older,
                        requested_before: page.requested_before,
                    },
                    cx,
                );
                if pinned {
                    self.conversation_scroll.scroll_to_bottom();
                }
            }
            BridgeReply::Older(Err(message)) => {
                self.apply_action(DesktopAction::HistoryLoadFinished, cx);
                self.apply_action(DesktopAction::Failed(message), cx);
            }
            BridgeReply::Resources(Ok(files)) => {
                self.apply_action(DesktopAction::ResourcesLoaded(files), cx);
            }
            BridgeReply::ProjectFiles(Ok(files)) => {
                self.apply_action(DesktopAction::MentionFiles(files), cx);
            }
            // A failed mention search just leaves the menu empty.
            BridgeReply::ProjectFiles(Err(_)) => {}
            BridgeReply::CopilotSignInStarted(Ok(info)) => {
                self.apply_action(
                    DesktopAction::CopilotSignInStarted(crate::view_model::CopilotSignIn {
                        user_code: info.user_code,
                        verification_uri: info.verification_uri,
                    }),
                    cx,
                );
            }
            BridgeReply::CopilotSignInStarted(Err(message)) => {
                self.apply_action(DesktopAction::CopilotSignInFinished(Err(message)), cx);
            }
            BridgeReply::AskAnswered(Ok(())) => {}
            BridgeReply::Sent {
                session,
                outcome: Ok((head, entry)),
            } => {
                // A send that raced a session switch must not touch the
                // conversation now open; the committed entry lives in its
                // own session's ledger and shows on reopen.
                let applicable = self
                    .vm
                    .active
                    .as_ref()
                    .is_some_and(|active| active.session_id == session);
                if !applicable {
                    return;
                }
                self.apply_action(DesktopAction::MessageSent { head, entry }, cx);
                self.begin_chat_turn(cx);
            }
            BridgeReply::Settings(Ok(loaded)) => {
                self.note_config_repairs(&loaded.repairs, cx);
                let revision = loaded.revision.get();
                let mut state =
                    SettingsState::from_settings(&loaded.settings, revision, loaded.provider_keys);
                state.mcp_with_keys = loaded.mcp_keys;
                // A dirty or in-flight editor keeps its appearance. Applying
                // the disk copy here would undo unsaved appearance edits
                // before the reducer can refuse the document swap.
                let preserve_editor = self
                    .vm
                    .settings
                    .as_ref()
                    .is_some_and(|settings| settings.dirty || settings.saving);
                if !preserve_editor {
                    self.paint_appearance(&state.theme, cx);
                    self.proxy_sync_pending = true;
                }
                self.apply_action(DesktopAction::SettingsLoaded(Box::new(state)), cx);
            }
            BridgeReply::Exported(Ok(_summary)) => {}
            BridgeReply::Exported(Err(message)) => {
                self.apply_action(
                    DesktopAction::Failed(format!(
                        "{} {message}",
                        crate::i18n::t("export failed:", "导出失败:")
                    )),
                    cx,
                );
            }
            BridgeReply::Imported(Err(message))
                if message == mycode_app::IMPORT_NEEDS_MCP_CONFIRM =>
            {
                // Not a failure: the Data card asks before MCP commands that
                // start programs on this computer are added or replaced.
                self.import_confirm = self.import_request.take();
                cx.notify();
            }
            BridgeReply::Imported(Ok(summary)) => {
                self.import_request = None;
                // Reload everything the bundle replaced: settings, key
                // markers, and UI state. Sessions were not touched.
                self.dispatch(BridgeCommand::LoadSettings, cx);
                self.dispatch(BridgeCommand::LoadUiState, cx);
                let message = if summary.secrets > 0 {
                    format!(
                        "{} {} {}",
                        crate::i18n::t("imported configuration and", "已导入配置和"),
                        summary.secrets,
                        crate::i18n::t("API key(s)", "个 API 密钥")
                    )
                } else {
                    crate::i18n::t("imported configuration", "已导入配置").to_owned()
                };
                self.push_toast(message, crate::workspace::ToastKind::Info, cx);
            }
            BridgeReply::Imported(Err(message)) => {
                self.import_request = None;
                self.apply_action(
                    DesktopAction::Failed(format!(
                        "{} {message}",
                        crate::i18n::t("import failed:", "导入失败:")
                    )),
                    cx,
                );
            }
            BridgeReply::Recalled(Ok((conversation, edit))) => {
                let prefill = edit;
                let session_id = conversation.session_id.clone();
                self.apply_action(
                    DesktopAction::ConversationOpened((*conversation).clone()),
                    cx,
                );
                self.follow_session_project(&session_id, cx);
                if prefill.is_some() {
                    self.pending_composer_prefill = prefill;
                }
                cx.notify();
            }
            BridgeReply::SessionDeleted(Ok(())) => {
                // The row delete handler updates only its target session.
                self.dispatch(BridgeCommand::ListSessions, cx);
            }
            BridgeReply::SettingsSaved(Ok(revision)) => {
                self.apply_action(
                    DesktopAction::SettingsSaved {
                        revision: revision.get(),
                        edit_epoch: self.settings_save_epoch,
                    },
                    cx,
                );
                // An edit that landed while this write was in flight stays
                // dirty. Persist it unless a text-field debounce is already
                // waiting to write the latest draft.
                self.continue_settings_save(cx);
            }
            BridgeReply::ProviderKeySaved(Ok((provider_keys, mcp_keys))) => {
                // Refresh the key markers in place: reloading settings here
                // would race the concurrently running settings save and wipe
                // the just-added provider (and any other unsaved edits).
                let stored = mcp_keys.clone();
                self.apply_action(
                    DesktopAction::ProviderKeySaved {
                        provider_keys,
                        mcp_keys,
                    },
                    cx,
                );
                // An enable that waited on this key save may probe now.
                for server_id in stored {
                    if self.pending_mcp_enable.contains(&server_id)
                        && !self.vm.mcp_probing.contains(&server_id)
                    {
                        self.probe_mcp_for_enable(&server_id, cx);
                    }
                }
            }
            BridgeReply::ChatStarted(Ok(())) => {}
            // The turn unwinds over the event channel; the reply itself
            // carries no state.
            BridgeReply::ChatCancelled(Ok(())) => {}
            // The core has no turn for this session, yet the composer still
            // shows one running: settle it like a quiet cancel so Stop and
            // the queue are not left stuck behind a turn that is gone.
            BridgeReply::ChatCancelled(Err(_)) => {
                if self.vm.sending {
                    self.apply_action(
                        DesktopAction::ChatFailed {
                            message: CHAT_CANCELLED.to_owned(),
                            head: String::new(),
                        },
                        cx,
                    );
                    self.pump_queued_send(cx);
                }
            }
            BridgeReply::Steered(_) => {}
            BridgeReply::SubagentCancelled(Err(message)) => {
                self.push_toast(message, crate::workspace::ToastKind::Info, cx);
            }
            BridgeReply::SubagentCancelled(Ok(())) => {}
            BridgeReply::McpTools {
                server_id,
                outcome: Ok(tools),
            } => {
                self.apply_action(
                    DesktopAction::McpToolsListed {
                        server_id: server_id.clone(),
                        tools,
                    },
                    cx,
                );
                // A probe that gated an enable switch lands it now.
                if self.pending_mcp_enable.remove(&server_id)
                    && let Some(index) = self.vm.settings.as_ref().and_then(|settings| {
                        settings
                            .mcp_servers
                            .iter()
                            .position(|server| server.id == server_id)
                    })
                {
                    self.apply_action(DesktopAction::SettingsMcpToggled(index, true), cx);
                }
            }
            BridgeReply::McpTools {
                server_id,
                outcome: Err(message),
            } => {
                self.pending_mcp_enable.remove(&server_id);
                self.apply_action(DesktopAction::McpProbeFailed { server_id, message }, cx);
            }
            BridgeReply::WebBackendProbed {
                backend_id,
                outcome,
            } => {
                if self.pending_web_enable.remove(&backend_id) {
                    match outcome {
                        Ok(()) => {
                            let index = self.vm.settings.as_ref().and_then(|settings| {
                                settings
                                    .web_backends
                                    .iter()
                                    .position(|backend| backend.id == backend_id)
                            });
                            if let Some(index) = index {
                                self.apply_action(
                                    DesktopAction::SettingsBackendToggled(index, true),
                                    cx,
                                );
                                self.push_toast(
                                    crate::i18n::t(
                                        "Search backend verified and enabled",
                                        "搜索后端已验证并启用",
                                    )
                                    .to_owned(),
                                    crate::workspace::ToastKind::Info,
                                    cx,
                                );
                            }
                        }
                        Err(message) => {
                            self.push_toast(message, crate::workspace::ToastKind::Error, cx);
                        }
                    }
                }
            }
            BridgeReply::Catalog(Ok(info)) => {
                let refreshed = self.pending_catalog_refresh;
                self.pending_catalog_refresh = false;
                self.apply_action(
                    DesktopAction::CatalogLoaded {
                        document: info.document,
                        fetched_at: info.fetched_at,
                    },
                    cx,
                );
                if refreshed {
                    self.push_toast(
                        crate::i18n::t("Provider catalog refreshed", "服务商目录已刷新").to_owned(),
                        crate::workspace::ToastKind::Info,
                        cx,
                    );
                }
            }
            BridgeReply::Catalog(Err(message)) => {
                // A failed lazy load clears the flag so the next surface
                // open retries; a failed manual refresh also toasts.
                self.catalog_requested = false;
                if self.pending_catalog_refresh {
                    self.pending_catalog_refresh = false;
                    self.apply_action(DesktopAction::Failed(message), cx);
                }
            }
            BridgeReply::OpenDesignModels(Ok((info, count))) => {
                self.apply_action(DesktopAction::OpenDesignLoading(false), cx);
                self.apply_action(
                    DesktopAction::CatalogLoaded {
                        document: info.document,
                        fetched_at: info.fetched_at,
                    },
                    cx,
                );
                // The open form re-checks the fresh list.
                if self.vm.models_subview == crate::view_model::ModelsSubview::OpenDesign {
                    self.apply_action(
                        DesktopAction::ActivePresetChanged(Some(
                            mycode_providers::catalog::opendesign::PROVIDER_ID.to_owned(),
                        )),
                        cx,
                    );
                }
                self.push_toast(
                    format!(
                        "{count} {}",
                        crate::i18n::t("OpenDesign models loaded", "个 OpenDesign 模型已加载")
                    ),
                    crate::workspace::ToastKind::Info,
                    cx,
                );
            }
            BridgeReply::OpenDesignModels(Err(message)) => {
                self.apply_action(DesktopAction::OpenDesignLoading(false), cx);
                self.apply_action(DesktopAction::Failed(message), cx);
            }
            BridgeReply::UiState(Ok((mut ui_state, repairs))) => {
                self.note_config_repairs(&repairs, cx);
                // First run (or a reset document) has no workspaces yet:
                // seed the default one from the last-opened project so the
                // sidebar always has a home for unbound sessions.
                let migrated = ui_state.workspaces.is_empty();
                if migrated {
                    let mut folders = Vec::new();
                    if let Some(last) = ui_state.last_project.clone()
                        && !last.trim().is_empty()
                    {
                        folders.push(last);
                    }
                    let default = mycode_config::WorkspaceDef::generate(
                        crate::i18n::t("Default", "默认"),
                        folders,
                    );
                    ui_state.active_workspace = Some(default.id.clone());
                    ui_state.workspaces.push(default);
                }
                self.apply_action(
                    DesktopAction::UiStateLoaded {
                        recents: ui_state.recent_projects,
                        last_project: ui_state.last_project,
                        auto_update: ui_state.auto_update,
                        selected_provider: ui_state.selected_provider,
                        selected_model: ui_state.selected_model,
                        selected_reasoning: ui_state.selected_reasoning,
                        session_projects: ui_state.session_projects,
                        session_models: ui_state.session_models,
                        workspaces: ui_state.workspaces,
                        session_workspaces: ui_state.session_workspaces,
                        active_workspace: ui_state.active_workspace,
                        recent_models: ui_state.recent_models,
                        thinking_collapsed: ui_state.thinking_collapsed,
                        window_size: ui_state.window_size,
                    },
                    cx,
                );
                if migrated {
                    self.persist_ui_state(cx);
                }
                self.restore_session_projects(cx);
                self.refresh_skills(cx);
            }
            BridgeReply::UiState(Err(_)) => {}
            BridgeReply::UiStateSaved(Ok(())) => {}
            BridgeReply::UiStateSaved(Err(message)) => {
                self.apply_action(DesktopAction::Failed(message), cx);
            }
            BridgeReply::ProjectSet(Ok(())) => {}
            BridgeReply::ProjectSet(Err(message)) => {
                self.apply_action(DesktopAction::Failed(message), cx);
            }
            BridgeReply::UpdateChecked(Ok(None)) => {
                if matches!(
                    self.vm.update,
                    UpdateState::Downloading { .. } | UpdateState::Ready { .. }
                ) {
                    let _ = self.take_manual_update_check();
                    return;
                }
                self.apply_action(DesktopAction::UpdateStateChanged(UpdateState::UpToDate), cx);
                if self.take_manual_update_check() {
                    self.push_toast(
                        crate::i18n::t("You're up to date", "已是最新版本").to_owned(),
                        crate::workspace::ToastKind::Info,
                        cx,
                    );
                }
            }
            BridgeReply::UpdateChecked(Ok(Some(offer))) => {
                // A manual check can be answered after its download already
                // started (an event offer landed mid-check); the state must
                // not fall back to "available" nor start a second download.
                if matches!(
                    self.vm.update,
                    UpdateState::Downloading { .. } | UpdateState::Ready { .. }
                ) {
                    let _ = self.take_manual_update_check();
                    return;
                }
                let version = offer.version.clone();
                // The offer action also flips the state to Available; the
                // download button reads `last_offer`, so the two must land
                // together or the button is dead.
                self.apply_action(DesktopAction::UpdateOfferFound(offer), cx);
                if self.take_manual_update_check() {
                    self.push_toast(
                        format!("v{version} {}", crate::i18n::t("is available", "可用")),
                        crate::workspace::ToastKind::Info,
                        cx,
                    );
                }
                // A manual check downloads too; the dialog prompts install.
                self.on_download_update(cx);
            }
            BridgeReply::UpdateChecked(Err(message)) => {
                if matches!(
                    self.vm.update,
                    UpdateState::Downloading { .. } | UpdateState::Ready { .. }
                ) {
                    let _ = self.take_manual_update_check();
                    return;
                }
                let brief = mycode_app::brief_error(&message);
                self.apply_action(
                    DesktopAction::UpdateStateChanged(UpdateState::Failed(brief.clone())),
                    cx,
                );
                // Manual and automatic checks both toast. The About row does
                // not render `brief`.
                let _ = self.take_manual_update_check();
                self.push_toast(brief, crate::workspace::ToastKind::Error, cx);
            }
            BridgeReply::UpdateDownloaded(Ok(prepared)) => {
                let version = self
                    .vm
                    .last_offer
                    .as_ref()
                    .map(|offer| offer.version.clone());
                self.apply_action(DesktopAction::UpdateStaged(prepared), cx);
                let message = match version {
                    Some(version) => format!(
                        "v{version} {}",
                        crate::i18n::t("is ready to install", "已就绪,可安装")
                    ),
                    None => {
                        crate::i18n::t("Update is ready to install", "更新已就绪,可安装").to_owned()
                    }
                };
                self.push_toast(message, crate::workspace::ToastKind::Info, cx);
                // Downloading runs on its own; installing never does.
                self.apply_action(DesktopAction::UpdateDialogToggled(true), cx);
            }
            BridgeReply::UpdateDownloaded(Err(message)) => {
                let brief = mycode_app::brief_error(&message);
                self.apply_action(
                    DesktopAction::UpdateStateChanged(UpdateState::Failed(brief.clone())),
                    cx,
                );
                self.push_toast(brief, crate::workspace::ToastKind::Error, cx);
                self.apply_action(DesktopAction::UpdateDialogToggled(true), cx);
            }
            BridgeReply::SettingsSaved(Err(message)) => {
                self.apply_action(DesktopAction::SettingsSaveFailed(message), cx);
            }
            BridgeReply::SessionDeleted(Err(message)) => {
                // The sidebar row was dropped optimistically. Put the session
                // back from disk when the delete did not actually happen.
                self.apply_action(DesktopAction::Failed(message), cx);
                self.dispatch(BridgeCommand::ListSessions, cx);
                self.dispatch(BridgeCommand::LoadUiState, cx);
            }
            BridgeReply::Created(Err(message)) => {
                self.welcome_send_session = None;
                self.apply_action(DesktopAction::WelcomeSendConsumed, cx);
                self.apply_action(DesktopAction::Failed(message), cx);
            }
            BridgeReply::Conversation(Err(message)) => {
                if self.welcome_send_session.is_some() {
                    self.welcome_send_session = None;
                    self.apply_action(DesktopAction::WelcomeSendConsumed, cx);
                }
                self.apply_action(DesktopAction::Failed(message), cx);
            }
            BridgeReply::Sent {
                session,
                outcome: Err(message),
            } => {
                if self
                    .vm
                    .active
                    .as_ref()
                    .is_some_and(|active| active.session_id == session)
                {
                    self.apply_action(failed_start(message), cx);
                }
            }
            BridgeReply::ChatStarted(Err(message)) => {
                self.apply_action(failed_start(message), cx);
            }
            BridgeReply::ProviderKeySaved(Err(message)) => {
                // Pending enables that waited on this key save can never
                // probe now; entries already probing keep their gate.
                let probing = &self.vm.mcp_probing;
                self.pending_mcp_enable.retain(|id| probing.contains(id));
                self.apply_action(DesktopAction::Failed(message), cx);
            }
            BridgeReply::Sessions(Err(message))
            | BridgeReply::Settings(Err(message))
            | BridgeReply::Recalled(Err(message))
            | BridgeReply::Resources(Err(message))
            | BridgeReply::AskAnswered(Err(message)) => {
                self.apply_action(DesktopAction::Failed(message), cx);
            }
            // The first-frame acknowledgement carries no UI state; a lost
            // reply only means the deferred refresh ran anyway.
            BridgeReply::UiReady(_) => {}
        }
    }
}
