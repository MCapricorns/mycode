//! Reducer internals: the pure transition functions behind
//! [`super::reduce`]. Only this tree mutates [`WorkspaceState`]; the parent
//! module declares the state, action, and projection types. The dispatch
//! match lives here; arm bodies with their own logic live in the topical
//! submodules.
mod composer;
mod jobs;
mod models;
mod projects;
mod streaming;
mod usage;

use mycode_app::CHAT_CANCELLED;

use super::{
    ActiveConversation, DesktopAction, MentionKind, SettingsState, TRANSCRIPT_PAGE, TurnStats,
    UpdateState, UsageTotal, WorkspaceState,
};

pub(crate) use self::models::{
    reasoning_levels_for, selected_model_supports_reasoning, selected_reasoning_levels,
};

use self::composer::parse_mention;
use self::jobs::{finish_live_job, tool_progress, tool_started};
use self::models::{
    active_preset_changed, ensure_model_selection, model_selected, provider_selected,
};
use self::projects::{
    bind_session_project, session_bindings_forgotten, session_project_bound,
    session_workspace_bound, sync_workspace_roots, workspace_created, workspace_removed,
    workspace_renamed, workspace_root_added, workspace_root_removed, workspace_switched,
};
use self::streaming::{append_streaming, set_streaming_status};
use self::usage::rebuild_session_usage;

/// Applies one action to the state.
pub(crate) fn reduce(state: &mut WorkspaceState, action: DesktopAction) {
    let touches_providers = matches!(
        action,
        DesktopAction::SettingsLoaded(_)
            | DesktopAction::SettingsProviderAdded(_)
            | DesktopAction::SettingsProviderRemoved(_)
            | DesktopAction::SettingsProviderToggled(_, _)
            | DesktopAction::SettingsSaved { .. }
            | DesktopAction::UiStateLoaded { .. }
    );
    match action {
        DesktopAction::SessionsLoaded(mut sessions) => {
            let active_id = state.active.as_ref().map(|c| c.session_id.as_str());
            for session in &mut sessions {
                session.active = active_id == Some(session.session_id.as_str());
            }
            state.sessions = sessions;
        }
        DesktopAction::SessionCreated(mut summary) => {
            state
                .sessions
                .retain(|s| s.session_id != summary.session_id);
            summary.active = true;
            state.sessions.insert(0, summary.clone());
            state.active = Some(ActiveConversation {
                session_id: summary.session_id,
                branch_id: summary.root_branch_id,
                head: "empty".to_owned(),
                entries: Vec::new(),
                streaming: None,
            });
            state.live_jobs.clear();
            state.subagent_window = None;
            state.changes_panel_open = false;
            state.transcript_extra = 0;
        }
        DesktopAction::SessionDeleted => {
            state.active = None;
            state.sending = false;
            state.queued.clear();
            state.live_jobs.clear();
            state.subagent_window = None;
            state.changes_panel_open = false;
            state.pending_ask = None;
            state.error = None;
            state.transcript_extra = 0;
        }
        DesktopAction::ConversationParked => {
            state.active = None;
            for session in &mut state.sessions {
                session.active = false;
            }
            state.sending = false;
            state.queued.clear();
            state.live_jobs.clear();
            state.subagent_window = None;
            state.changes_panel_open = false;
            state.pending_ask = None;
            state.transcript_extra = 0;
            state.composer_draft.clear();
            state.mention = None;
            state.resources.clear();
            state.live_turn = None;
        }
        DesktopAction::ConversationOpened(conversation) => {
            let had_other = state
                .active
                .as_ref()
                .is_some_and(|active| active.session_id != conversation.session_id);
            let switched = state
                .active
                .as_ref()
                .map(|active| active.session_id.as_str())
                != Some(conversation.session_id.as_str());
            if switched {
                // Queue, tasks, and asks belong to the previous session.
                state.queued.clear();
                state.live_jobs.clear();
                state.subagent_window = None;
                state.changes_panel_open = false;
                state.pending_ask = None;
                state.sending = false;
                state.transcript_extra = 0;
            }
            let session_id = conversation.session_id.clone();
            state.active = Some(conversation);
            for session in &mut state.sessions {
                session.active = session.session_id == session_id;
            }
            if switched {
                rebuild_session_usage(state);
                state.live_turn = None;
            }
            // The composer is one widget. A draft typed in the previous
            // session must not ride along and send into this one. Opening
            // the first session keeps a welcome-screen draft.
            if had_other {
                state.composer_draft.clear();
                state.mention = None;
            }
        }
        DesktopAction::ComposerChanged(text) => {
            let bounded: String = text.chars().take(super::MAX_COMPOSER_CHARS).collect();
            state.mention = parse_mention(&bounded);
            state.composer_draft = bounded;
        }
        DesktopAction::MentionFiles(files) => {
            if let Some(mention) = state.mention.as_mut()
                && mention.kind == MentionKind::File
            {
                mention.items = files
                    .into_iter()
                    .map(|path| {
                        let display = path.clone();
                        (path, display)
                    })
                    .collect();
            }
        }
        DesktopAction::CopilotSignInStarted(info) => {
            state.copilot_sign_in = Some(info);
            state.copilot_error = None;
        }
        DesktopAction::CopilotSignInFinished(outcome) => {
            state.copilot_sign_in = None;
            state.copilot_error = outcome.err();
        }
        DesktopAction::MessageQueued(text) => {
            let bounded: String = text.chars().take(super::MAX_COMPOSER_CHARS).collect();
            if !bounded.trim().is_empty() && state.queued.len() < super::MAX_QUEUED_MESSAGES {
                state.queued.push(bounded);
            }
            state.composer_draft.clear();
            state.mention = None;
        }
        DesktopAction::QueuedMessageRemoved(index) => {
            if index < state.queued.len() {
                state.queued.remove(index);
            }
        }
        DesktopAction::QueuedMessageTaken => {
            if !state.queued.is_empty() {
                state.queued.remove(0);
            }
        }
        DesktopAction::QueuedMessagePromoted(index) => {
            if index < state.queued.len() {
                let item = state.queued.remove(index);
                state.queued.insert(0, item);
            }
        }
        DesktopAction::SubagentWindowChanged(call_id) => {
            state.subagent_window = call_id;
        }
        DesktopAction::ChangesPanelToggled(open) => {
            state.changes_panel_open = open;
        }
        DesktopAction::SubagentDismissed(call_id) => {
            jobs::drop_live_job(state, &call_id);
        }
        DesktopAction::MessageSent { head, entry } => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.head = head;
                conversation.entries.push(entry);
            }
            state.composer_draft.clear();
            state.mention = None;
        }
        DesktopAction::TurnArmed => {
            state.sending = true;
            state.error = None;
            set_streaming_status(
                state,
                crate::i18n::t("Waiting for the model", "等待模型响应"),
            );
        }
        DesktopAction::ChatDelta(delta) => {
            append_streaming(state, false, delta);
        }
        DesktopAction::ToolStarted {
            call_id,
            name,
            target,
        } => tool_started(state, call_id, name, target),
        DesktopAction::ToolProgress {
            call_id,
            name,
            message,
        } => tool_progress(state, call_id, name, message),
        DesktopAction::ToolResultAppended(entry) => {
            if let Some(call_id) = entry.call_id.as_deref() {
                finish_live_job(state, call_id);
            }
            if let Some(conversation) = state.active.as_mut() {
                conversation.entries.push(entry);
            }
        }
        DesktopAction::ResourcesLoaded(files) => state.resources = files,
        DesktopAction::SkillsLoaded(skills) => state.skills = skills,
        DesktopAction::UsageSnapshot {
            model,
            input,
            output,
            cache,
            elapsed_ms,
        } => {
            state.live_turn = Some(TurnStats {
                model,
                input,
                output,
                cache,
                elapsed_ms,
            });
        }
        DesktopAction::UsageRecorded {
            provider,
            model,
            input,
            output,
            cache,
            elapsed_ms,
            entry,
        } => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.entries.push(entry);
            }
            state.last_turn = Some(TurnStats {
                model: model.clone(),
                input,
                output,
                cache,
                elapsed_ms,
            });
            let key = format!("{provider}/{model}");
            // Merge with a rebuilt bare-model row from an earlier replay so
            // the visible totals keep updating instead of freezing behind
            // a stale first row.
            let row =
                match state.usage_totals.iter_mut().find(|row| {
                    row.key == key || crate::view_model::usage_key_matches(&row.key, &key)
                }) {
                    Some(row) => row,
                    None => {
                        state.usage_totals.push(UsageTotal {
                            key,
                            ..UsageTotal::default()
                        });
                        state.usage_totals.last_mut().expect("just pushed")
                    }
                };
            row.input = row.input.saturating_add(input);
            row.output = row.output.saturating_add(output);
            row.cache = row.cache.saturating_add(cache.unwrap_or_default());
            row.requests = row.requests.saturating_add(1);
            state.live_turn = None;
        }
        DesktopAction::AskRequested(rows) => {
            state.ask_answers = vec![String::new(); rows.len()];
            state.pending_ask = Some(rows);
        }
        DesktopAction::AskChoicePicked { index, answer } => {
            if let Some(slot) = state.ask_answers.get_mut(index) {
                *slot = answer;
            }
        }
        DesktopAction::AskAnswered => {
            state.pending_ask = None;
            state.ask_answers.clear();
        }
        DesktopAction::TranscriptRevealMore => {
            state.transcript_extra = state.transcript_extra.saturating_add(TRANSCRIPT_PAGE);
        }
        DesktopAction::ChatThinkingDelta(delta) => {
            append_streaming(state, true, delta);
        }
        DesktopAction::AssistantStepCommitted(entry) => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.entries.push(entry);
            }
            set_streaming_status(
                state,
                crate::i18n::t("Waiting for the next step", "等待下一步"),
            );
            if let Some(conversation) = state.active.as_mut()
                && let Some(streaming) = conversation.streaming.as_mut()
            {
                streaming.text.clear();
                streaming.thinking.clear();
            }
        }
        DesktopAction::ChatDone { head, entry } => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.head = head;
                // A turn that ended on a committed tool step reports that
                // step again as its last message; it is already listed.
                let already_listed = conversation
                    .entries
                    .iter()
                    .any(|existing| existing.event_id == entry.event_id);
                if !already_listed {
                    conversation.entries.push(entry);
                }
                conversation.streaming = None;
            }
            state.live_jobs.clear();
            state.live_turn = None;
            state.sending = false;
        }
        DesktopAction::ChatFailed(message) => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.streaming = None;
            }
            // A user-initiated cancel resets the turn without an error
            // banner; the sentinel travels as the failure message.
            if message != CHAT_CANCELLED {
                state.error = Some(message);
            }
            state.live_jobs.clear();
            state.live_turn = None;
            state.sending = false;
        }
        DesktopAction::Failed(message) => {
            state.error = Some(message);
            state.sending = false;
        }
        DesktopAction::SettingsLoaded(settings) => {
            match state.settings.as_mut() {
                Some(current) if current.dirty || current.saving => {
                    // Key badges live outside the settings document. Refresh
                    // them even while a save is in flight. New provider rows
                    // are adopted only when nothing is saving: merging them
                    // into the snapshot already on the wire would mark the
                    // editor clean while memory and disk disagree.
                    current.providers_with_keys = settings.providers_with_keys;
                    current.mcp_with_keys = settings.mcp_with_keys;
                    if !current.saving {
                        current.revision = settings.revision;
                        for provider in settings.providers {
                            if current.providers.len() >= mycode_config::MAX_PROVIDERS {
                                break;
                            }
                            if !current
                                .providers
                                .iter()
                                .any(|existing| existing.id == provider.id)
                            {
                                current.providers.push(provider);
                                mark_settings_dirty(current);
                            }
                        }
                    }
                }
                _ => {
                    let dark = settings.theme != "light";
                    crate::i18n::apply_language(&settings.language);
                    state.settings = Some(settings);
                    state.dark_theme = dark;
                }
            }
        }
        DesktopAction::SettingsThemeSelected(dark) => {
            if let Some(settings) = state.settings.as_mut() {
                settings.theme = if dark { "dark" } else { "light" }.to_owned();
                mark_settings_dirty(settings);
            }
            state.dark_theme = dark;
        }
        DesktopAction::SettingsLanguageSelected(language) => {
            if !mycode_config::VALID_LANGUAGES.contains(&language.as_str()) {
                return;
            }
            crate::i18n::apply_language(&language);
            if let Some(settings) = state.settings.as_mut() {
                settings.language = language;
                mark_settings_dirty(settings);
            }
        }
        DesktopAction::SettingsPaletteSelected(palette) => {
            let palette = crate::ui::desk::normalize_palette(&palette).to_owned();
            if let Some(settings) = state.settings.as_mut() {
                settings.palette = palette;
                mark_settings_dirty(settings);
            }
        }
        DesktopAction::SettingsUserAgentChanged(user_agent) => {
            edit_settings(state, |settings| {
                settings.user_agent = user_agent;
                true
            });
        }
        DesktopAction::SettingsProviderToggled(index, enabled) => {
            edit_settings(state, |settings| {
                settings.providers.get_mut(index).is_some_and(|provider| {
                    provider.enabled = enabled;
                    true
                })
            });
        }
        DesktopAction::SettingsProviderAdded(provider) => {
            edit_settings(state, |settings| {
                if settings.providers.len() < mycode_config::MAX_PROVIDERS {
                    settings.providers.push(provider);
                    true
                } else {
                    false
                }
            });
        }
        DesktopAction::SettingsProviderRemoved(index) => {
            edit_settings(state, |settings| {
                if index < settings.providers.len() {
                    settings.providers.remove(index);
                    true
                } else {
                    false
                }
            });
        }
        DesktopAction::SettingsBackendAdded(backend) => {
            edit_settings(state, |settings| {
                if settings.web_backends.len() < mycode_config::MAX_WEB_BACKENDS {
                    settings.web_backends.push(backend);
                    true
                } else {
                    false
                }
            });
        }
        DesktopAction::SettingsBackendRemoved(index) => {
            edit_settings(state, |settings| {
                if index < settings.web_backends.len() {
                    settings.web_backends.remove(index);
                    true
                } else {
                    false
                }
            });
        }
        DesktopAction::SettingsUsageToggled(enabled) => {
            edit_settings(state, |settings| {
                settings.usage_enabled = enabled;
                true
            });
        }
        DesktopAction::SettingsBackendToggled(index, enabled) => {
            edit_settings(state, |settings| {
                if index >= settings.web_backends.len() {
                    return false;
                }
                if enabled {
                    for (slot, backend) in settings.web_backends.iter_mut().enumerate() {
                        backend.enabled = slot == index;
                    }
                } else {
                    settings.web_backends[index].enabled = false;
                }
                true
            });
        }
        DesktopAction::SettingsSubagentsChanged(subagents) => {
            edit_settings(state, |settings| {
                settings.subagents = subagents;
                true
            });
        }
        DesktopAction::SettingsToolsChanged(tools) => {
            edit_settings(state, |settings| {
                settings.tools = tools;
                true
            });
        }
        DesktopAction::SubagentMenuToggled(menu) => state.subagent_menu = menu,
        DesktopAction::SettingsSaved {
            revision,
            edit_epoch,
        } => {
            if let Some(settings) = state.settings.as_mut() {
                settings.revision = revision;
                settings.saving = false;
                if settings.edit_epoch == edit_epoch {
                    settings.dirty = false;
                }
                settings.effective_user_agent = settings.to_settings().effective_user_agent();
            }
        }
        DesktopAction::SettingsSaveFailed(message) => {
            if let Some(settings) = state.settings.as_mut() {
                settings.saving = false;
            }
            state.error = Some(message);
        }
        DesktopAction::ProviderKeySaved {
            provider_keys,
            mcp_keys,
        } => {
            if let Some(settings) = state.settings.as_mut() {
                settings.providers_with_keys = provider_keys;
                settings.mcp_with_keys = mcp_keys;
            }
        }
        DesktopAction::SettingsMcpAdded(server) => {
            edit_settings(state, |settings| {
                if settings.mcp_servers.len() < mycode_config::MAX_MCP_SERVERS {
                    settings.mcp_servers.push(server);
                    true
                } else {
                    false
                }
            });
        }
        DesktopAction::SettingsMcpRemoved(index) => {
            if let Some(settings) = state.settings.as_mut()
                && index < settings.mcp_servers.len()
            {
                let removed = settings.mcp_servers.remove(index);
                mark_settings_dirty(settings);
                // A stale listing for a deleted row must not resurface if a
                // server with the same id is added again later.
                state.mcp_tools.retain(|(id, _)| *id != removed.id);
                state.mcp_probing.retain(|id| *id != removed.id);
            }
        }
        DesktopAction::SettingsMcpToggled(index, enabled) => {
            edit_settings(state, |settings| {
                settings.mcp_servers.get_mut(index).is_some_and(|server| {
                    server.enabled = enabled;
                    true
                })
            });
        }
        DesktopAction::McpProbeStarted(server_id) => {
            if !state.mcp_probing.contains(&server_id) {
                state.mcp_probing.push(server_id);
            }
        }
        DesktopAction::McpToolsListed { server_id, tools } => {
            state.mcp_probing.retain(|id| *id != server_id);
            if let Some(entry) = state.mcp_tools.iter_mut().find(|(id, _)| *id == server_id) {
                entry.1 = tools;
            } else {
                state.mcp_tools.push((server_id, tools));
            }
        }
        DesktopAction::McpProbeFailed { server_id, message } => {
            state.mcp_probing.retain(|id| *id != server_id);
            state.mcp_tools.retain(|(id, _)| *id != server_id);
            state.error = Some(message);
        }
        DesktopAction::ShowMainView(view) => {
            state.view = view;
            // Crossing views closes every floating menu so no stale layer
            // renders above the destination view.
            close_floating_menus(state);
        }
        DesktopAction::ShowSettingsSection(section) => state.settings_section = section,
        DesktopAction::ProjectMenuToggled(open) => state.project_menu_open = open,
        DesktopAction::CatalogLoaded {
            document,
            fetched_at,
        } => {
            state.catalog = Some(document);
            state.catalog_fetched_at = fetched_at;
        }
        DesktopAction::UiStateLoaded {
            recents,
            last_project,
            auto_update,
            selected_provider,
            selected_model,
            session_projects,
            workspaces,
            session_workspaces,
            active_workspace: active,
        } => {
            state.recents = recents;
            state.project_dir = last_project.filter(|path| !path.trim().is_empty());
            state.session_projects = session_projects;
            state.workspaces = workspaces;
            state.session_workspaces = session_workspaces;
            state.active_workspace = active;
            sync_workspace_roots(state);
            state.auto_update = auto_update;
            if selected_provider.is_some() {
                state.selected_provider = selected_provider;
                state.selected_model = selected_model;
            }
            ensure_model_selection(state);
        }
        DesktopAction::WorkspaceMenuToggled(open) => {
            state.workspace_menu_open = open;
            if !open {
                state.workspace_rename_open = false;
            }
        }
        DesktopAction::WorkspaceRenameToggled(open) => state.workspace_rename_open = open,
        DesktopAction::WorkspaceCreated(workspace) => workspace_created(state, workspace),
        DesktopAction::WorkspaceSwitched(id) => workspace_switched(state, id),
        DesktopAction::WorkspaceRenamed(name) => workspace_renamed(state, name),
        DesktopAction::WorkspaceRemoved(id) => workspace_removed(state, id),
        DesktopAction::SessionWorkspaceBound {
            session_id,
            workspace_id,
        } => session_workspace_bound(state, session_id, workspace_id),
        DesktopAction::SessionBindingsForgotten(session_id) => {
            session_bindings_forgotten(state, session_id)
        }
        DesktopAction::WorkspaceRootAdded(project) => workspace_root_added(state, project),
        DesktopAction::WorkspaceRootRemoved(project) => {
            workspace_root_removed(state, project);
        }
        DesktopAction::ProjectOpened(project) => {
            state.project_dir = Some(project.clone());
            state.recents.retain(|existing| existing != &project);
            state.recents.insert(0, project);
            state.recents.truncate(mycode_config::MAX_RECENT_PROJECTS);
        }
        DesktopAction::SessionProjectBound {
            session_id,
            project,
        } => session_project_bound(state, session_id, project),
        DesktopAction::WorkspaceFolderFocused {
            session_id,
            project,
        } => {
            if project.trim().is_empty() {
                return;
            }
            state.project_dir = Some(project.clone());
            bind_session_project(state, session_id, project);
        }
        DesktopAction::ActiveProjectChanged(project) => {
            state.project_dir = project;
            if !super::task_surface_visible(state) {
                state.subagent_window = None;
            }
        }
        DesktopAction::RecentRemoved(project) => {
            state
                .recents
                .retain(|existing| !super::same_project_path(existing, &project));
            if state
                .project_dir
                .as_ref()
                .is_some_and(|current| super::same_project_path(current, &project))
            {
                state.project_dir = None;
            }
        }
        DesktopAction::ProviderSelected(provider) => provider_selected(state, provider),
        DesktopAction::ModelSelected(model) => model_selected(state, model),
        DesktopAction::ModelMenuToggled(open) => {
            state.model_menu_open = open;
            if open {
                state.reasoning_menu_open = false;
                if state.model_menu_browse.is_none() {
                    state.model_menu_browse = state.selected_provider.clone();
                }
            } else {
                state.model_menu_browse = None;
            }
        }
        DesktopAction::ModelMenuBrowse(provider) => {
            state.model_menu_open = true;
            state.reasoning_menu_open = false;
            state.model_menu_browse = Some(provider);
        }
        DesktopAction::ReasoningMenuToggled(open) => {
            state.reasoning_menu_open = open;
            if open {
                state.model_menu_open = false;
            }
        }
        DesktopAction::SettingsReasoningChanged(level) => {
            state.reasoning_menu_open = false;
            if state
                .settings
                .as_ref()
                .is_none_or(|settings| settings.saving)
            {
                return;
            }
            let picked: Option<String> = if level == "default" || level.is_empty() {
                None
            } else if selected_reasoning_levels(state).contains(&level) {
                Some(level.clone())
            } else {
                // Levels the catalog does not advertise are refused.
                return;
            };
            edit_settings(state, |settings| {
                settings.reasoning = picked;
                true
            });
        }
        DesktopAction::PresetSearchChanged(text) => state.preset_search = text,
        DesktopAction::ActivePresetChanged(preset) => active_preset_changed(state, preset),
        DesktopAction::PresetModelToggled(model) => {
            if let Some(position) = state.preset_models.iter().position(|m| *m == model) {
                state.preset_models.remove(position);
            } else {
                state.preset_models.push(model);
            }
        }
        DesktopAction::PresetModelMenuToggled(open) => state.preset_model_menu_open = open,
        DesktopAction::ShowModelsSubview(view) => {
            state.models_subview = view;
            state.active_preset = None;
            state.preset_model_menu_open = false;
            state.provider_kind_menu_open = false;
            state.mcp_transport_menu_open = false;
        }
        DesktopAction::ShowWebSubview(view) => state.web_subview = view,
        DesktopAction::ShowMcpSubview(view) => {
            state.mcp_subview = view;
            state.mcp_transport_menu_open = false;
        }
        DesktopAction::ProviderKindMenuToggled(open) => state.provider_kind_menu_open = open,
        DesktopAction::McpTransportMenuToggled(open) => state.mcp_transport_menu_open = open,
        DesktopAction::ShellKindMenuToggled(open) => state.shell_kind_menu_open = open,
        DesktopAction::LanguageMenuToggled(open) => state.language_menu_open = open,
        DesktopAction::UpdateStateChanged(update) => state.update = update,
        DesktopAction::UpdateDialogToggled(open) => state.update_dialog_open = open,
        DesktopAction::UpdateOfferFound(offer) => state.last_offer = Some(offer),
        DesktopAction::AutoUpdateToggled(auto_update) => state.auto_update = auto_update,
        DesktopAction::UpdateStaged(prepared) => {
            let version = state
                .last_offer
                .as_ref()
                .map(|offer| offer.version.clone())
                .unwrap_or_else(|| mycode_app::current_version().to_owned());
            state.prepared_update = Some(prepared);
            state.update = UpdateState::Ready { version };
        }
    }
    if touches_providers {
        ensure_model_selection(state);
    }
}

/// Applies one edit to the settings projection and marks the document dirty
/// when the edit reports it changed something. Collapses the
/// borrow-guard-mark boilerplate the settings arms share.
fn edit_settings(state: &mut WorkspaceState, edit: impl FnOnce(&mut SettingsState) -> bool) {
    if let Some(settings) = state.settings.as_mut()
        && edit(settings)
    {
        mark_settings_dirty(settings);
    }
}

/// Records one local settings edit so an in-flight save cannot clear it.
fn mark_settings_dirty(settings: &mut SettingsState) {
    settings.dirty = true;
    settings.edit_epoch = settings.edit_epoch.wrapping_add(1);
}

/// Closes every floating menu layer, whatever view it belongs to. Returns
/// whether anything was open, so Escape can tell a dismissal from a no-op.
pub(crate) fn close_floating_menus(state: &mut WorkspaceState) -> bool {
    let was_open = state.project_menu_open
        || state.workspace_menu_open
        || state.model_menu_open
        || state.reasoning_menu_open
        || state.subagent_menu.is_some()
        || state.preset_model_menu_open
        || state.provider_kind_menu_open
        || state.mcp_transport_menu_open
        || state.shell_kind_menu_open
        || state.language_menu_open
        || state.mention.is_some();
    state.project_menu_open = false;
    state.workspace_menu_open = false;
    state.workspace_rename_open = false;
    state.model_menu_open = false;
    state.reasoning_menu_open = false;
    state.subagent_menu = None;
    state.preset_model_menu_open = false;
    state.provider_kind_menu_open = false;
    state.mcp_transport_menu_open = false;
    state.shell_kind_menu_open = false;
    state.language_menu_open = false;
    state.mention = None;
    was_open
}
