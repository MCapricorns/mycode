//! The bridge half of the workspace: folding core events and replies into
//! actions, and driving one model turn end to end.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{Context, Window};

use mycode_app::{BranchId, BridgeCommand, BridgeEvent, BridgeReply, SessionId};

use crate::view_model::{CHAT_CANCELLED, DesktopAction, SettingsState, UpdateState};
use crate::workspace::Workspace;

impl Workspace {
    pub(super) fn apply_event(&mut self, event: BridgeEvent, cx: &mut Context<Self>) {
        let active_session = self.vm.active.as_ref().map(|c| c.session_id.clone());
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
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                let cancelled = message == CHAT_CANCELLED;
                self.apply_action(DesktopAction::ChatFailed(message), cx);
                // A user interrupt frees the turn; queued follow-ups start next.
                // Provider errors keep the queue so a failed retry cannot loop.
                if cancelled {
                    self.pump_queued_send(cx);
                }
                return;
            }
            BridgeEvent::Notice {
                session_id,
                message,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::Failed(message)
            }
            BridgeEvent::UsageSnapshot {
                session_id,
                model,
                input,
                output,
                cache,
                elapsed_ms,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::UsageSnapshot {
                    model,
                    input,
                    output,
                    cache,
                    elapsed_ms,
                }
            }
            BridgeEvent::UsageRecorded {
                session_id,
                provider,
                model,
                input,
                output,
                cache,
                elapsed_ms,
                entry,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::UsageRecorded {
                    provider,
                    model,
                    input,
                    output,
                    cache,
                    elapsed_ms,
                    entry,
                }
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
                self.dispatch(BridgeCommand::GetCatalog, cx);
                return;
            }
            BridgeEvent::CopilotSignedIn => {
                // The provider entry and key landed; refresh the settings
                // projection so the row and keyed badge appear immediately.
                self.dispatch(BridgeCommand::LoadSettings, cx);
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
                let notes_url = offer.notes_url.clone();
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
                self.apply_action(
                    DesktopAction::UpdateStateChanged(UpdateState::Available {
                        version,
                        notes_url,
                    }),
                    cx,
                );
                // An offer on the wire starts the download right away; the
                // install itself always waits for the user's confirm.
                self.on_download_update(cx);
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
                self.dispatch(BridgeCommand::ListResources { session_id }, cx);
                self.refresh_skills(cx);
            }
            BridgeReply::Older(Ok(page)) => {
                let applicable = self.vm.active.as_ref().is_some_and(|active| {
                    active.session_id == page.session_id
                        && active.older_before.as_deref() == Some(page.requested_before.as_str())
                });
                let pinned = applicable && self.conversation_pinned();
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
            BridgeReply::Sent(Ok((head, entry))) => {
                self.apply_action(DesktopAction::MessageSent { head, entry }, cx);
                self.begin_chat_turn(cx);
            }
            BridgeReply::Settings(Ok((settings, revision, provider_keys, mcp_keys))) => {
                let revision = revision.get();
                let mut state = SettingsState::from_settings(&settings, revision, provider_keys);
                state.mcp_with_keys = mcp_keys;
                // A dirty or in-flight editor keeps its palette and
                // user-agent field. Applying the disk copy here would undo
                // unsaved appearance edits before the reducer can refuse the
                // document swap.
                let preserve_editor = self
                    .vm
                    .settings
                    .as_ref()
                    .is_some_and(|settings| settings.dirty || settings.saving);
                if !preserve_editor {
                    if cx.theme().mode != ThemeMode::Dark {
                        Theme::change(ThemeMode::Dark, None, cx);
                    }
                    crate::ui::desk::apply_palette(Theme::global_mut(cx), &state.palette);
                    self.applied_font_size.clear();
                    Theme::sync_base(cx);
                    self.ua_sync_pending = true;
                }
                self.apply_action(DesktopAction::SettingsLoaded(state), cx);
                self.apply_runtime_shell();
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
            BridgeReply::Imported(Ok(summary)) => {
                // Reload everything the bundle may have replaced.
                self.dispatch(BridgeCommand::LoadSettings, cx);
                self.dispatch(BridgeCommand::LoadUiState, cx);
                self.dispatch(BridgeCommand::ListSessions, cx);
                if summary.sessions > 0 {
                    self.apply_action(
                        DesktopAction::Failed(format!(
                            "imported {} new session(s); restart to see restored history",
                            summary.sessions
                        )),
                        cx,
                    );
                }
            }
            BridgeReply::Imported(Err(message)) => {
                self.apply_action(
                    DesktopAction::Failed(format!(
                        "{} {message}",
                        crate::i18n::t("import failed:", "导入失败:")
                    )),
                    cx,
                );
            }
            BridgeReply::Recalled(Ok((conversation, edit))) => {
                let prefill = edit.clone();
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
                self.apply_action(DesktopAction::SessionDeleted, cx);
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
            }
            BridgeReply::ProviderKeySaved(Ok((provider_keys, mcp_keys))) => {
                // Refresh the key markers in place: reloading settings here
                // would race the concurrently running settings save and wipe
                // the just-added provider (and any other unsaved edits).
                self.apply_action(
                    DesktopAction::ProviderKeySaved {
                        provider_keys,
                        mcp_keys,
                    },
                    cx,
                );
            }
            BridgeReply::ChatStarted(Ok(())) => {}
            // The turn unwinds over the event channel; the reply itself
            // carries no state.
            BridgeReply::ChatCancelled(_) => {}
            BridgeReply::SubagentCancelled(Err(message)) => {
                self.push_toast(message, crate::workspace::ToastKind::Info, cx);
            }
            BridgeReply::SubagentCancelled(Ok(())) => {}
            BridgeReply::McpTools {
                server_id,
                outcome: Ok(tools),
            } => {
                self.apply_action(DesktopAction::McpToolsListed { server_id, tools }, cx);
            }
            BridgeReply::McpTools {
                server_id,
                outcome: Err(message),
            } => {
                self.apply_action(DesktopAction::McpProbeFailed { server_id, message }, cx);
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
                        crate::i18n::t("Models refreshed", "模型目录已刷新").to_owned(),
                        crate::workspace::ToastKind::Info,
                        cx,
                    );
                }
            }
            BridgeReply::Catalog(Err(message)) => {
                if self.pending_catalog_refresh {
                    self.pending_catalog_refresh = false;
                    self.apply_action(DesktopAction::Failed(message), cx);
                }
            }
            BridgeReply::UiState(Ok(mut ui_state)) => {
                // One-time upgrade: the legacy anonymous folder list becomes
                // the first named workspace, and every session that predates
                // workspaces belongs to it (unbound sessions resolve to the
                // first workspace on read).
                let migrated = ui_state.workspaces.is_empty();
                if migrated {
                    let mut folders = ui_state.workspace_roots.clone();
                    if folders.is_empty()
                        && let Some(last) = ui_state.last_project.clone()
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
                        session_projects: ui_state.session_projects,
                        workspaces: ui_state.workspaces,
                        session_workspaces: ui_state.session_workspaces,
                        trusted_projects: ui_state.trusted_projects,
                        active_workspace: ui_state.active_workspace,
                        recent_models: ui_state.recent_models,
                        starred_models: ui_state.starred_models,
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
                let version = offer.version.clone();
                self.apply_action(
                    DesktopAction::UpdateStateChanged(UpdateState::Available {
                        version: offer.version,
                        notes_url: offer.notes_url,
                    }),
                    cx,
                );
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
                let brief = mycode_app::brief_error(&message);
                self.apply_action(
                    DesktopAction::UpdateStateChanged(UpdateState::Failed(brief.clone())),
                    cx,
                );
                if self.take_manual_update_check() {
                    self.push_toast(brief, crate::workspace::ToastKind::Error, cx);
                }
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
            BridgeReply::Sessions(Err(message))
            | BridgeReply::Created(Err(message))
            | BridgeReply::Conversation(Err(message))
            | BridgeReply::Sent(Err(message))
            | BridgeReply::Settings(Err(message))
            | BridgeReply::ProviderKeySaved(Err(message))
            | BridgeReply::ChatStarted(Err(message))
            | BridgeReply::Recalled(Err(message))
            | BridgeReply::Resources(Err(message))
            | BridgeReply::AskAnswered(Err(message)) => {
                self.apply_action(DesktopAction::Failed(message), cx);
            }
        }
    }

    /// Starts one model turn over the active conversation using the picked
    /// provider/model, falling back to the first enabled provider.
    fn begin_chat_turn(&mut self, cx: &mut Context<Self>) {
        let Some(conversation) = self.vm.active.clone() else {
            self.apply_action(DesktopAction::Failed("no open session".to_owned()), cx);
            return;
        };
        let (Some(session), Some(branch)) = (
            SessionId::parse(&conversation.session_id),
            BranchId::parse(&conversation.branch_id),
        ) else {
            self.apply_action(
                DesktopAction::Failed("the open session could not be read".to_owned()),
                cx,
            );
            return;
        };
        let expected_head = crate::workspace::parse_head(&conversation.head);
        let Some(settings) = self.vm.settings.as_ref() else {
            self.apply_action(
                DesktopAction::Failed("settings are still loading".to_owned()),
                cx,
            );
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
                DesktopAction::Failed(
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
                DesktopAction::Failed("the provider has no models configured".to_owned()),
                cx,
            );
            return;
        };
        // The bridge rebuilds the turn history from the ledger's typed
        // events, so tool_use/tool_result pairing survives replay.
        self.dispatch(
            BridgeCommand::ChatTurn {
                session,
                branch,
                expected_head,
                provider_id: provider.id.clone(),
                model,
            },
            cx,
        );
    }

    pub(super) fn send(&mut self, draft: String, window: &mut Window, cx: &mut Context<Self>) {
        self.send_text(draft, true, Some(window), cx);
    }

    /// Enqueues a follow-up while a turn is running; the composer stays free
    /// for the next draft. The queue is capped so a stuck turn cannot grow
    /// without bound.
    pub(super) fn enqueue_follow_up(
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
    pub(super) fn pump_queued_send(&mut self, cx: &mut Context<Self>) {
        if self.vm.sending || self.vm.queued.is_empty() {
            return;
        }
        let draft = self.vm.queued[0].clone();
        self.apply_action(DesktopAction::QueuedMessageTaken, cx);
        self.send_text(draft, false, None, cx);
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
        let Some(conversation) = self.vm.active.clone() else {
            return;
        };
        let (Some(session), Some(branch)) = (
            SessionId::parse(&conversation.session_id),
            BranchId::parse(&conversation.branch_id),
        ) else {
            return;
        };
        let expected_head = crate::workspace::parse_head(&conversation.head);
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
