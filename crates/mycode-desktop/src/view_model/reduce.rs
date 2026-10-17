//! Reducer internals: the pure transition functions behind
//! [`super::reduce`]. The `DesktopAction` mutations live here; the parent
//! module declares the state, action, and projection types. The dispatch
//! match lives here; arm bodies with their own logic live in the topical
//! submodules.
mod composer;
mod jobs;
mod models;
mod projects;
mod settings;
mod streaming;
mod usage;

use mycode_app::CHAT_CANCELLED;

use super::{
    ActiveConversation, DesktopAction, MentionKind, TurnStats, UpdateState, WorkspaceState,
};

pub(crate) use self::models::{
    preset_pick, reasoning_levels_for, selected_model_supports_reasoning,
    selected_reasoning_levels, strongest_reasoning_level,
};

pub(crate) use self::composer::{ComposerSubmit, composer_submit, preferred_slash_index};
use self::composer::{parse_mention, slash_items};
use self::jobs::{drop_live_job, refresh_subagent_status, tool_progress, tool_started};
use self::models::{
    active_preset_changed, apply_session_model, assign_fresh_session_model, ensure_model_selection,
    model_selected, provider_selected, remember_active_session_model, remember_session_model,
};
use self::projects::{
    bind_session_project, session_bindings_forgotten, session_project_bound,
    session_workspace_bound, sync_workspace_roots, workspace_created, workspace_removed,
    workspace_renamed, workspace_root_added, workspace_root_removed, workspace_switched,
};
use self::settings::{edit_settings, mark_settings_dirty};
use self::streaming::{append_streaming, set_streaming_status};
use self::usage::{arm_cache_watch, clear_session_usage, observe_cache_hit};

/// A mid-turn summary arrives while the final reply is still only in the
/// streaming bubble. Inserting it then paints the card above that reply.
/// Hold it until `ChatDone` appends the reply. A manual `/compact`, or an
/// automatic checkpoint taken before this turn has streamed anything, is
/// inserted where it arrives, which is also where the ledger stores it.
fn summary_waits_for_the_reply(state: &WorkspaceState) -> bool {
    state.sending
        && state.active.as_ref().is_some_and(|active| {
            active.streaming.as_ref().is_some_and(|reply| {
                !reply.text.trim().is_empty() || !reply.thinking.trim().is_empty()
            })
        })
}

/// Cuts a composer text to [`super::MAX_COMPOSER_CHARS`] in place.
fn bounded_draft(mut text: String) -> String {
    if let Some((cut, _)) = text.char_indices().nth(super::MAX_COMPOSER_CHARS) {
        text.truncate(cut);
    }
    text
}

/// Appends one follow-up to the queue unless it is blank or the queue is full.
fn queue_message(state: &mut WorkspaceState, text: String) {
    let text = bounded_draft(text);
    if !text.trim().is_empty() && state.queued.len() < super::MAX_QUEUED_MESSAGES {
        state.queued.push(text);
    }
}

/// Drops the turn-scoped view of the conversation leaving the screen: its
/// queue, steers, live jobs, asks, held summary, and the in-flight flag.
/// Events for a session that is not open are dropped, so none of this would
/// ever be cleared once another conversation takes the screen.
fn reset_turn_view(state: &mut WorkspaceState) {
    state.sending = false;
    state.pending_summary = None;
    state.queued.clear();
    state.queue_paused = false;
    state.pending_steers.clear();
    state.live_jobs.clear();
    state.subagent_window = None;
    state.changes_panel_open = false;
    state.pending_ask = None;
    state.ask_answers.clear();
    state.history_loading = false;
}

/// Closes the turn on screen after it committed or failed.
fn end_turn(state: &mut WorkspaceState) {
    state.live_jobs.clear();
    state.subagent_window = None;
    state.pending_ask = None;
    state.ask_answers.clear();
    state.sending = false;
}

fn push_entry_once(conversation: &mut ActiveConversation, entry: mycode_app::ConversationEntry) {
    let already = conversation
        .entries
        .iter()
        .any(|existing| existing.event_id == entry.event_id);
    if !already {
        conversation.entries.push(entry);
    }
}

/// Applies one action to the state.
pub(crate) fn reduce(state: &mut WorkspaceState, action: DesktopAction) {
    // A paged-back window stays readable through ordinary UI actions. Only
    // new tail activity (or opening a new snapshot) applies the live ceiling.
    let trim_window = matches!(
        action,
        DesktopAction::TurnArmed
            | DesktopAction::MessageSent { .. }
            | DesktopAction::SteerCommitted { .. }
            | DesktopAction::AssistantStepCommitted(_)
            | DesktopAction::ToolStarted { .. }
            | DesktopAction::ToolResultAppended(_)
            | DesktopAction::ChatDone { .. }
            | DesktopAction::ChatFailed { .. }
            | DesktopAction::SummaryShown(_)
            | DesktopAction::ConversationOpened(_)
    );
    let touches_providers = matches!(
        action,
        DesktopAction::SettingsLoaded(_)
            | DesktopAction::SettingsProviderAdded(_)
            | DesktopAction::SettingsProviderRemoved(_)
            | DesktopAction::SettingsProviderToggled(_, _)
            | DesktopAction::SettingsSaved { .. }
            | DesktopAction::UiStateLoaded { .. }
    );
    // Streaming ticks arrive many times a second and change no catalog the
    // `/` menu lists; rebuilding it for each one is wasted work.
    let streaming_tick = matches!(
        action,
        DesktopAction::ChatDelta(_)
            | DesktopAction::ChatThinkingDelta(_)
            | DesktopAction::ToolProgress { .. }
            | DesktopAction::UsageSnapshot { .. }
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
            // A new chat keeps the model already on the picker and pins it,
            // so leaving and coming back does not jump to the first enabled
            // model. The previous chat's pin is written first.
            if let Some(previous) = state
                .active
                .as_ref()
                .map(|active| active.session_id.clone())
            {
                remember_session_model(state, &previous);
            }
            state
                .sessions
                .retain(|s| s.session_id != summary.session_id);
            summary.active = true;
            state.sessions.insert(0, summary.clone());
            let session_id = summary.session_id.clone();
            // A turn still running in the previous chat keeps running, but
            // its events no longer reach this view; the fresh chat must not
            // inherit its in-flight flag, queue, or asks.
            reset_turn_view(state);
            state.active = Some(ActiveConversation {
                session_id: summary.session_id,
                branch_id: summary.root_branch_id,
                head: "empty".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                context_used: 0,
                context_cache: 0,
            });
            remember_session_model(state, &session_id);
            // The composer meter reads these fields directly; a fresh session
            // starts with an empty meter.
            clear_session_usage(state);
        }
        DesktopAction::SessionDeleted => {
            state.active = None;
            reset_turn_view(state);
            state.error = None;
            state.pending_welcome_send = None;
            clear_session_usage(state);
        }
        DesktopAction::ConversationParked => {
            state.active = None;
            for session in &mut state.sessions {
                session.active = false;
            }
            reset_turn_view(state);
            state.composer_draft.clear();
            state.mention = None;
            state.pending_welcome_send = None;
            state.resources.clear();
            clear_session_usage(state);
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
            let previous_session = if switched {
                state
                    .active
                    .as_ref()
                    .map(|active| active.session_id.clone())
            } else {
                None
            };
            if switched {
                // Queue, tasks, and asks belong to the previous session.
                reset_turn_view(state);
            }
            let session_id = conversation.session_id.clone();
            let restored_used = conversation.context_used;
            let restored_cache = conversation.context_cache;
            state.active = Some(conversation);
            for session in &mut state.sessions {
                session.active = session.session_id == session_id;
            }
            if switched {
                if let Some(previous) = previous_session {
                    remember_session_model(state, &previous);
                }
                clear_session_usage(state);
                // The tail already carries the last prompt's usage. Put it
                // back on the live meter so the cache ring paints before the
                // next turn.
                state.context_used = restored_used;
                state.context_cache = restored_cache;
                if !apply_session_model(state, &session_id) {
                    assign_fresh_session_model(state, &session_id);
                }
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
            let bounded = bounded_draft(text);
            state.mention = parse_mention(&bounded);
            state.composer_draft = bounded;
        }
        DesktopAction::WelcomeSendHeld(text) => {
            if state.pending_welcome_send.is_none()
                && let ComposerSubmit::StartTask(text) = composer_submit(state, &text)
            {
                let bounded = bounded_draft(text);
                if !bounded.trim().is_empty() {
                    state.pending_welcome_send = Some(bounded);
                }
            }
        }
        DesktopAction::WelcomeSendConsumed => {
            state.pending_welcome_send = None;
        }
        DesktopAction::MentionFiles(files) => {
            if let Some(mention) = state.mention.as_mut()
                && mention.kind == MentionKind::File
            {
                mention.items = files
                    .into_iter()
                    .map(|path| crate::view_model::MentionItem {
                        insert: path.clone(),
                        label: path,
                        group: crate::view_model::MentionGroup::File,
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
            queue_message(state, text);
            state.composer_draft.clear();
            state.mention = None;
        }
        DesktopAction::SteerSent(text) => state.pending_steers.push(text),
        DesktopAction::SteersReturned(texts) => {
            for text in texts {
                if let Some(position) = state.pending_steers.iter().position(|sent| *sent == text) {
                    state.pending_steers.remove(position);
                }
                queue_message(state, text);
            }
        }
        DesktopAction::QueuedMessageRemoved(index) => {
            if index < state.queued.len() {
                state.queued.remove(index);
            }
            state.queue_paused &= !state.queued.is_empty();
        }
        DesktopAction::QueueCleared => {
            state.queued.clear();
            state.queue_paused = false;
        }
        DesktopAction::QueuedMessageTaken => {
            if !state.queued.is_empty() {
                state.queued.remove(0);
            }
            state.queue_paused &= !state.queued.is_empty();
        }
        DesktopAction::QueuePaused(paused) => {
            state.queue_paused = paused && !state.queued.is_empty();
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
            if drop_live_job(state, &call_id) && state.sending {
                refresh_subagent_status(state);
            }
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
            if let Some(call_id) = entry.call_id.as_deref()
                && drop_live_job(state, call_id)
                && state.sending
            {
                refresh_subagent_status(state);
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
            context,
            context_cache,
            output,
            cache,
            requests,
            elapsed_ms,
        } => {
            state.live_turn = Some(TurnStats {
                model,
                input,
                output,
                cache,
                requests,
                elapsed_ms,
            });
            if context > 0 {
                state.context_used = context;
                state.context_cache = context_cache;
            }
            // The watch compares this prompt, not the summed turn. A size of
            // zero leaves the watch armed for the next real reading.
            observe_cache_hit(state, context, context_cache);
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
        DesktopAction::SessionFilterChanged(query) => state.session_filter = query,
        DesktopAction::HistoryLoadStarted => state.history_loading = true,
        DesktopAction::HistoryLoadFinished => state.history_loading = false,
        DesktopAction::OlderLoaded {
            session_id,
            entries,
            older_before,
            requested_before,
        } => {
            state.history_loading = false;
            let applicable = state.active.as_ref().is_some_and(|active| {
                active.session_id == session_id
                    && active.older_before.as_deref() == Some(requested_before.as_str())
            });
            if applicable {
                let active = state.active.as_mut().expect("applicable conversation");
                let mut older = entries;
                older.append(&mut active.entries);
                active.entries = older;
                active.older_before = older_before;
            }
        }
        DesktopAction::ChatThinkingDelta(delta) => {
            append_streaming(state, true, delta);
        }
        DesktopAction::SummaryShown(entry) => {
            if summary_waits_for_the_reply(state) {
                if state
                    .pending_summary
                    .as_ref()
                    .is_none_or(|pending| pending.event_id != entry.event_id)
                {
                    state.pending_summary = Some(entry);
                }
            } else if let Some(conversation) = state.active.as_mut() {
                push_entry_once(conversation, entry);
            }
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
            // Steers that did not commit come back as `SteersReturned`.
            state.pending_steers.clear();
            let summary = state.pending_summary.take();
            if let Some(conversation) = state.active.as_mut() {
                conversation.head = head;
                // A turn that ended on a committed tool step reports that
                // step again as its last message; it is already listed.
                push_entry_once(conversation, entry);
                // The summary follows the reply, matching the ledger order
                // a reopen projects.
                if let Some(summary) = summary {
                    push_entry_once(conversation, summary);
                }
                conversation.streaming = None;
            }
            end_turn(state);
            // The finished turn keeps its meter: the last live snapshot held
            // the turn's sums, so it becomes the completed-turn stats.
            state.last_turn = state.live_turn.take();
        }
        DesktopAction::ChatFailed { message, head } => {
            state.pending_steers.clear();
            let summary = state.pending_summary.take();
            if let Some(conversation) = state.active.as_mut() {
                if !head.is_empty() {
                    conversation.head = head;
                }
                if let Some(summary) = summary {
                    push_entry_once(conversation, summary);
                }
                conversation.streaming = None;
            }
            // A user-initiated cancel resets the turn without an error
            // banner; the sentinel travels as the failure message.
            // Context meters stay: a cancel is not a new empty session.
            if message != CHAT_CANCELLED {
                state.error = Some(message);
            }
            end_turn(state);
            state.live_turn = None;
        }
        DesktopAction::CacheBreakArmed(cause) => arm_cache_watch(state, cause),
        DesktopAction::SteerCommitted {
            session_id,
            head,
            entry,
        } => {
            if let Some(conversation) = state.active.as_mut()
                && conversation.session_id == session_id
            {
                if !head.is_empty() {
                    conversation.head = head;
                }
                if let Some(position) = state
                    .pending_steers
                    .iter()
                    .position(|sent| *sent == entry.text.as_ref())
                {
                    state.pending_steers.remove(position);
                }
                push_entry_once(conversation, entry);
            }
        }
        DesktopAction::Failed(message) => {
            state.error = Some(message);
        }
        DesktopAction::SettingsLoaded(settings) => settings::settings_loaded(state, *settings),
        DesktopAction::SettingsLanguageSelected(language) => {
            settings::language_selected(state, language);
        }
        DesktopAction::SettingsThemeSelected(theme) => settings::theme_selected(state, &theme),
        DesktopAction::SettingsFontSizeSelected(font_size) => {
            settings::font_size_selected(state, font_size);
        }
        DesktopAction::InspectorChanged { open, pinned } => {
            state.inspector_open = open;
            state.inspector_pinned = pinned;
        }
        DesktopAction::SettingsProxyChanged { value, .. } => {
            edit_settings(state, |settings| {
                if settings.proxy == value {
                    return false;
                }
                settings.proxy = value;
                true
            });
        }
        DesktopAction::SettingsModelsShown {
            provider,
            models,
            shown,
        } => settings::models_shown(state, &provider, models, shown),
        DesktopAction::SettingsProviderToggled(index, enabled) => {
            edit_settings(state, |settings| {
                settings.providers.get_mut(index).is_some_and(|provider| {
                    provider.enabled = enabled;
                    true
                })
            });
        }
        DesktopAction::SettingsProviderBaseUrlChanged { id, base_url } => {
            settings::provider_base_url_changed(state, &id, base_url);
        }
        DesktopAction::SettingsProviderAdded(provider) => settings::provider_added(state, provider),
        DesktopAction::SettingsProviderRemoved(index) => settings::provider_removed(state, index),
        DesktopAction::SettingsBackendToggled(index, enabled) => {
            settings::backend_toggled(state, index, enabled);
        }
        DesktopAction::SettingsSubagentsChanged(subagents) => {
            edit_settings(state, |settings| {
                settings.subagents = subagents;
                true
            });
        }
        DesktopAction::SettingsSkillToggled(slug, enabled) => {
            edit_settings(state, |settings| {
                settings.skills.set_enabled(&slug, enabled)
            });
        }
        DesktopAction::SubagentMenuToggled(menu) => {
            let opening = menu.is_some();
            state.subagent_menu = menu;
            state.model_menu_browse = None;
            state.picker_query.clear();
            if opening {
                state.model_menu_open = false;
                state.reasoning_menu_open = false;
            }
        }
        DesktopAction::SettingsSaved {
            revision,
            edit_epoch,
        } => settings::settings_saved(state, revision, edit_epoch),
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
        DesktopAction::SettingsMcpAdded(server) => settings::mcp_added(state, server),
        DesktopAction::SettingsMcpRemoved(index) => settings::mcp_removed(state, index),
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
            settings::mcp_tools_listed(state, server_id, tools);
        }
        DesktopAction::McpProbeFailed { server_id, message } => {
            state.mcp_probing.retain(|id| *id != server_id);
            state.mcp_tools.retain(|(id, _)| *id != server_id);
            state.error = Some(message);
        }
        DesktopAction::ShowMainView(view) => {
            state.view = view;
            state.provider_detail = None;
            state.web_detail = None;
            // Crossing views closes every floating menu so no stale layer
            // renders above the destination view.
            close_floating_menus(state);
        }
        DesktopAction::ShowSettingsSection(section) => {
            if section != super::SettingsSection::Models {
                state.provider_detail = None;
            }
            if section != super::SettingsSection::Web {
                state.web_detail = None;
            }
            state.model_menu_open = false;
            state.model_menu_browse = None;
            state.picker_query.clear();
            state.subagent_menu = None;
            state.settings_section = section;
        }
        DesktopAction::ProjectMenuToggled(open) => state.project_menu_open = open,
        DesktopAction::CatalogLoaded {
            document,
            fetched_at,
        } => {
            let had_catalog = state.catalog.is_some();
            state.catalog = Some(document);
            state.catalog_fetched_at = fetched_at;
            if let Some(preset) = state.active_preset.clone()
                && (!had_catalog && state.preset_models.is_empty()
                    || preset == mycode_providers::OPENAI_CODEX_PROVIDER_ID)
            {
                active_preset_changed(state, Some(preset));
            }
        }
        DesktopAction::UiStateLoaded {
            recents,
            last_project,
            auto_update,
            selected_provider,
            selected_model,
            selected_reasoning,
            session_projects,
            session_models,
            workspaces,
            session_workspaces,
            active_workspace: active,
            recent_models,
            thinking_collapsed,
            window_size,
        } => {
            state.thinking_collapsed = thinking_collapsed;
            state.window_size = state.window_size.or(window_size);
            state.recents = recents;
            state.project_dir = last_project.filter(|path| !path.trim().is_empty());
            state.session_projects = session_projects;
            state.session_models = session_models;
            state.workspaces = workspaces;
            state.session_workspaces = session_workspaces;
            state.active_workspace = active;
            state.recent_models = recent_models;
            state.default_reasoning = selected_reasoning;
            sync_workspace_roots(state);
            state.auto_update = auto_update;
            if selected_provider.is_some() {
                state.selected_provider = selected_provider;
                state.selected_model = selected_model;
            }
            ensure_model_selection(state);
            if let Some(session_id) = state
                .active
                .as_ref()
                .map(|active| active.session_id.clone())
                && !apply_session_model(state, &session_id)
            {
                remember_session_model(state, &session_id);
            }
        }
        DesktopAction::ThinkingCollapsedToggled => {
            state.thinking_collapsed = !state.thinking_collapsed;
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
            state.model_menu_browse = None;
            state.picker_query.clear();
            if open {
                state.reasoning_menu_open = false;
                state.subagent_menu = None;
            }
        }
        DesktopAction::ModelMenuBrowse(provider) => {
            state.reasoning_menu_open = false;
            state.model_menu_browse = provider;
            state.picker_query.clear();
        }
        DesktopAction::PickerQueryChanged(query) => state.picker_query = query,
        DesktopAction::SettingsQueryChanged(query) => state.settings_query = query,
        DesktopAction::ProviderDetailOpened(provider) => {
            state.provider_detail = provider;
            state.model_menu_open = false;
            state.model_menu_browse = None;
            state.picker_query.clear();
        }
        DesktopAction::PresetModelQueryChanged(query) => state.preset_model_query = query,
        DesktopAction::ReasoningMenuToggled(open) => {
            state.reasoning_menu_open = open;
            if open {
                state.model_menu_open = false;
            }
        }
        DesktopAction::SettingsReasoningChanged(level) => {
            state.reasoning_menu_open = false;
            // Stored on the session pin. The live projection is updated so the
            // chip matches, and the document is left clean so this action does
            // not schedule a save.
            if state.settings.is_none() {
                return;
            }
            let picked: Option<String> = if level == "default" || level.is_empty() {
                None
            } else if selected_reasoning_levels(state).contains(&level) {
                Some(level)
            } else {
                return;
            };
            if let Some(settings) = state.settings.as_mut() {
                settings.reasoning = picked.clone();
            }
            state.default_reasoning = picked;
            remember_active_session_model(state);
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
        DesktopAction::ShowModelsSubview(view) => {
            state.models_subview = view;
            state.provider_kind_menu_open = false;
            state.mcp_transport_menu_open = false;
            state.provider_detail = None;
            // The OAuth page carries no preset form: signing in binds the
            // catalog row's models, edited later on the provider detail.
            active_preset_changed(state, None);
        }
        DesktopAction::WebDetailOpened(backend) => state.web_detail = backend,
        DesktopAction::ShowMcpSubview(view) => {
            state.mcp_subview = view;
            state.mcp_transport_menu_open = false;
        }
        DesktopAction::ProviderKindMenuToggled(open) => state.provider_kind_menu_open = open,
        DesktopAction::McpTransportMenuToggled(open) => state.mcp_transport_menu_open = open,
        DesktopAction::LanguageMenuToggled(open) => state.language_menu_open = open,
        DesktopAction::UpdateStateChanged(update) => state.update = update,
        DesktopAction::UpdateDialogToggled(open) => state.update_dialog_open = open,
        DesktopAction::CommandPaletteSet(open) => {
            state.command_palette_open = open;
            if open {
                close_floating_menus(state);
            }
        }
        DesktopAction::UpdateOfferFound(offer) => {
            // The offer and the Available flip are one transition: the
            // download button's handler reads `last_offer`, so an
            // Available state without a published offer is a dead button.
            state.update = UpdateState::Available {
                version: offer.version.clone(),
                notes_url: offer.notes_url.clone(),
            };
            state.last_offer = Some(offer);
        }
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
    if trim_window {
        trim_resident_entries(state);
    }
    if touches_providers {
        ensure_model_selection(state);
    }
    if !streaming_tick {
        fill_open_slash(state);
    }
}

/// Ceiling on entries held for one open conversation. The transcript pages
/// from disk in both directions; past this the oldest rows drop out of the
/// window and page back in on demand, so a long-running window stays
/// bounded. Older-page prepends are not trimmed so the newly loaded page is
/// never discarded on arrival.
const MAX_RESIDENT_ENTRIES: usize = 1500;

fn trim_resident_entries(state: &mut WorkspaceState) {
    if let Some(active) = state.active.as_mut()
        && active.entries.len() > MAX_RESIDENT_ENTRIES
    {
        let excess = active.entries.len() - MAX_RESIDENT_ENTRIES;
        active.entries.drain(..excess);
        if let Some(first_ledger_id) = active.entries.iter().find_map(|entry| {
            mycode_app::SessionEventId::parse(&entry.event_id)
                .map(|event_id| event_id.as_str().to_owned())
        }) {
            active.older_before = Some(first_ledger_id);
        }
    }
}

/// Rebuilds the open `/` menu from the catalogs this action may have changed.
fn fill_open_slash(state: &mut WorkspaceState) {
    let Some(fragment) = state.mention.as_ref().and_then(|mention| {
        (mention.kind == MentionKind::Command).then(|| mention.fragment.clone())
    }) else {
        return;
    };
    let skills = state.enabled_skills();
    let servers = state
        .settings
        .as_ref()
        .map(|settings| settings.mcp_servers.clone())
        .unwrap_or_default();
    let tools = state.mcp_tools.clone();
    if let Some(mention) = state.mention.as_mut() {
        mention.items = slash_items(&fragment, &skills, &servers, &tools);
    }
}

/// Closes every floating menu layer, whatever view it belongs to. Returns
/// whether anything was open, so Escape can tell a dismissal from a no-op.
pub(crate) fn close_floating_menus(state: &mut WorkspaceState) -> bool {
    let was_open = state.project_menu_open
        || state.workspace_menu_open
        || state.model_menu_open
        || state.reasoning_menu_open
        || state.subagent_menu.is_some()
        || state.provider_kind_menu_open
        || state.mcp_transport_menu_open
        || state.language_menu_open
        || state.mention.is_some();
    state.project_menu_open = false;
    state.workspace_menu_open = false;
    state.workspace_rename_open = false;
    state.model_menu_open = false;
    state.model_menu_browse = None;
    state.picker_query.clear();
    state.reasoning_menu_open = false;
    state.subagent_menu = None;
    state.provider_kind_menu_open = false;
    state.mcp_transport_menu_open = false;
    state.language_menu_open = false;
    state.mention = None;
    was_open
}

#[cfg(test)]
mod tests;
