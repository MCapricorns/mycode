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
    ActiveConversation, DesktopAction, MentionKind, SettingsState, TurnStats, UpdateState,
    WorkspaceState,
};

pub(crate) use self::models::{
    reasoning_levels_for, selected_model_supports_reasoning, selected_reasoning_levels,
    strongest_reasoning_level,
};

pub(crate) use self::composer::{ComposerSubmit, composer_submit, preferred_slash_index};
use self::composer::{parse_mention, slash_items};
use self::jobs::{drop_live_job, tool_progress, tool_started};
use self::models::{
    active_preset_changed, apply_session_model, assign_fresh_session_model, ensure_model_selection,
    model_selected, provider_selected, remember_active_session_model, remember_session_model,
};
use self::projects::{
    bind_session_project, session_bindings_forgotten, session_project_bound,
    session_workspace_bound, sync_workspace_roots, workspace_created, workspace_removed,
    workspace_renamed, workspace_root_added, workspace_root_removed, workspace_switched,
};
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
            state.live_jobs.clear();
            state.subagent_window = None;
            state.changes_panel_open = false;
            state.history_loading = false;
            // The composer meter reads these fields directly; a fresh session
            // starts with an empty meter.
            clear_session_usage(state);
        }
        DesktopAction::SessionDeleted => {
            state.active = None;
            state.sending = false;
            state.pending_summary = None;
            state.queued.clear();
            state.live_jobs.clear();
            state.subagent_window = None;
            state.changes_panel_open = false;
            state.pending_ask = None;
            state.ask_answers.clear();
            state.error = None;
            state.history_loading = false;
            state.pending_welcome_send = None;
            clear_session_usage(state);
        }
        DesktopAction::ConversationParked => {
            state.active = None;
            for session in &mut state.sessions {
                session.active = false;
            }
            state.sending = false;
            state.pending_summary = None;
            state.queued.clear();
            state.live_jobs.clear();
            state.subagent_window = None;
            state.changes_panel_open = false;
            state.pending_ask = None;
            state.ask_answers.clear();
            state.history_loading = false;
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
                state.queued.clear();
                state.pending_steers.clear();
                state.live_jobs.clear();
                state.subagent_window = None;
                state.changes_panel_open = false;
                state.pending_ask = None;
                state.ask_answers.clear();
                state.pending_summary = None;
                state.sending = false;
                state.history_loading = false;
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
            let bounded: String = text.chars().take(super::MAX_COMPOSER_CHARS).collect();
            state.mention = parse_mention(&bounded);
            state.composer_draft = bounded;
        }
        DesktopAction::WelcomeSendHeld(text) => {
            if state.pending_welcome_send.is_none()
                && let ComposerSubmit::StartTask(text) = composer_submit(state, &text)
            {
                let bounded: String = text.chars().take(super::MAX_COMPOSER_CHARS).collect();
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
            let bounded: String = text.chars().take(super::MAX_COMPOSER_CHARS).collect();
            if !bounded.trim().is_empty() && state.queued.len() < super::MAX_QUEUED_MESSAGES {
                state.queued.push(bounded);
            }
            state.composer_draft.clear();
            state.mention = None;
        }
        DesktopAction::SteerSent(text) => state.pending_steers.push(text),
        DesktopAction::SteersReturned(texts) => {
            for text in texts {
                if let Some(position) = state.pending_steers.iter().position(|sent| *sent == text) {
                    state.pending_steers.remove(position);
                }
                let bounded: String = text.chars().take(super::MAX_COMPOSER_CHARS).collect();
                if !bounded.trim().is_empty() && state.queued.len() < super::MAX_QUEUED_MESSAGES {
                    state.queued.push(bounded);
                }
            }
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
                drop_live_job(state, call_id);
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
            state.live_jobs.clear();
            state.subagent_window = None;
            state.pending_ask = None;
            state.ask_answers.clear();
            // The finished turn keeps its meter: the last live snapshot held
            // the turn's sums, so it becomes the completed-turn stats.
            state.last_turn = state.live_turn.take();
            state.sending = false;
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
            state.live_jobs.clear();
            state.subagent_window = None;
            state.pending_ask = None;
            state.ask_answers.clear();
            state.live_turn = None;
            state.sending = false;
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
                    crate::i18n::apply_language(&settings.language);
                    state.settings = Some(settings);
                }
            }
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
        DesktopAction::SettingsThemeSelected(theme) => {
            let theme = crate::ui::desk::normalize_theme(&theme).to_owned();
            if let Some(settings) = state.settings.as_mut() {
                settings.theme = theme;
                mark_settings_dirty(settings);
            }
        }
        DesktopAction::SettingsFontSizeSelected(font_size) => {
            if !mycode_config::VALID_FONT_SIZES.contains(&font_size.as_str()) {
                return;
            }
            if let Some(settings) = state.settings.as_mut() {
                settings.font_size = font_size;
                mark_settings_dirty(settings);
            }
        }
        DesktopAction::SettingsFontFamilySelected(font_family) => {
            let Some(font_family) = mycode_config::canonical_font_family(&font_family) else {
                return;
            };
            if let Some(settings) = state.settings.as_mut() {
                if mycode_config::canonical_font_family(&settings.font_family) == Some(font_family)
                {
                    return;
                }
                settings.font_family = font_family.to_owned();
                mark_settings_dirty(settings);
            }
        }
        DesktopAction::InspectorChanged { open, pinned } => {
            state.inspector_open = open;
            state.inspector_pinned = pinned;
        }
        DesktopAction::SettingsUserAgentChanged(user_agent) => {
            edit_settings(state, |settings| {
                if settings.user_agent == user_agent {
                    return false;
                }
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
        DesktopAction::SettingsProviderBaseUrlChanged { id, base_url } => {
            edit_settings(state, |settings| {
                settings
                    .providers
                    .iter_mut()
                    .find(|provider| provider.id == id)
                    .is_some_and(|provider| {
                        if provider.base_url == base_url {
                            false
                        } else {
                            provider.base_url = base_url;
                            true
                        }
                    })
            });
        }
        DesktopAction::SettingsProviderAdded(provider) => {
            edit_settings(state, |settings| {
                if let Some(existing) = settings
                    .providers
                    .iter_mut()
                    .find(|existing| existing.id == provider.id)
                {
                    *existing = provider;
                    true
                } else if settings.providers.len() < mycode_config::MAX_PROVIDERS {
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
            state.provider_detail = None;
            // Crossing views closes every floating menu so no stale layer
            // renders above the destination view.
            close_floating_menus(state);
        }
        DesktopAction::ShowSettingsSection(section) => {
            if section != super::SettingsSection::Models {
                state.provider_detail = None;
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
            state.catalog = Some(document);
            state.catalog_fetched_at = fetched_at;
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
            starred_models,
            thinking_collapsed,
        } => {
            state.thinking_collapsed = thinking_collapsed;
            state.recents = recents;
            state.project_dir = last_project.filter(|path| !path.trim().is_empty());
            state.session_projects = session_projects;
            state.session_models = session_models;
            state.workspaces = workspaces;
            state.session_workspaces = session_workspaces;
            state.active_workspace = active;
            state.recent_models = recent_models;
            state.starred_models = starred_models;
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
        DesktopAction::ModelStarToggled { provider, model } => {
            let _ = mycode_config::toggle_star(&mut state.starred_models, &provider, &model);
        }
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
                Some(level.clone())
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
            // The OpenDesign page is the add form for that one provider.
            let preset = (view == crate::view_model::ModelsSubview::OpenDesign)
                .then(|| mycode_providers::catalog::opendesign::PROVIDER_ID.to_owned());
            active_preset_changed(state, preset);
        }
        DesktopAction::OpenDesignLoading(loading) => state.opendesign_loading = loading,
        DesktopAction::ShowWebSubview(view) => state.web_subview = view,
        DesktopAction::ShowMcpSubview(view) => {
            state.mcp_subview = view;
            state.mcp_transport_menu_open = false;
        }
        DesktopAction::ProviderKindMenuToggled(open) => state.provider_kind_menu_open = open,
        DesktopAction::McpTransportMenuToggled(open) => state.mcp_transport_menu_open = open,
        DesktopAction::LanguageMenuToggled(open) => {
            state.language_menu_open = open;
            if open {
                state.font_family_menu_open = false;
            }
        }
        DesktopAction::FontFamilyMenuToggled(open) => {
            state.font_family_menu_open = open;
            if open {
                state.language_menu_open = false;
            }
        }
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
    fill_open_slash(state);
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
    let skills = state.skills.clone();
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
        || state.provider_kind_menu_open
        || state.mcp_transport_menu_open
        || state.language_menu_open
        || state.font_family_menu_open
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
    state.font_family_menu_open = false;
    state.mention = None;
    was_open
}

#[cfg(test)]
mod tests {
    use super::reduce;
    use crate::view_model::{
        ActiveConversation, ConversationEntry, DesktopAction, EntryKind, TurnStats, UpdateState,
        WorkspaceState,
    };

    fn user(id: &str, text: &str) -> ConversationEntry {
        ConversationEntry {
            event_id: id.to_owned(),
            kind: EntryKind::UserMessage,
            text: text.into(),
            call_id: None,
            thinking: String::new(),
        }
    }

    fn open(entries: Vec<ConversationEntry>, older: Option<&str>) -> WorkspaceState {
        WorkspaceState {
            history_loading: true,
            active: Some(ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "br".to_owned(),
                head: "evt-head".to_owned(),
                entries,
                older_before: older.map(str::to_owned),
                streaming: None,
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        }
    }

    #[test]
    fn an_offer_found_publishes_the_offer_and_flips_available_together() {
        let offer = mycode_app::UpdateOffer {
            version: "9.9.9".to_owned(),
            notes_url: "https://example.com/releases/v9.9.9".to_owned(),
            asset_url: "https://example.com/app.zip".to_owned(),
            checksum_url: "https://example.com/app.zip.sha256".to_owned(),
        };
        let mut state = WorkspaceState::default();
        reduce(&mut state, DesktopAction::UpdateOfferFound(offer.clone()));
        // The download button's handler reads `last_offer`; an Available
        // state without a published offer is a dead button.
        assert_eq!(state.last_offer, Some(offer));
        assert_eq!(
            state.update,
            UpdateState::Available {
                version: "9.9.9".to_owned(),
                notes_url: "https://example.com/releases/v9.9.9".to_owned(),
            }
        );
        reduce(
            &mut state,
            DesktopAction::UpdateStaged(mycode_app::PreparedUpdate {
                new_binary: std::path::PathBuf::from("stage/app.exe"),
                stage_dir: std::path::PathBuf::from("stage"),
                binary_sha256: "abc123".to_owned(),
            }),
        );
        // Staging reports the offered version, not the running one.
        assert_eq!(
            state.update,
            UpdateState::Ready {
                version: "9.9.9".to_owned(),
            }
        );
    }

    fn ledger_id(index: usize) -> String {
        format!("evt1-{index:032x}")
    }

    #[test]
    fn the_open_conversation_is_capped_at_the_resident_entry_ceiling() {
        let mut entries: Vec<ConversationEntry> = (0..super::MAX_RESIDENT_ENTRIES + 400)
            .map(|index| user(&ledger_id(index), "row"))
            .collect();
        // A live tool placeholder (`call-*`) is not a ledger event id; if it
        // lands at the front of the kept window, `older_before` must skip it
        // and anchor on the first real ledger entry.
        entries[400] = ConversationEntry {
            event_id: "call-live-1".to_owned(),
            kind: EntryKind::ToolCall,
            text: "read  src/lib.rs".into(),
            call_id: Some("live-1".to_owned()),
            thinking: String::new(),
        };
        let mut state = open(entries, None);
        reduce(&mut state, DesktopAction::TurnArmed);
        let active = state.active.expect("conversation stays open");
        assert_eq!(
            active.entries.len(),
            super::MAX_RESIDENT_ENTRIES,
            "resident entries must be trimmed to the ceiling"
        );
        assert_eq!(
            active.entries.last().expect("kept tail").event_id,
            ledger_id(super::MAX_RESIDENT_ENTRIES + 399),
            "trimming drops the oldest rows, never the tail"
        );
        assert_eq!(
            active.older_before.as_deref(),
            Some(ledger_id(401).as_str()),
            "older_before advances to the first remaining ledger event id so trimmed rows can page back in"
        );
    }

    #[test]
    fn older_loaded_is_not_trimmed_past_the_resident_ceiling() {
        let tail: Vec<ConversationEntry> = (10..super::MAX_RESIDENT_ENTRIES + 10)
            .map(|index| user(&ledger_id(index), "row"))
            .collect();
        let older_page: Vec<ConversationEntry> = (0..10)
            .map(|index| user(&ledger_id(index), "older"))
            .collect();
        let requested = ledger_id(10);
        let mut state = open(tail, Some(&requested));
        reduce(
            &mut state,
            DesktopAction::OlderLoaded {
                session_id: "ses".to_owned(),
                entries: older_page,
                older_before: None,
                requested_before: requested,
            },
        );
        reduce(
            &mut state,
            DesktopAction::ComposerChanged("draft".to_owned()),
        );
        reduce(&mut state, DesktopAction::HistoryLoadStarted);
        let active = state.active.expect("open");
        assert_eq!(active.entries.len(), super::MAX_RESIDENT_ENTRIES + 10);
        assert_eq!(
            active.entries.first().expect("first").event_id,
            ledger_id(0)
        );
        assert!(active.older_before.is_none());
    }

    #[test]
    fn non_turn_failure_preserves_sending_and_streaming_during_a_turn() {
        use crate::view_model::StreamingReply;

        let mut state = WorkspaceState {
            sending: true,
            active: Some(ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "br".to_owned(),
                head: "evt-1".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: Some(StreamingReply {
                    text: "partial".to_owned(),
                    thinking: String::new(),
                    status: "Replying".to_owned(),
                }),
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        reduce(
            &mut state,
            DesktopAction::Failed("branch head moved on".to_owned()),
        );
        assert!(state.sending, "non-turn failure must not clear sending");
        assert_eq!(state.error.as_deref(), Some("branch head moved on"));
        assert!(
            state
                .active
                .as_ref()
                .and_then(|active| active.streaming.as_ref())
                .is_some()
        );
    }

    #[test]
    fn inspector_changed_toggles_open_and_keeps_the_pin() {
        let mut state = WorkspaceState::default();
        assert!(!state.inspector_open);
        assert!(!state.inspector_pinned);
        reduce(
            &mut state,
            DesktopAction::InspectorChanged {
                open: true,
                pinned: false,
            },
        );
        assert!(state.inspector_open);
        assert!(!state.inspector_pinned);
        reduce(
            &mut state,
            DesktopAction::InspectorChanged {
                open: true,
                pinned: true,
            },
        );
        assert!(state.inspector_open);
        assert!(state.inspector_pinned);
        reduce(
            &mut state,
            DesktopAction::InspectorChanged {
                open: false,
                pinned: true,
            },
        );
        assert!(!state.inspector_open);
        assert!(state.inspector_pinned);
    }

    #[test]
    fn older_page_prepends_when_the_cursor_still_matches() {
        let mut state = open(vec![user("e5", "new")], Some("e5"));
        reduce(
            &mut state,
            DesktopAction::OlderLoaded {
                session_id: "ses".to_owned(),
                entries: vec![user("e0", "old")],
                older_before: None,
                requested_before: "e5".to_owned(),
            },
        );
        let active = state.active.expect("open");
        assert_eq!(
            active
                .entries
                .iter()
                .map(|entry| entry.event_id.as_str())
                .collect::<Vec<_>>(),
            ["e0", "e5"]
        );
        assert!(active.older_before.is_none());
        assert!(!state.history_loading);
    }

    #[test]
    fn stale_older_page_is_dropped() {
        let mut state = open(vec![user("e5", "new")], Some("e5"));
        reduce(
            &mut state,
            DesktopAction::OlderLoaded {
                session_id: "ses".to_owned(),
                entries: vec![user("e0", "old")],
                older_before: None,
                requested_before: "e9".to_owned(),
            },
        );
        let active = state.active.expect("open");
        assert_eq!(active.entries.len(), 1);
        assert_eq!(active.entries[0].event_id, "e5");
        assert_eq!(active.older_before.as_deref(), Some("e5"));
        assert!(!state.history_loading);
    }

    #[test]
    fn reasoning_pick_stays_on_the_session_and_does_not_dirty_settings() {
        use std::sync::Arc;

        use mycode_providers::catalog::{CatalogDocument, CatalogModel, CatalogProvider};

        let mut state = WorkspaceState::default();
        let mut settings = crate::view_model::SettingsState::from_settings(
            &mycode_config::AppSettings::default(),
            1,
            Vec::new(),
        );
        settings.saving = true;
        state.settings = Some(settings);
        state.catalog = Some(Arc::new(CatalogDocument {
            providers: vec![CatalogProvider {
                id: "zhipu".to_owned(),
                models: vec![CatalogModel {
                    id: "glm-4.7".to_owned(),
                    reasoning: true,
                    reasoning_efforts: vec!["high".to_owned()],
                    ..CatalogModel::default()
                }],
                ..CatalogProvider::default()
            }],
        }));
        state.selected_provider = Some("zhipu".to_owned());
        state.selected_model = Some("glm-4.7".to_owned());
        state.active = Some(mycode_app::ActiveConversation {
            session_id: "session-a".to_owned(),
            branch_id: "branch".to_owned(),
            head: "empty".to_owned(),
            entries: Vec::new(),
            older_before: None,
            streaming: None,
            context_used: 0,
            context_cache: 0,
        });
        reduce(
            &mut state,
            DesktopAction::SettingsReasoningChanged("high".to_owned()),
        );
        let settings = state.settings.expect("settings");
        assert_eq!(settings.reasoning.as_deref(), Some("high"));
        assert_eq!(settings.to_settings().reasoning_effort, None);
        assert!(!settings.dirty);
        assert!(settings.saving);
        assert_eq!(
            mycode_config::session_model(&state.session_models, "session-a")
                .and_then(|pin| pin.reasoning.as_deref()),
            Some("high")
        );
    }

    #[test]
    fn a_new_session_clears_the_previous_context_meter() {
        let turn = TurnStats {
            model: "deepseek-v4-flash".to_owned(),
            input: 25_000,
            output: 100,
            cache: Some(24_300),
            requests: 1,
            elapsed_ms: 10,
        };
        let mut state = WorkspaceState {
            context_used: 25_000,
            context_cache: 24_300,
            last_turn: Some(turn.clone()),
            live_turn: Some(turn),
            active: Some(ActiveConversation {
                session_id: "old".to_owned(),
                branch_id: "branch".to_owned(),
                head: "use-1".to_owned(),
                entries: vec![user("use-1", "hello")],
                older_before: None,
                streaming: None,
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        reduce(
            &mut state,
            DesktopAction::SessionCreated(mycode_app::SessionSummary {
                session_id: "new".to_owned(),
                root_branch_id: "branch".to_owned(),
                title: String::new(),
                updated_at_ms: None,
                event_count: 0,
                active: false,
                corrupt: false,
            }),
        );
        assert_eq!(
            state
                .active
                .as_ref()
                .map(|active| active.session_id.as_str()),
            Some("new")
        );
        assert_eq!(state.context_used, 0);
        assert_eq!(state.context_cache, 0);
        assert!(state.last_turn.is_none());
        assert!(state.live_turn.is_none());

        // A reopen whose tail has no usage snapshot stays empty. The previous
        // session's in-memory meter does not leak across.
        reduce(
            &mut state,
            DesktopAction::ConversationOpened(ActiveConversation {
                session_id: "old".to_owned(),
                branch_id: "branch".to_owned(),
                head: "use-1".to_owned(),
                entries: vec![user("use-1", "hello")],
                older_before: None,
                streaming: None,
                context_used: 0,
                context_cache: 0,
            }),
        );
        assert_eq!(state.context_used, 0);
        assert_eq!(state.context_cache, 0);
        assert!(state.last_turn.is_none());
        assert!(state.live_turn.is_none());

        // Reopening a session whose tail recorded a prompt restores that
        // meter. The ring must not wait for another turn.
        reduce(
            &mut state,
            DesktopAction::ConversationOpened(ActiveConversation {
                session_id: "metered".to_owned(),
                branch_id: "branch".to_owned(),
                head: "use-2".to_owned(),
                entries: vec![user("use-2", "hello")],
                older_before: None,
                streaming: None,
                context_used: 42_365,
                context_cache: 41_472,
            }),
        );
        assert_eq!(state.context_used, 42_365);
        assert_eq!(state.context_cache, 41_472);
        assert!(state.last_turn.is_none());
        assert!(state.live_turn.is_none());

        reduce(&mut state, DesktopAction::ConversationParked);
        assert!(state.active.is_none());
        assert_eq!(state.context_used, 0);
        assert_eq!(state.context_cache, 0);
        assert!(state.last_turn.is_none());

        state.context_used = 25_000;
        state.context_cache = 24_300;
        reduce(&mut state, DesktopAction::SessionDeleted);
        assert_eq!(state.context_used, 0);
        assert_eq!(state.context_cache, 0);
    }

    #[test]
    fn a_steer_shows_until_it_commits_and_a_returned_one_is_queued() {
        let mut state = open(Vec::new(), None);
        state.composer_draft = "typing the next one".to_owned();
        reduce(&mut state, DesktopAction::SteerSent("first".to_owned()));
        reduce(&mut state, DesktopAction::SteerSent("second".to_owned()));
        assert_eq!(state.pending_steers, ["first", "second"]);
        let session_id = state.active.as_ref().expect("active").session_id.clone();
        reduce(
            &mut state,
            DesktopAction::SteerCommitted {
                session_id,
                head: String::new(),
                entry: user("use-steer", "first"),
            },
        );
        assert_eq!(state.pending_steers, ["second"]);
        reduce(
            &mut state,
            DesktopAction::SteersReturned(vec!["second".to_owned()]),
        );
        assert!(state.pending_steers.is_empty());
        assert_eq!(state.queued, ["second"]);
        assert_eq!(
            state.composer_draft, "typing the next one",
            "a returned steer must not wipe the draft"
        );
        reduce(&mut state, DesktopAction::SteerSent("late".to_owned()));
        reduce(
            &mut state,
            DesktopAction::ChatFailed {
                message: mycode_app::CHAT_CANCELLED.to_owned(),
                head: String::new(),
            },
        );
        assert!(state.pending_steers.is_empty());
    }

    #[test]
    fn a_finished_turn_promotes_its_live_meter_to_last_turn() {
        let mut state = open(Vec::new(), None);
        reduce(
            &mut state,
            DesktopAction::UsageSnapshot {
                model: "glm-5.3".to_owned(),
                input: 100,
                context: 12_000,
                context_cache: 11_800,
                output: 20,
                cache: Some(11_800),
                requests: 1,
                elapsed_ms: 40,
            },
        );
        assert!(state.live_turn.is_some());
        assert!(state.last_turn.is_none());
        assert_eq!(state.context_used, 12_000);
        assert_eq!(state.context_cache, 11_800);

        reduce(
            &mut state,
            DesktopAction::ChatDone {
                head: "evt-2".to_owned(),
                entry: user("evt-2", "reply"),
            },
        );
        let turn = state.last_turn.as_ref().expect("promoted stats");
        assert_eq!(turn.model, "glm-5.3");
        assert_eq!(turn.input, 100);
        assert_eq!(turn.output, 20);
        assert_eq!(turn.cache, Some(11_800));
        assert!(state.live_turn.is_none());
        // The meter is the latest prompt; the finished turn keeps it.
        assert_eq!(state.context_used, 12_000);
        assert_eq!(state.context_cache, 11_800);
    }

    #[test]
    fn a_new_session_inherits_the_open_chat_model() {
        let mut settings = crate::view_model::SettingsState::from_settings(
            &mycode_config::AppSettings::default(),
            1,
            Vec::new(),
        );
        settings.reasoning = Some("on".to_owned());
        let mut state = WorkspaceState {
            settings: Some(settings),
            selected_provider: Some("zai".to_owned()),
            selected_model: Some("glm-4.7".to_owned()),
            active: Some(mycode_app::ActiveConversation {
                session_id: "old".to_owned(),
                branch_id: "branch".to_owned(),
                head: "empty".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        reduce(
            &mut state,
            DesktopAction::SessionCreated(mycode_app::SessionSummary {
                session_id: "new".to_owned(),
                root_branch_id: "branch".to_owned(),
                title: String::new(),
                updated_at_ms: None,
                event_count: 0,
                active: false,
                corrupt: false,
            }),
        );
        let inherited = mycode_config::session_model(&state.session_models, "new").expect("pin");
        assert_eq!(inherited.provider, "zai");
        assert_eq!(inherited.model, "glm-4.7");
        assert_eq!(inherited.reasoning.as_deref(), Some("on"));
        let previous = mycode_config::session_model(&state.session_models, "old").expect("old");
        assert_eq!(previous.model, "glm-4.7");
    }

    #[test]
    fn manual_and_auto_compaction_show_the_summary_in_the_chat() {
        use mycode_app::protocol::{ConversationEntry, EntryKind};

        let mut state = WorkspaceState {
            active: Some(mycode_app::ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "branch".to_owned(),
                head: "e1".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        let text = mycode_app::display_summary_text("files: src/main.rs");
        let entry = ConversationEntry {
            event_id: "sum-1".to_owned(),
            kind: EntryKind::UserMessage,
            text: text.into(),
            call_id: None,
            thinking: String::new(),
        };
        // Both triggers emit this action with the written summary.
        reduce(&mut state, DesktopAction::SummaryShown(entry.clone()));
        reduce(&mut state, DesktopAction::SummaryShown(entry));
        let entries = &state.active.expect("open").entries;
        assert_eq!(entries.len(), 1);
        assert!(mycode_app::is_compaction_summary(&entries[0].text));
        assert!(mycode_app::summary_body(&entries[0].text).contains("src/main.rs"));
    }

    #[test]
    fn a_mid_turn_summary_lands_after_the_final_reply() {
        use mycode_app::protocol::{ConversationEntry, EntryKind};

        let tool = ConversationEntry {
            event_id: "call-1".to_owned(),
            kind: EntryKind::ToolCall,
            text: "grep  src".into(),
            call_id: Some("c1".into()),
            thinking: String::new(),
        };
        let result = ConversationEntry {
            event_id: "res-1".to_owned(),
            kind: EntryKind::ToolResult,
            text: "src/main.rs:1:fn main".into(),
            call_id: Some("c1".into()),
            thinking: String::new(),
        };
        let mut state = WorkspaceState {
            sending: true,
            active: Some(mycode_app::ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "branch".to_owned(),
                head: "e1".to_owned(),
                entries: vec![tool, result],
                older_before: None,
                streaming: Some(mycode_app::StreamingReply {
                    text: String::new(),
                    thinking: "weigh the matches".to_owned(),
                    status: String::new(),
                }),
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        let summary = ConversationEntry {
            event_id: "sum-1".to_owned(),
            kind: EntryKind::UserMessage,
            text: mycode_app::display_summary_text("files: src/main.rs").into(),
            call_id: None,
            thinking: String::new(),
        };
        reduce(&mut state, DesktopAction::SummaryShown(summary.clone()));
        let held = state.active.as_ref().expect("open");
        assert!(
            held.entries
                .iter()
                .all(|entry| !mycode_app::is_compaction_summary(&entry.text)),
            "the card must not sit above the still-streaming reply"
        );
        assert_eq!(
            state
                .pending_summary
                .as_ref()
                .map(|entry| entry.event_id.as_str()),
            Some("sum-1")
        );
        let reply = ConversationEntry {
            event_id: "reply-1".to_owned(),
            kind: EntryKind::AssistantMessage,
            text: "done".into(),
            call_id: None,
            thinking: "weigh the matches".to_owned(),
        };
        reduce(
            &mut state,
            DesktopAction::ChatDone {
                head: "reply-1".to_owned(),
                entry: reply,
            },
        );
        let entries = &state.active.expect("open").entries;
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].kind, EntryKind::ToolCall);
        assert_eq!(entries[1].kind, EntryKind::ToolResult);
        assert_eq!(entries[2].event_id, "reply-1");
        assert!(mycode_app::is_compaction_summary(&entries[3].text));
        assert!(state.pending_summary.is_none());
        assert!(!state.sending);
    }

    #[test]
    fn welcome_enter_holds_the_draft_until_the_new_session_opens() {
        let mut state = WorkspaceState {
            project_dir: Some("/tmp/app".to_owned()),
            composer_draft: "explain the build".to_owned(),
            ..WorkspaceState::default()
        };
        assert_eq!(
            super::composer_submit(&state, &state.composer_draft),
            super::ComposerSubmit::StartTask("explain the build".to_owned())
        );
        reduce(
            &mut state,
            DesktopAction::WelcomeSendHeld("explain the build".to_owned()),
        );
        assert_eq!(
            state.pending_welcome_send.as_deref(),
            Some("explain the build")
        );
        assert_eq!(state.composer_draft, "explain the build");
        reduce(
            &mut state,
            DesktopAction::SessionCreated(mycode_app::SessionSummary {
                session_id: "new".to_owned(),
                root_branch_id: "branch".to_owned(),
                title: String::new(),
                updated_at_ms: None,
                event_count: 0,
                active: false,
                corrupt: false,
            }),
        );
        assert_eq!(
            state.pending_welcome_send.as_deref(),
            Some("explain the build")
        );
        assert_eq!(state.composer_draft, "explain the build");
        assert!(state.active.is_some());
        reduce(
            &mut state,
            DesktopAction::ConversationOpened(ActiveConversation {
                session_id: "new".to_owned(),
                branch_id: "branch".to_owned(),
                head: "empty".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                context_used: 0,
                context_cache: 0,
            }),
        );
        assert_eq!(state.composer_draft, "explain the build");
        assert_eq!(
            state.pending_welcome_send.as_deref(),
            Some("explain the build")
        );
        assert_eq!(
            super::composer_submit(&state, &state.composer_draft),
            super::ComposerSubmit::Send
        );
        reduce(&mut state, DesktopAction::WelcomeSendConsumed);
        assert!(state.pending_welcome_send.is_none());
    }

    #[test]
    fn welcome_enter_without_a_folder_does_not_start_a_task() {
        let mut state = WorkspaceState {
            composer_draft: "hello".to_owned(),
            ..WorkspaceState::default()
        };
        assert_eq!(
            super::composer_submit(&state, "hello"),
            super::ComposerSubmit::NeedFolder
        );
        reduce(
            &mut state,
            DesktopAction::WelcomeSendHeld("hello".to_owned()),
        );
        assert!(state.pending_welcome_send.is_none());
        assert_eq!(state.composer_draft, "hello");
        let open = WorkspaceState {
            project_dir: Some("/tmp/app".to_owned()),
            composer_draft: "  ".to_owned(),
            active: Some(ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "br".to_owned(),
                head: "empty".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        assert_eq!(
            super::composer_submit(&open, "still here"),
            super::ComposerSubmit::Send
        );
        assert_eq!(
            super::composer_submit(
                &WorkspaceState {
                    project_dir: Some("/tmp/app".to_owned()),
                    ..WorkspaceState::default()
                },
                "   ",
            ),
            super::ComposerSubmit::Ignore
        );
    }

    #[test]
    fn chat_failed_head_updates_conversation_without_clearing_context() {
        use crate::view_model::StreamingReply;

        let mut state = WorkspaceState {
            context_used: 25_000,
            context_cache: 24_300,
            sending: true,
            active: Some(ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "br".to_owned(),
                head: "evt-old".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: Some(StreamingReply {
                    text: "PARTIAL_A".to_owned(),
                    thinking: String::new(),
                    status: "working".to_owned(),
                }),
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        reduce(
            &mut state,
            DesktopAction::ChatFailed {
                message: mycode_app::CHAT_CANCELLED.to_owned(),
                head: "evt-partial".to_owned(),
            },
        );
        let conversation = state.active.as_ref().expect("conversation");
        assert_eq!(conversation.head, "evt-partial");
        assert!(conversation.streaming.is_none());
        assert!(!state.sending);
        assert!(state.error.is_none());
        assert_eq!(state.context_used, 25_000);
        assert_eq!(state.context_cache, 24_300);

        reduce(
            &mut state,
            DesktopAction::ChatFailed {
                message: "provider down".to_owned(),
                head: String::new(),
            },
        );
        assert_eq!(
            state.active.as_ref().expect("conversation").head,
            "evt-partial"
        );
        assert_eq!(state.context_used, 25_000);
        assert_eq!(state.error.as_deref(), Some("provider down"));
    }

    #[test]
    fn steer_committed_appends_the_entry_and_moves_the_head() {
        let mut state = WorkspaceState {
            active: Some(ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "br".to_owned(),
                head: "evt-old".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        let entry = ConversationEntry {
            event_id: "evt-steer".to_owned(),
            kind: EntryKind::UserMessage,
            text: "keep going".into(),
            call_id: None,
            thinking: String::new(),
        };
        reduce(
            &mut state,
            DesktopAction::SteerCommitted {
                session_id: "other".to_owned(),
                head: "evt-ignored".to_owned(),
                entry: entry.clone(),
            },
        );
        assert_eq!(state.active.as_ref().expect("conversation").head, "evt-old");
        assert!(
            state
                .active
                .as_ref()
                .expect("conversation")
                .entries
                .is_empty()
        );
        reduce(
            &mut state,
            DesktopAction::SteerCommitted {
                session_id: "ses".to_owned(),
                head: "evt-steer".to_owned(),
                entry,
            },
        );
        let conversation = state.active.as_ref().expect("conversation");
        assert_eq!(conversation.head, "evt-steer");
        assert_eq!(conversation.entries.len(), 1);
        assert_eq!(conversation.entries[0].text.as_ref(), "keep going");
    }

    #[test]
    fn ending_a_turn_clears_pending_ask_and_subagent_detail_window() {
        let prompt = mycode_app::AskPrompt {
            question: "Pick one?".to_owned(),
            choices: Vec::new(),
            optional: false,
            multiple: false,
        };
        let mut state = open(Vec::new(), None);
        state.sending = true;
        reduce(
            &mut state,
            DesktopAction::AskRequested(vec![prompt.clone()]),
        );
        reduce(
            &mut state,
            DesktopAction::AskChoicePicked {
                index: 0,
                answer: "a".to_owned(),
            },
        );
        reduce(
            &mut state,
            DesktopAction::SubagentWindowChanged(Some("call-1".to_owned())),
        );
        reduce(
            &mut state,
            DesktopAction::ChatFailed {
                message: mycode_app::CHAT_CANCELLED.to_owned(),
                head: String::new(),
            },
        );
        assert!(state.pending_ask.is_none());
        assert!(state.ask_answers.is_empty());
        assert!(state.subagent_window.is_none());

        state.sending = true;
        reduce(&mut state, DesktopAction::AskRequested(vec![prompt]));
        reduce(
            &mut state,
            DesktopAction::SubagentWindowChanged(Some("call-2".to_owned())),
        );
        reduce(
            &mut state,
            DesktopAction::ChatDone {
                head: "evt-done".to_owned(),
                entry: user("evt-done", "done"),
            },
        );
        assert!(state.pending_ask.is_none());
        assert!(state.ask_answers.is_empty());
        assert!(state.subagent_window.is_none());
    }

    #[test]
    fn workspace_rename_strips_control_characters_for_durable_ui_state() {
        let ws = mycode_config::WorkspaceDef::generate("Default", Vec::new());
        let mut state = WorkspaceState {
            active_workspace: Some(ws.id.clone()),
            workspaces: vec![ws],
            ..WorkspaceState::default()
        };
        reduce(
            &mut state,
            DesktopAction::WorkspaceRenamed("  Line 1\nLine 2\t ".to_owned()),
        );
        assert_eq!(state.workspaces[0].name, "Line 1 Line 2");
        let ui_state = mycode_config::UiState {
            workspaces: state.workspaces.clone(),
            active_workspace: state.active_workspace.clone(),
            ..mycode_config::UiState::default()
        };
        assert!(ui_state.validate().is_ok());
    }
}
