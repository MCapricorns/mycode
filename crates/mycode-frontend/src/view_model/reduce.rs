//! Reducer internals: the pure transition functions behind
//! [`super::reduce`]. The `FrontendAction` mutations live here; the parent
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
    ActiveConversation, FrontendAction, MentionKind, ProxyTestState, TurnStats, UpdateState,
    WorkspaceState,
};

pub use self::models::{
    reasoning_levels_for, selected_model_supports_reasoning, selected_reasoning_levels,
    strongest_reasoning_level,
};

pub use self::composer::{ComposerSubmit, composer_submit, preferred_slash_index};
use self::composer::{parse_mention, slash_items};
pub use self::jobs::{PREPARING_ROW_PREFIX, is_preparing_row};
use self::jobs::{
    drop_live_job, drop_preparing_rows, refresh_subagent_status, retire_live_jobs, tool_preparing,
    tool_progress, tool_started,
};
use self::models::{
    apply_session_model, assign_fresh_session_model, ensure_model_selection, model_selected,
    provider_selected, remember_active_session_model, remember_session_model,
    restore_live_reasoning,
};
use self::projects::{
    bind_session_project, session_bindings_forgotten, session_project_bound, workspace_root_added,
    workspace_root_removed,
};
use self::settings::edit_settings;
use self::streaming::{append_streaming, set_streaming_status};
use self::usage::{arm_cache_watch, clear_session_usage, observe_cache_hit};

mod turn_state;
use self::turn_state::*;

/// Applies one action to the state.
pub fn reduce(state: &mut WorkspaceState, action: FrontendAction) {
    // A paged-back window stays readable through ordinary UI actions. Only
    // new tail activity (or opening a new snapshot) applies the live ceiling.
    let trim_window = matches!(
        action,
        FrontendAction::TurnArmed
            | FrontendAction::MessageSent { .. }
            | FrontendAction::SteerCommitted { .. }
            | FrontendAction::AssistantStepCommitted(_)
            | FrontendAction::ToolPreparing { .. }
            | FrontendAction::ToolStarted { .. }
            | FrontendAction::ToolResultAppended(_)
            | FrontendAction::ChatDone { .. }
            | FrontendAction::ChatFailed { .. }
            | FrontendAction::SummaryShown(_)
            | FrontendAction::ConversationOpened(_)
    );
    let touches_providers = matches!(
        action,
        FrontendAction::SettingsLoaded(_)
            | FrontendAction::SettingsSaved { .. }
            | FrontendAction::UiStateLoaded { .. }
    );
    // Streaming ticks arrive many times a second and change no catalog the
    // `/` menu lists; rebuilding it for each one is wasted work.
    let streaming_tick = matches!(
        action,
        FrontendAction::ChatDelta(_)
            | FrontendAction::ChatThinkingDelta(_)
            | FrontendAction::ToolProgress { .. }
            | FrontendAction::UsageSnapshot { .. }
    );
    match action {
        FrontendAction::SessionsLoaded(mut sessions) => {
            let active_id = state.active.as_ref().map(|c| c.session_id.as_str());
            for session in &mut sessions {
                session.active = active_id == Some(session.session_id.as_str());
            }
            state.sessions = sessions;
        }
        FrontendAction::SessionTitled { session_id, title } => {
            if let Some(row) = state
                .sessions
                .iter_mut()
                .find(|row| row.session_id == session_id)
            {
                row.title = title;
            }
        }
        FrontendAction::SessionCreated(mut summary) => {
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
            // inherit its in-flight flag, steers, or asks.
            reset_turn_view(state);
            state.active = Some(ActiveConversation {
                session_id: summary.session_id,
                branch_id: summary.root_branch_id,
                head: "empty".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                running: false,
                compacting: false,
                context_used: 0,
                context_cache: 0,
            });
            remember_session_model(state, &session_id);
            // The composer meter reads these fields directly; a fresh session
            // starts with an empty meter.
            clear_session_usage(state);
        }
        FrontendAction::SessionDeleted => {
            state.active = None;
            reset_turn_view(state);
            state.error = None;
            state.pending_welcome_send = None;
            clear_session_usage(state);
            restore_live_reasoning(state);
        }
        FrontendAction::ConversationParked => {
            state.active = None;
            for session in &mut state.sessions {
                session.active = false;
            }
            reset_turn_view(state);
            restore_live_reasoning(state);
            state.composer_draft.clear();
            state.mention = None;
            state.pending_welcome_send = None;
            state.resources.clear();
            clear_session_usage(state);
        }
        FrontendAction::ConversationOpened(conversation) => {
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
                // Steers, tasks, and asks belong to the previous session.
                reset_turn_view(state);
            }
            let session_id = conversation.session_id.clone();
            let restored_used = conversation.context_used;
            let restored_cache = conversation.context_cache;
            // A reopen can land while the session's turn is still in flight:
            // the live flag keeps running affordances honest, and only then
            // do turn events keep flowing into this view. Reopening the
            // session already on screen mid-turn, or just after a send armed
            // the next turn, keeps the streaming buffer the wire copy never
            // carries.
            let running = conversation.running;
            let compacting = conversation.compacting;
            let mut conversation = conversation;
            if !switched
                && (running || state.sending)
                && conversation.streaming.is_none()
                && let Some(previous) = state.active.as_mut()
            {
                conversation.streaming = previous.streaming.take();
            }
            state.active = Some(conversation);
            // For the conversation already on screen, the flag may lag a
            // just-armed turn by a few async hops; only the fresh-open case
            // trusts the wire value outright.
            state.sending = running || (!switched && state.sending);
            // A `/compact` in flight ends with `CompactFinished`, never a
            // chat terminal, so it is restored as compacting, not sending.
            state.compacting = compacting || (!switched && state.compacting);
            if state.sending && state.turn_started_ms == 0 {
                // The wire does not carry the real turn start; count from
                // the reopen so the elapsed label still ticks.
                state.turn_started_ms = crate::unix_now().as_millis() as u64;
            }
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
        FrontendAction::ComposerChanged(text) => {
            let bounded = bounded_draft(text);
            state.mention = parse_mention(&bounded);
            state.composer_draft = bounded;
        }
        FrontendAction::WelcomeSendHeld(text) => {
            if state.pending_welcome_send.is_none()
                && let ComposerSubmit::StartTask(text) = composer_submit(state, &text)
            {
                let bounded = bounded_draft(text);
                if !bounded.trim().is_empty() {
                    state.pending_welcome_send = Some(bounded);
                }
            }
        }
        FrontendAction::WelcomeSendConsumed => {
            state.pending_welcome_send = None;
        }
        FrontendAction::MentionFiles(files) => {
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
        FrontendAction::SteerSent(text) => {
            state.pending_steers.push(text);
            state.composer_draft.clear();
            state.mention = None;
        }
        FrontendAction::SteerRetracted(text) => {
            if let Some(position) = state.pending_steers.iter().position(|sent| *sent == text) {
                state.pending_steers.remove(position);
            }
        }
        FrontendAction::SteersParked { session_id, texts } => {
            park_steers(state, session_id, texts);
        }
        FrontendAction::SteersReturned(texts) => {
            // The driver puts them back in the composer; here they only
            // leave the pending strip.
            for text in texts {
                if let Some(position) = state.pending_steers.iter().position(|sent| *sent == text) {
                    state.pending_steers.remove(position);
                }
            }
        }
        FrontendAction::SubagentWindowChanged(call_id) => {
            state.subagent_window = call_id;
        }
        FrontendAction::ChangesPanelToggled(open) => {
            state.changes_panel_open = open;
        }
        FrontendAction::SubagentDismissed(call_id) => {
            if drop_live_job(state, &call_id) && state.sending {
                refresh_subagent_status(state);
            }
        }
        FrontendAction::MessageSent { head, entry } => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.head = head;
                push_entry_once(conversation, entry);
            }
            state.composer_draft.clear();
            state.mention = None;
            state.turn_alert = None;
        }
        FrontendAction::TurnArmed => {
            state.sending = true;
            state.auto_compacting = false;
            state.turn_started_ms = crate::unix_now().as_millis() as u64;
            state.error = None;
            state.turn_alert = None;
            set_streaming_status(
                state,
                crate::i18n::t("Waiting for the model", "等待模型响应"),
            );
        }
        FrontendAction::ChatDelta(delta) => {
            // Events arriving after the turn's Done/Failed are stragglers
            // from work the view already closed; applying them would
            // resurrect a streaming bubble over a finished transcript.
            if state.sending {
                append_streaming(state, false, delta);
            }
        }
        FrontendAction::ToolPreparing {
            provider_call_id,
            name,
            target,
        } => {
            if state.sending {
                tool_preparing(state, &provider_call_id, &name, &target);
            }
        }
        FrontendAction::ToolStarted {
            call_id,
            provider_call_id,
            name,
            target,
        } => {
            if state.sending {
                tool_started(state, call_id, provider_call_id, name, target);
            }
        }
        FrontendAction::ToolProgress {
            call_id,
            provider_call_id,
            name,
            message,
        } => {
            // Same guard: a late progress line must not repopulate
            // `live_jobs` and the status line once the turn has ended.
            if state.sending {
                tool_progress(state, call_id, provider_call_id, name, message);
            }
        }
        FrontendAction::ToolResultAppended(entry) => {
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
        FrontendAction::ResourcesLoaded(files) => state.resources = files,
        FrontendAction::SkillsLoaded(skills) => state.skills = skills,
        FrontendAction::UsageSnapshot {
            model,
            input,
            context,
            context_cache,
            output,
            cache,
            requests,
            elapsed_ms,
        } => {
            if !state.sending {
                // A snapshot is per-turn; a straggler must not seed
                // `live_turn` after the turn already closed.
                return;
            }
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
        FrontendAction::AskRequested(rows) => {
            // A late ask after the turn closed must not reopen the modal.
            if !state.sending {
                return;
            }
            state.ask_answers = vec![String::new(); rows.len()];
            state.pending_ask = Some(rows);
        }
        FrontendAction::AskChoicePicked { index, answer } => {
            if let Some(slot) = state.ask_answers.get_mut(index) {
                *slot = answer;
            }
        }
        FrontendAction::AskAnswered => {
            state.pending_ask = None;
            state.ask_answers.clear();
        }
        FrontendAction::SessionFilterChanged(query) => state.session_filter = query,
        FrontendAction::HistoryLoadStarted => state.history_loading = true,
        FrontendAction::HistoryLoadFinished => state.history_loading = false,
        FrontendAction::OlderLoaded {
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
        FrontendAction::ChatThinkingDelta(delta) => {
            if state.sending {
                append_streaming(state, true, delta);
            }
        }
        FrontendAction::SummaryShown(entry) => {
            if summary_waits_for_the_reply(state) {
                if state
                    .pending_summaries
                    .iter()
                    .all(|pending| pending.event_id != entry.event_id)
                {
                    state.pending_summaries.push(entry);
                }
            } else if let Some(conversation) = state.active.as_mut() {
                push_entry_once(conversation, entry);
            }
        }
        FrontendAction::CompactArmed => {
            state.compacting = true;
            state.error = None;
        }
        FrontendAction::CompactFinished { ok: _, head } => {
            state.compacting = false;
            if !head.is_empty()
                && let Some(conversation) = state.active.as_mut()
            {
                conversation.head = head;
            }
        }
        FrontendAction::AssistantStepCommitted(entry) => {
            if let Some(conversation) = state.active.as_mut() {
                push_entry_once(conversation, entry);
            }
            // Status text and buffer clearing are turn-scoped; a straggler
            // step after the turn closed only contributes its entry.
            if !state.sending {
                return;
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
        FrontendAction::ChatDone { head, entry } => {
            // Steers that did not commit come back as `SteersReturned`.
            state.pending_steers.clear();
            let summaries = std::mem::take(&mut state.pending_summaries);
            if let Some(conversation) = state.active.as_mut() {
                conversation.head = head;
                // A turn that ended on a committed tool step reports that
                // step again as its last message; it is already listed.
                push_entry_once(conversation, entry);
                // The summaries follow the reply, matching the ledger order
                // a reopen projects.
                for summary in summaries {
                    push_entry_once(conversation, summary);
                }
                conversation.streaming = None;
            }
            end_turn(state);
            // The finished turn keeps its meter: the last live snapshot held
            // the turn's sums, so it becomes the completed-turn stats.
            state.last_turn = state.live_turn.take();
        }
        FrontendAction::ChatFailed { message, head } => {
            state.pending_steers.clear();
            let summaries = std::mem::take(&mut state.pending_summaries);
            if let Some(conversation) = state.active.as_mut() {
                if !head.is_empty() {
                    conversation.head = head;
                }
                for summary in summaries {
                    push_entry_once(conversation, summary);
                }
                conversation.streaming = None;
            }
            // A user-initiated cancel resets the turn without an error
            // banner; the sentinel travels as the failure message.
            // Context meters stay: a cancel is not a new empty session.
            // The transcript keeps a red row for any turn that closed
            // without a reply — provider failure or a user stop — so an
            // interrupted tail is not mistaken for a normal answer.
            state.turn_alert = Some(if message == CHAT_CANCELLED {
                crate::i18n::t("Stopped by user", "已被用户停止").to_owned()
            } else {
                message.clone()
            });
            if message != CHAT_CANCELLED {
                state.error = Some(message);
            }
            end_turn(state);
            state.live_turn = None;
        }
        FrontendAction::CacheBreakArmed(cause) => arm_cache_watch(state, cause),
        FrontendAction::ContextCompacting(active) => {
            if !state.sending {
                return;
            }
            state.auto_compacting = active;
            let status = if active {
                crate::i18n::t("Compacting context", "正在压缩上下文")
            } else {
                crate::i18n::t("Waiting for the model", "等待模型响应")
            };
            set_streaming_status(state, status);
        }
        FrontendAction::SteerCommitted {
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
        FrontendAction::Failed(message) => {
            state.error = Some(message);
        }
        FrontendAction::SettingsLoaded(settings) => settings::settings_loaded(state, *settings),
        FrontendAction::SettingsLanguageSelected(language) => {
            settings::language_selected(state, language);
        }
        FrontendAction::SettingsThemeSelected(theme) => settings::theme_selected(state, &theme),
        FrontendAction::SettingsFontSizeSelected(font_size) => {
            settings::font_size_selected(state, font_size);
        }
        FrontendAction::InspectorChanged { open, pinned } => {
            state.inspector_open = open;
            state.inspector_pinned = pinned;
        }
        FrontendAction::SettingsProxyChanged { value, .. } => {
            if state
                .settings
                .as_ref()
                .is_some_and(|settings| settings.proxy != value)
            {
                state.proxy_test = ProxyTestState::Idle;
            }
            edit_settings(state, |settings| {
                if settings.proxy == value {
                    return false;
                }
                settings.proxy = value;
                true
            });
        }
        FrontendAction::SettingsSubagentsChanged(subagents) => {
            edit_settings(state, |settings| {
                settings.subagents = subagents;
                true
            });
        }
        FrontendAction::SettingsSkillToggled(slug, enabled) => {
            edit_settings(state, |settings| {
                settings.skills.set_enabled(&slug, enabled)
            });
        }
        FrontendAction::SubagentMenuToggled(menu) => {
            let opening = menu.is_some();
            state.subagent_menu = menu;
            state.model_menu_browse = None;
            state.picker_query.clear();
            if opening {
                state.model_menu_open = false;
                state.reasoning_menu_open = false;
            }
        }
        FrontendAction::SettingsSaved {
            revision,
            edit_epoch,
        } => settings::settings_saved(state, revision, edit_epoch),
        FrontendAction::SettingsSaveFailed(message) => {
            if let Some(settings) = state.settings.as_mut() {
                settings.saving = false;
            }
            state.error = Some(message);
        }
        FrontendAction::ProviderKeySaved { mcp_keys } => {
            if let Some(settings) = state.settings.as_mut() {
                settings.mcp_with_keys = mcp_keys;
            }
        }
        FrontendAction::McpProbeStarted(server_id) => {
            if !state.mcp_probing.contains(&server_id) {
                state.mcp_probing.push(server_id);
            }
        }
        FrontendAction::McpToolsListed { server_id, tools } => {
            settings::mcp_tools_listed(state, server_id, tools);
        }
        FrontendAction::McpProbeFailed { server_id, message } => {
            state.mcp_probing.retain(|id| *id != server_id);
            state.mcp_tools.retain(|(id, _)| *id != server_id);
            state.error = Some(message);
        }
        FrontendAction::McpLibraryLoaded { servers, error } => {
            // The magpie Library file is the only MCP catalog: the settings
            // projection mirrors it so the page, the nav count and the `/`
            // menu read one list. It is read-only — never `dirty`.
            if let Some(settings) = state.settings.as_mut() {
                settings.mcp_servers = servers.clone();
            }
            state.mcp_library = servers;
            state.mcp_library_error = error;
        }
        FrontendAction::ShowMainView(view) => {
            state.view = view;
            // Crossing views closes every floating menu so no stale layer
            // renders above the destination view.
            close_floating_menus(state);
        }
        FrontendAction::ShowSettingsSection(section) => {
            state.model_menu_open = false;
            state.model_menu_browse = None;
            state.picker_query.clear();
            state.subagent_menu = None;
            state.settings_section = section;
        }
        FrontendAction::ProjectMenuToggled(open) => state.project_menu_open = open,
        FrontendAction::CatalogLoaded {
            document,
            fetched_at,
        } => {
            state.catalog = Some(document);
            state.catalog_fetched_at = fetched_at;
        }
        FrontendAction::UiStateLoaded {
            recents,
            last_project,
            auto_update,
            selected_provider,
            selected_model,
            selected_reasoning,
            session_projects,
            session_models,
            workspace_roots,
            recent_models,
            thinking_collapsed,
            inspector_open,
            inspector_pinned,
            window_size,
        } => {
            state.thinking_collapsed = thinking_collapsed;
            state.inspector_open = inspector_open;
            state.inspector_pinned = inspector_pinned;
            state.window_size = state.window_size.or(window_size);
            state.recents = recents;
            state.project_dir = last_project.filter(|path| !path.trim().is_empty());
            state.session_projects = session_projects;
            state.session_models = session_models;
            state.workspace_roots = workspace_roots;
            state.recent_models = recent_models;
            state.default_reasoning = selected_reasoning;
            state.auto_update = auto_update;
            if selected_provider.is_some() {
                state.selected_provider = selected_provider;
                state.selected_model = selected_model;
            }
            ensure_model_selection(state);
            // The live effort is an overlay on the document: re-apply the
            // open session's pin, else the composer's last pick. Without
            // it a remembered pick stays in ui.json while the chip and the
            // next session's pin fall back to the provider default.
            restore_live_reasoning(state);
            if let Some(session_id) = state
                .active
                .as_ref()
                .map(|active| active.session_id.clone())
                && !apply_session_model(state, &session_id)
            {
                remember_session_model(state, &session_id);
            }
        }
        FrontendAction::ThinkingCollapsedToggled => {
            state.thinking_collapsed = !state.thinking_collapsed;
        }
        FrontendAction::SessionBindingsForgotten(session_id) => {
            session_bindings_forgotten(state, session_id)
        }
        FrontendAction::WorkspaceRootAdded(project) => workspace_root_added(state, project),
        FrontendAction::WorkspaceRootRemoved(project) => {
            workspace_root_removed(state, project);
        }
        FrontendAction::ProjectOpened(project) => {
            state.project_dir = Some(project.clone());
            state.recents.retain(|existing| existing != &project);
            state.recents.insert(0, project);
            state.recents.truncate(mycode_app::MAX_RECENT_PROJECTS);
        }
        FrontendAction::SessionProjectBound {
            session_id,
            project,
        } => session_project_bound(state, session_id, project),
        FrontendAction::WorkspaceFolderFocused {
            session_id,
            project,
        } => {
            if project.trim().is_empty() {
                return;
            }
            state.project_dir = Some(project.clone());
            bind_session_project(state, session_id, project);
        }
        FrontendAction::ActiveProjectChanged(project) => {
            state.project_dir = project;
            if !super::task_surface_visible(state) {
                state.subagent_window = None;
            }
        }
        FrontendAction::RecentRemoved(project) => {
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
        FrontendAction::ProviderSelected(provider) => provider_selected(state, provider),
        FrontendAction::ModelSelected(model) => model_selected(state, model),
        FrontendAction::ModelMenuToggled(open) => {
            state.model_menu_open = open;
            state.model_menu_browse = None;
            state.picker_query.clear();
            if open {
                state.reasoning_menu_open = false;
                state.subagent_menu = None;
            }
        }
        FrontendAction::ModelMenuBrowse(provider) => {
            state.reasoning_menu_open = false;
            state.model_menu_browse = provider;
            state.picker_query.clear();
        }
        FrontendAction::PickerQueryChanged(query) => state.picker_query = query,
        FrontendAction::SettingsQueryChanged(query) => state.settings_query = query,
        FrontendAction::ReasoningMenuToggled(open) => {
            state.reasoning_menu_open = open;
            if open {
                state.model_menu_open = false;
            }
        }
        FrontendAction::SettingsReasoningChanged(level) => {
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
        FrontendAction::ShowModelsSubview(view) => {
            state.models_subview = view;
        }
        FrontendAction::LanguageMenuToggled(open) => state.language_menu_open = open,
        FrontendAction::UpdateStateChanged(update) => state.update = update,
        FrontendAction::ProxyTestChanged(test) => state.proxy_test = test,
        FrontendAction::UpdateDialogToggled(open) => state.update_dialog_open = open,
        FrontendAction::CommandPaletteSet(open) => {
            state.command_palette_open = open;
            if open {
                close_floating_menus(state);
            }
        }
        FrontendAction::UpdateOfferFound(offer) => {
            // The offer and the Available flip are one transition: the
            // download button's handler reads `last_offer`, so an
            // Available state without a published offer is a dead button.
            state.update = UpdateState::Available {
                version: offer.version.clone(),
                notes_url: offer.notes_url.clone(),
            };
            state.last_offer = Some(offer);
        }
        FrontendAction::AutoUpdateToggled(auto_update) => state.auto_update = auto_update,
        FrontendAction::UpdateStaged(prepared) => {
            let version = prepared.version.clone();
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
    let Some(mention) = state
        .mention
        .as_ref()
        .filter(|mention| mention.kind == MentionKind::Command)
    else {
        return;
    };
    // Built from borrows, then stored: this runs on every keystroke while
    // the menu is open, so the server and tool lists are not copied.
    let servers = state
        .settings
        .as_ref()
        .map_or(&[][..], |settings| settings.mcp_servers.as_slice());
    let items = slash_items(
        &mention.fragment,
        &state.enabled_skills(),
        servers,
        &state.mcp_library,
        &state.mcp_tools,
    );
    if let Some(mention) = state.mention.as_mut() {
        mention.items = items;
    }
}

/// Closes every floating menu layer, whatever view it belongs to. Returns
/// whether anything was open, so Escape can tell a dismissal from a no-op.
pub fn close_floating_menus(state: &mut WorkspaceState) -> bool {
    let was_open = state.project_menu_open
        || state.model_menu_open
        || state.reasoning_menu_open
        || state.subagent_menu.is_some()
        || state.language_menu_open
        || state.mention.is_some();
    state.project_menu_open = false;
    state.model_menu_open = false;
    state.model_menu_browse = None;
    state.picker_query.clear();
    state.reasoning_menu_open = false;
    state.subagent_menu = None;
    state.language_menu_open = false;
    state.mention = None;
    was_open
}

#[cfg(test)]
mod tests;
