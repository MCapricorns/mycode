//! Conversation lifetime: open, create, park, delete, the rewind page-walk,
//! older-history paging, and the durable UI state that follows them.
use mycode_app::{
    ActiveConversation, BranchId, BridgeCommand, BridgeReply, SessionEventId, SessionId,
};

use super::{Effect, FrontendDriver, RecallRequest, ReplyScope, ToastKind, parse_head};
use crate::view_model::{ConversationEntry, EntryKind, FrontendAction};

/// The last committed ledger event strictly before `entries[index]`.
/// Transient tool placeholders have no parseable event id and are skipped.
fn previous_event(entries: &[ConversationEntry], index: usize) -> Option<&str> {
    entries
        .get(..index)?
        .iter()
        .rev()
        .find_map(|entry| SessionEventId::parse(&entry.event_id).map(|_| entry.event_id.as_str()))
}

fn can_rewind(conversation: &ActiveConversation, index: usize) -> bool {
    conversation.entries.get(index).is_some_and(|entry| {
        entry.kind == EntryKind::UserMessage
            && (previous_event(&conversation.entries, index).is_some()
                || conversation.older_before.is_some())
    })
}

impl FrontendDriver {
    /// Persists the durable UI state projection.
    pub fn persist_ui_state(&mut self) {
        let state = mycode_config::UiState {
            recent_projects: self.vm.recents.clone(),
            last_project: self.vm.project_dir.clone(),
            auto_update: self.vm.auto_update,
            selected_provider: self.vm.selected_provider.clone(),
            selected_model: self.vm.selected_model.clone(),
            selected_reasoning: self.vm.default_reasoning.clone(),
            session_projects: self.vm.session_projects.clone(),
            session_models: self.vm.session_models.clone(),
            workspace_roots: self.vm.workspace_roots.clone(),
            workspaces: Vec::new(),
            session_workspaces: Vec::new(),
            active_workspace: None,
            recent_models: self.vm.recent_models.clone(),
            thinking_collapsed: self.vm.thinking_collapsed,
            inspector_open: self.vm.inspector_open,
            inspector_pinned: self.vm.inspector_pinned,
            window_size: self.vm.window_size,
        };
        self.dispatch(BridgeCommand::SaveUiState { state });
    }

    /// Starts a fresh task (the New-task affordance). `false` when no folder
    /// is open — the only case that drops a held welcome draft.
    pub fn new_session(&mut self, out: &mut Vec<Effect>) -> bool {
        // A session needs a folder. The empty desk and sidebar hide this
        // action until one is open; the slash command uses the same gate.
        if !crate::view_model::has_open_folder(&self.vm) {
            return false;
        }
        // The welcome desk stays up while the open chat has no messages.
        // New task is already that chat. Another CreateSession lists a
        // second empty row.
        if crate::view_model::open_session_is_empty(&self.vm) {
            return true;
        }
        // A newer intent: drop whatever open is still in flight so the
        // created session opens instead of a reply landing for the old one.
        self.pending_open = None;
        // New sessions inherit the active project so the sidebar grouping
        // and the tool working directory follow the project switcher.
        if self.vm.project_dir.is_some() {
            self.pending_project = self.vm.project_dir.clone();
        }
        self.dispatch(BridgeCommand::CreateSession {
            project: self.pending_project.clone(),
        });
        let _ = out;
        true
    }

    /// Opens one session and ignores any conversation reply that is not it.
    pub(crate) fn request_open_session(&mut self, session_id: &str) {
        let Some(parsed) = SessionId::parse(session_id) else {
            return;
        };
        self.pending_open = Some(session_id.to_owned());
        self.dispatch(BridgeCommand::OpenSession(parsed));
    }

    /// Opens one session from the sidebar: clears a stale project filter,
    /// rebinds the working directory, then opens the conversation.
    pub fn open_session(&mut self, session_id: &str, out: &mut Vec<Effect>) {
        if crate::view_model::project_of_session(&self.vm.session_projects, session_id).is_none() {
            self.act(FrontendAction::ActiveProjectChanged(None), out);
        }
        self.follow_session_project(session_id, out);
        self.request_open_session(session_id);
    }

    /// Makes the sidebar and tool directory follow one session's project.
    /// Other chats keep their own bindings, so several projects can stay open.
    pub(crate) fn follow_session_project(&mut self, session_id: &str, out: &mut Vec<Effect>) {
        let Some(project) = self
            .vm
            .session_projects
            .iter()
            .find(|(id, _)| id == session_id)
            .map(|(_, project)| project.clone())
        else {
            return;
        };
        self.dispatch(BridgeCommand::SetProjectDir {
            session_id: session_id.to_owned(),
            path: Some(project.clone()),
        });
        self.act(FrontendAction::WorkspaceRootAdded(project.clone()), out);
        let already = self
            .vm
            .project_dir
            .as_deref()
            .is_some_and(|current| crate::view_model::same_project_path(current, &project));
        if already {
            self.persist_ui_state();
            return;
        }
        self.act(FrontendAction::ActiveProjectChanged(Some(project)), out);
        self.refresh_skills(out);
        self.persist_ui_state();
    }

    /// Deletes one session. The reply is scoped by id so a stale delete
    /// cannot wipe a conversation the user opened meanwhile.
    pub fn delete_session(&mut self, session_id: &str) {
        self.dispatch_scoped(
            BridgeCommand::DeleteSession {
                session_id: session_id.to_owned(),
            },
            ReplyScope::DeletedSession(session_id.to_owned()),
        );
    }

    /// Whether the message at `index` can be rewound to: a committed user
    /// message with an earlier event resident or older history to walk to.
    pub fn can_rewind_message(&self, index: usize) -> bool {
        !self.vm.sending
            && self
                .vm
                .active
                .as_ref()
                .is_some_and(|active| can_rewind(active, index))
    }

    /// Rewinds to before this message and restores its text to the composer.
    pub fn recall_message(&mut self, index: usize, out: &mut Vec<Effect>) {
        if !self.can_rewind_message(index) {
            return;
        }
        let active = self.vm.active.as_ref().expect("rewindable conversation");
        let (Some(session), Some(branch)) = (
            SessionId::parse(&active.session_id),
            BranchId::parse(&active.branch_id),
        ) else {
            return;
        };
        self.recall_generation = self.recall_generation.wrapping_add(1);
        let request = RecallRequest {
            session,
            branch,
            head: active.head.clone(),
            edit: active.entries[index].text.to_string(),
            generation: self.recall_generation,
        };
        if let Some(target) = previous_event(&active.entries, index).map(str::to_owned) {
            self.finish_recall(request, target);
        } else if let Some(before) = active.older_before.clone() {
            self.resolve_recall_page(request, before, out);
        }
    }

    /// A resident-window boundary is not a branch boundary. Resolve the
    /// preceding committed event without prepending or disturbing the viewport.
    pub(crate) fn resolve_recall_page(
        &mut self,
        request: RecallRequest,
        before: String,
        _out: &mut Vec<Effect>,
    ) {
        self.dispatch_scoped(
            BridgeCommand::LoadOlder {
                session: request.session.clone(),
                branch: request.branch.clone(),
                expected_head: parse_head(&request.head),
                before,
            },
            ReplyScope::Recall(Box::new(request)),
        );
    }

    /// One `LoadOlder` page answering a recall walk: find the event before
    /// the rewind target in this page or keep walking.
    pub(crate) fn resolve_recall_reply(
        &mut self,
        request: RecallRequest,
        reply: BridgeReply,
        out: &mut Vec<Effect>,
    ) {
        let current = self.recall_generation == request.generation
            && !self.vm.sending
            && self.vm.active.as_ref().is_some_and(|active| {
                active.session_id == request.session.as_str()
                    && active.branch_id == request.branch.as_str()
                    && active.head == request.head
            });
        if !current {
            return;
        }
        match reply {
            BridgeReply::Older(Ok(page)) => {
                if let Some(target) = previous_event(&page.entries, page.entries.len()) {
                    self.finish_recall(request, target.to_owned());
                } else if let Some(older) =
                    page.older.filter(|older| older != &page.requested_before)
                {
                    self.resolve_recall_page(request, older, out);
                } else {
                    self.toast(
                        crate::i18n::t(
                            "The first message has no earlier event to recall to.",
                            "第一条消息之前没有可以回退到的位置。",
                        ),
                        ToastKind::Info,
                        out,
                    );
                }
            }
            BridgeReply::Older(Err(message)) => self.act(FrontendAction::Failed(message), out),
            _ => {}
        }
    }

    fn finish_recall(&mut self, request: RecallRequest, to_event: String) {
        self.dispatch(BridgeCommand::RecallMessage {
            session: request.session,
            branch: request.branch,
            expected_head: parse_head(&request.head),
            to_event,
            edit: Some(request.edit),
        });
    }

    /// Loads the next older page when the viewport is near the top.
    /// `scroll_hold_pending` (reported via `note_scroll_hold`) means a page
    /// is still anchored; starting another read measures the wrong height.
    pub fn request_older(&mut self, out: &mut Vec<Effect>) {
        if self.vm.sending || self.vm.history_loading || self.scroll_hold_pending {
            return;
        }
        let Some(before) = self
            .vm
            .active
            .as_ref()
            .and_then(|active| active.older_before.clone())
        else {
            return;
        };
        let Some((session, branch, expected_head)) = self.open_turn_target() else {
            return;
        };
        self.act(FrontendAction::HistoryLoadStarted, out);
        self.dispatch(BridgeCommand::LoadOlder {
            session,
            branch,
            expected_head,
            before,
        });
    }

    /// Restores each chat's working directory after startup so a later
    /// session switch does not run tools in the last-opened project.
    pub(crate) fn restore_session_projects(&mut self) {
        let bindings = self.vm.session_projects.clone();
        for (session_id, project) in bindings {
            self.dispatch(BridgeCommand::SetProjectDir {
                session_id,
                path: Some(project),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{can_rewind, previous_event};
    use crate::view_model::{ConversationEntry, EntryKind};
    use mycode_app::ActiveConversation;

    fn entry(id: &str, kind: EntryKind) -> ConversationEntry {
        ConversationEntry {
            event_id: id.to_owned(),
            kind,
            text: "text".into(),
            call_id: None,
            thinking: String::new(),
        }
    }

    #[test]
    fn rewind_skips_cancelled_live_tool_placeholders() {
        let entries = [
            entry(
                "evt1-00000000000000000000000000000001",
                EntryKind::AssistantMessage,
            ),
            entry("call-live", EntryKind::ToolCall),
            entry(
                "evt1-00000000000000000000000000000002",
                EntryKind::UserMessage,
            ),
        ];
        assert_eq!(
            previous_event(&entries, 2),
            Some(entries[0].event_id.as_str())
        );
        assert_eq!(previous_event(&entries, 0), None);
        assert_eq!(previous_event(&entries, 8), None);
    }

    #[test]
    fn first_resident_user_message_can_resolve_older_history() {
        let mut active = ActiveConversation {
            session_id: "session".to_owned(),
            branch_id: "branch".to_owned(),
            head: "empty".to_owned(),
            entries: vec![entry(
                "evt1-00000000000000000000000000000002",
                EntryKind::UserMessage,
            )],
            older_before: Some("evt1-00000000000000000000000000000002".to_owned()),
            streaming: None,
            running: false,
            context_used: 0,
            context_cache: 0,
        };
        assert!(can_rewind(&active, 0));
        active.older_before = None;
        assert!(!can_rewind(&active, 0));
        assert!(!can_rewind(&active, 1));
    }
}
