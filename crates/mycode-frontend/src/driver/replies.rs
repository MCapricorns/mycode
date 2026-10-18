//! Folding core replies into actions. Each reply already passed its
//! [`ReplyScope`] check in `poll`; the two scope-embedded kinds (a recall
//! page walk and a row delete) resolve here before the generic match.
use mycode_app::{BridgeCommand, BridgeReply};

use super::{Effect, FrontendDriver, ReplyScope, ToastKind};
use crate::view_model::{CHAT_CANCELLED, FrontendAction, SettingsState, UpdateState};

fn failed_start(message: String) -> FrontendAction {
    FrontendAction::ChatFailed {
        message,
        head: String::new(),
    }
}

impl FrontendDriver {
    /// Handles one scoped reply. `scope` was chosen when its command was
    /// dispatched, so a late answer cannot mutate a newer conversation.
    pub(crate) fn on_reply(
        &mut self,
        scope: ReplyScope,
        reply: BridgeReply,
        out: &mut Vec<Effect>,
    ) {
        match scope {
            ReplyScope::Recall(request) => {
                self.resolve_recall_reply(*request, reply, out);
                return;
            }
            ReplyScope::DeletedSession(session_id) => {
                self.finish_delete(&session_id, reply, out);
                return;
            }
            _ => {}
        }
        match reply {
            BridgeReply::Sessions(Ok(sessions)) => {
                self.act(FrontendAction::SessionsLoaded(sessions), out);
            }
            BridgeReply::Created(Ok(summary)) => {
                let session_id = summary.session_id.clone();
                self.act(FrontendAction::SessionCreated(summary), out);
                if self.vm.pending_welcome_send.is_some() {
                    self.welcome_send_session = Some(session_id.clone());
                }
                // An explicit open requested after the create wins: the late
                // reply must not steal the view from what the user clicked.
                let opening = self.pending_open.is_none();
                if opening {
                    self.request_open_session(&session_id);
                } else if self.welcome_send_session.as_deref() == Some(session_id.as_str()) {
                    self.welcome_send_session = None;
                    // Keep the held text as a draft instead of dropping it.
                    if let Some(text) = self.vm.pending_welcome_send.clone() {
                        self.pending_composer_prefill = Some(text.clone());
                        out.push(Effect::ComposerSet { text, focus: true });
                    }
                    self.act(FrontendAction::WelcomeSendConsumed, out);
                }
                if let Some(project) = self.pending_project.take() {
                    if opening {
                        self.attach_project(&session_id, &project, out);
                    } else {
                        // The session was still created for this folder; bind
                        // it without moving the view's project back.
                        self.act(
                            FrontendAction::SessionProjectBound {
                                session_id: session_id.clone(),
                                project,
                            },
                            out,
                        );
                        self.persist_ui_state();
                    }
                }
                self.dispatch(BridgeCommand::ListSessions);
            }
            BridgeReply::Conversation(Ok(conversation)) => {
                let session_id = conversation.session_id.clone();
                if self
                    .pending_open
                    .as_ref()
                    .is_some_and(|pending| pending != &session_id)
                {
                    return;
                }
                self.pending_open = None;
                // The snapshot was read before this reply landed. Events in
                // between were dropped (another chat was open) or applied to
                // the entries this snapshot now replaces.
                self.resync_on_turn_end = conversation.running.then(|| session_id.clone());
                let switching = self
                    .vm
                    .active
                    .as_ref()
                    .is_some_and(|active| active.session_id != conversation.session_id);
                self.act(FrontendAction::ConversationOpened(conversation), out);
                if switching {
                    self.pending_composer_prefill = Some(String::new());
                    out.push(Effect::ComposerSet {
                        text: String::new(),
                        focus: false,
                    });
                    self.mention_query = None;
                }
                self.follow_session_project(&session_id, out);
                self.dispatch(BridgeCommand::ListResources {
                    session_id: session_id.clone(),
                });
                self.refresh_skills(out);
                self.flush_welcome_send(&session_id, out);
            }
            BridgeReply::Older(Ok(page)) => {
                let applicable = self.vm.active.as_ref().is_some_and(|active| {
                    active.session_id == page.session_id
                        && active.older_before.as_deref() == Some(page.requested_before.as_str())
                });
                let pinned = applicable && self.conversation_at_tail;
                if applicable {
                    self.scroll_hold_pending = !pinned;
                }
                self.act(
                    FrontendAction::OlderLoaded {
                        session_id: page.session_id,
                        entries: page.entries,
                        older_before: page.older,
                        requested_before: page.requested_before,
                    },
                    out,
                );
                if applicable {
                    out.push(Effect::OlderPagePrepended { pinned });
                }
            }
            BridgeReply::Older(Err(message)) => {
                self.act(FrontendAction::HistoryLoadFinished, out);
                self.act(FrontendAction::Failed(message), out);
            }
            BridgeReply::Resources(Ok(files)) => {
                self.act(FrontendAction::ResourcesLoaded(files), out);
            }
            BridgeReply::ProjectFiles(Ok(files)) => {
                self.act(FrontendAction::MentionFiles(files), out);
            }
            // A failed mention search just leaves the menu empty.
            BridgeReply::ProjectFiles(Err(_)) => {}
            BridgeReply::CopilotSignInStarted(Ok(info)) => {
                self.act(
                    FrontendAction::CopilotSignInStarted(crate::view_model::CopilotSignIn {
                        provider_id: info.provider_id,
                        user_code: info.user_code,
                        verification_uri: info.verification_uri,
                    }),
                    out,
                );
            }
            BridgeReply::CopilotSignInStarted(Err(message)) => {
                self.act(FrontendAction::CopilotSignInFinished(Err(message)), out);
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
                self.act(FrontendAction::MessageSent { head, entry }, out);
                self.begin_chat_turn(out);
            }
            BridgeReply::Settings(Ok(loaded)) => {
                self.note_config_repairs(&loaded.repairs);
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
                    out.push(Effect::AppearanceTheme(state.theme.clone()));
                    out.push(Effect::SyncProxyField);
                }
                self.act(FrontendAction::SettingsLoaded(Box::new(state)), out);
            }
            BridgeReply::Exported(Ok(_summary)) => {}
            BridgeReply::Exported(Err(message)) => {
                self.act(
                    FrontendAction::Failed(format!(
                        "{} {message}",
                        crate::i18n::t("export failed:", "导出失败：")
                    )),
                    out,
                );
            }
            BridgeReply::Imported(Err(message))
                if message == mycode_app::IMPORT_NEEDS_MCP_CONFIRM =>
            {
                // Not a failure: the Data card asks before MCP commands that
                // start programs on this computer are added or replaced.
                self.import_confirm = self.import_request.take();
            }
            BridgeReply::Imported(Ok(summary)) => {
                self.import_request = None;
                // Reload everything the bundle replaced: settings, key
                // markers, and UI state. Sessions were not touched.
                self.dispatch(BridgeCommand::LoadSettings);
                self.dispatch(BridgeCommand::LoadUiState);
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
                self.toast(message, ToastKind::Info, out);
            }
            BridgeReply::Imported(Err(message)) => {
                self.import_request = None;
                self.act(
                    FrontendAction::Failed(format!(
                        "{} {message}",
                        crate::i18n::t("import failed:", "导入失败：")
                    )),
                    out,
                );
            }
            BridgeReply::Recalled(Ok((conversation, edit))) => {
                let session_id = conversation.session_id.clone();
                self.act(
                    FrontendAction::ConversationOpened((*conversation).clone()),
                    out,
                );
                self.follow_session_project(&session_id, out);
                if let Some(text) = edit {
                    self.pending_composer_prefill = Some(text.clone());
                    out.push(Effect::ComposerSet { text, focus: true });
                    // The rewind wipes the transcript tail; say where the text went.
                    self.toast(
                        crate::i18n::t(
                            "Message recalled — its text is back in the composer.",
                            "消息已撤回，原文已放回输入框。",
                        )
                        .to_owned(),
                        ToastKind::Info,
                        out,
                    );
                }
            }
            BridgeReply::SessionDeleted(Ok(())) => {
                // The row delete handler updates only its target session.
                self.dispatch(BridgeCommand::ListSessions);
            }
            BridgeReply::SettingsSaved(Ok(revision)) => {
                self.act(
                    FrontendAction::SettingsSaved {
                        revision: revision.get(),
                        edit_epoch: self.settings_save_epoch,
                    },
                    out,
                );
                // An edit that landed while this write was in flight stays
                // dirty. Persist it unless a text-field debounce is already
                // waiting to write the latest draft.
                self.continue_settings_save(out);
            }
            BridgeReply::ProviderKeySaved(Ok((provider_keys, mcp_keys))) => {
                // Refresh the key markers in place: reloading settings here
                // would race the concurrently running settings save and wipe
                // the just-added provider (and any other unsaved edits).
                let stored = mcp_keys.clone();
                self.act(
                    FrontendAction::ProviderKeySaved {
                        provider_keys,
                        mcp_keys,
                    },
                    out,
                );
                // An enable that waited on this key save may probe now.
                for server_id in stored {
                    if self.pending_mcp_enable.contains(&server_id)
                        && !self.vm.mcp_probing.contains(&server_id)
                    {
                        self.probe_mcp_for_enable(&server_id, out);
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
                    self.act(
                        FrontendAction::ChatFailed {
                            message: CHAT_CANCELLED.to_owned(),
                            head: String::new(),
                        },
                        out,
                    );
                    self.pump_queued_send(out);
                }
            }
            BridgeReply::Steered(_) => {}
            BridgeReply::SubagentCancelled(Err(message)) => {
                self.toast(message, ToastKind::Info, out);
            }
            BridgeReply::SubagentCancelled(Ok(())) => {}
            BridgeReply::McpTools {
                server_id,
                outcome: Ok(tools),
            } => {
                self.act(
                    FrontendAction::McpToolsListed {
                        server_id: server_id.clone(),
                        tools,
                    },
                    out,
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
                    self.act(FrontendAction::SettingsMcpToggled(index, true), out);
                }
            }
            BridgeReply::McpTools {
                server_id,
                outcome: Err(message),
            } => {
                self.pending_mcp_enable.remove(&server_id);
                self.act(FrontendAction::McpProbeFailed { server_id, message }, out);
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
                                self.act(FrontendAction::SettingsBackendToggled(index, true), out);
                                self.toast(
                                    crate::i18n::t(
                                        "Search backend verified and enabled",
                                        "搜索后端已验证并启用",
                                    )
                                    .to_owned(),
                                    ToastKind::Info,
                                    out,
                                );
                            }
                        }
                        Err(message) => {
                            self.toast(message, ToastKind::Error, out);
                        }
                    }
                }
            }
            BridgeReply::Catalog(Ok(info)) => {
                let refreshed = self.pending_catalog_refresh;
                self.pending_catalog_refresh = false;
                self.act(
                    FrontendAction::CatalogLoaded {
                        document: info.document,
                        fetched_at: info.fetched_at,
                    },
                    out,
                );
                if refreshed {
                    self.toast(
                        crate::i18n::t("Provider catalog refreshed", "服务商目录已刷新").to_owned(),
                        ToastKind::Info,
                        out,
                    );
                }
            }
            BridgeReply::Catalog(Err(message)) => {
                // A failed lazy load clears the flag so the next surface
                // open retries; a failed manual refresh also toasts.
                self.catalog_requested = false;
                if self.pending_catalog_refresh {
                    self.pending_catalog_refresh = false;
                    self.act(FrontendAction::Failed(message), out);
                }
            }
            BridgeReply::UiState(Ok((ui_state, repairs))) => {
                self.ui_state_loaded = true;
                self.note_config_repairs(&repairs);
                self.act(
                    FrontendAction::UiStateLoaded {
                        recents: ui_state.recent_projects,
                        last_project: ui_state.last_project,
                        auto_update: ui_state.auto_update,
                        selected_provider: ui_state.selected_provider,
                        selected_model: ui_state.selected_model,
                        selected_reasoning: ui_state.selected_reasoning,
                        session_projects: ui_state.session_projects,
                        session_models: ui_state.session_models,
                        workspace_roots: ui_state.workspace_roots,
                        recent_models: ui_state.recent_models,
                        thinking_collapsed: ui_state.thinking_collapsed,
                        inspector_open: ui_state.inspector_open,
                        inspector_pinned: ui_state.inspector_pinned,
                        window_size: ui_state.window_size,
                    },
                    out,
                );
                self.restore_session_projects();
                self.refresh_skills(out);
            }
            BridgeReply::UiState(Err(_)) => self.ui_state_loaded = true,
            BridgeReply::UiStateSaved(Ok(())) => {}
            BridgeReply::UiStateSaved(Err(message)) => {
                self.act(FrontendAction::Failed(message), out);
            }
            BridgeReply::ProjectSet(Ok(())) => {}
            BridgeReply::ProjectSet(Err(message)) => {
                self.act(FrontendAction::Failed(message), out);
            }
            BridgeReply::ProxyTested(outcome) => {
                // An edit while the test ran reset the state; that answer
                // was for a value no longer on screen.
                if self.vm.proxy_test == crate::view_model::ProxyTestState::Testing {
                    let test = match outcome {
                        Ok(report) => crate::view_model::ProxyTestState::Done(report),
                        Err(message) => crate::view_model::ProxyTestState::Invalid(message),
                    };
                    self.act(FrontendAction::ProxyTestChanged(test), out);
                }
            }
            BridgeReply::UpdateChecked(Ok(None)) => {
                if matches!(
                    self.vm.update,
                    UpdateState::Downloading { .. } | UpdateState::Ready { .. }
                ) {
                    let _ = self.take_manual_update_check();
                    return;
                }
                self.act(
                    FrontendAction::UpdateStateChanged(UpdateState::UpToDate),
                    out,
                );
                if self.take_manual_update_check() {
                    self.toast(
                        crate::i18n::t("You're up to date", "已是最新版本").to_owned(),
                        ToastKind::Info,
                        out,
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
                self.act(FrontendAction::UpdateOfferFound(offer), out);
                if self.take_manual_update_check() {
                    self.toast(
                        format!("v{version} {}", crate::i18n::t("is available", "可用")),
                        ToastKind::Info,
                        out,
                    );
                }
                // A manual check downloads too; the dialog prompts install.
                self.download_update(out);
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
                self.act(
                    FrontendAction::UpdateStateChanged(UpdateState::Failed(brief.clone())),
                    out,
                );
                // Manual and automatic checks both toast. The About row does
                // not render `brief`.
                let _ = self.take_manual_update_check();
                self.toast(brief, ToastKind::Error, out);
            }
            BridgeReply::UpdateDownloaded(Ok(prepared)) => {
                let version = self
                    .vm
                    .last_offer
                    .as_ref()
                    .map(|offer| offer.version.clone());
                self.act(FrontendAction::UpdateStaged(prepared), out);
                let message = match version {
                    Some(version) => format!(
                        "v{version} {}",
                        crate::i18n::t("is ready to install", "已就绪，可安装")
                    ),
                    None => crate::i18n::t("Update is ready to install", "更新已就绪，可安装")
                        .to_owned(),
                };
                self.toast(message, ToastKind::Info, out);
                // Downloading runs on its own; installing never does.
                self.act(FrontendAction::UpdateDialogToggled(true), out);
            }
            BridgeReply::UpdateDownloaded(Err(message)) => {
                let brief = mycode_app::brief_error(&message);
                self.act(
                    FrontendAction::UpdateStateChanged(UpdateState::Failed(brief.clone())),
                    out,
                );
                self.toast(brief, ToastKind::Error, out);
                self.act(FrontendAction::UpdateDialogToggled(true), out);
            }
            BridgeReply::SettingsSaved(Err(message)) => {
                self.act(FrontendAction::SettingsSaveFailed(message), out);
            }
            BridgeReply::SessionDeleted(Err(message)) => {
                // The sidebar row was dropped optimistically. Put the session
                // back from disk when the delete did not actually happen.
                self.act(FrontendAction::Failed(message), out);
                self.dispatch(BridgeCommand::ListSessions);
                self.dispatch(BridgeCommand::LoadUiState);
            }
            BridgeReply::Created(Err(message)) => {
                self.welcome_send_session = None;
                self.act(FrontendAction::WelcomeSendConsumed, out);
                self.act(FrontendAction::Failed(message), out);
            }
            BridgeReply::Conversation(Err(message)) => {
                // A failed open still ends the wait: a stale pending_open
                // would scope-drop the next explicit open's reply.
                self.pending_open = None;
                if self.welcome_send_session.is_some() {
                    self.welcome_send_session = None;
                    self.act(FrontendAction::WelcomeSendConsumed, out);
                }
                self.act(FrontendAction::Failed(message), out);
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
                    self.act(failed_start(message), out);
                }
            }
            BridgeReply::ChatStarted(Err(message)) => {
                self.act(failed_start(message), out);
            }
            BridgeReply::ProviderKeySaved(Err(message)) => {
                // Pending enables that waited on this key save can never
                // probe now; entries already probing keep their gate.
                let probing = &self.vm.mcp_probing;
                self.pending_mcp_enable.retain(|id| probing.contains(id));
                self.act(FrontendAction::Failed(message), out);
            }
            BridgeReply::Sessions(Err(message))
            | BridgeReply::Settings(Err(message))
            | BridgeReply::Recalled(Err(message))
            | BridgeReply::Resources(Err(message))
            | BridgeReply::AskAnswered(Err(message)) => {
                self.act(FrontendAction::Failed(message), out);
            }
            // The first-frame acknowledgement carries no UI state; a lost
            // reply only means the deferred refresh ran anyway.
            BridgeReply::UiReady(_) => {}
            // Title generation only acknowledges acceptance; the title
            // itself arrives as `BridgeEvent::SessionTitle`.
            BridgeReply::TitleSummarized(_) => {}
        }
    }

    /// A row delete reply: the core confirmed the session is gone, so the
    /// bindings and — when it was open — the conversation can be forgotten.
    fn finish_delete(&mut self, session_id: &str, reply: BridgeReply, out: &mut Vec<Effect>) {
        match reply {
            BridgeReply::SessionDeleted(Ok(())) => {
                if self
                    .vm
                    .active
                    .as_ref()
                    .is_some_and(|active| active.session_id == session_id)
                {
                    self.act(FrontendAction::SessionDeleted, out);
                }
                // Forget bindings only after the core confirms deletion;
                // failure must leave both the transcript and routing intact.
                self.act(
                    FrontendAction::SessionBindingsForgotten(session_id.to_owned()),
                    out,
                );
                self.persist_ui_state();
                self.dispatch(BridgeCommand::ListSessions);
            }
            BridgeReply::SessionDeleted(Err(message)) => {
                self.act(FrontendAction::Failed(message), out);
            }
            _ => {}
        }
    }
}
