//! Project- and workspace-shaped projections: path comparison, session
//! bindings, and the active-workspace view of the sidebar.

use std::collections::HashMap;

use mycode_app::SessionSummary;
pub(crate) use mycode_config::same_project_path;

use super::state::WorkspaceState;

/// Short display label for a project folder path (its trailing component, or
/// the full path when it has none).
#[must_use]
pub(crate) fn project_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_owned())
}

/// Pre-indexed session-to-workspace and session-to-project lookup tables so
/// callers that iterate over all sessions (`workspace_session_rows`,
/// `workspace_session_counts`, `command_palette`) run in linear time instead
/// of scanning the binding vectors per session.
pub(crate) struct SessionBindingsIndex<'a> {
    first_workspace: Option<&'a mycode_config::WorkspaceDef>,
    known_workspaces: HashMap<&'a str, &'a mycode_config::WorkspaceDef>,
    owners: HashMap<&'a str, &'a str>,
    projects: HashMap<&'a str, &'a str>,
}

impl<'a> SessionBindingsIndex<'a> {
    #[must_use]
    pub(crate) fn new(state: &'a WorkspaceState) -> Self {
        let first_workspace = state.workspaces.first();
        let known_workspaces: HashMap<&'a str, &'a mycode_config::WorkspaceDef> = state
            .workspaces
            .iter()
            .map(|workspace| (workspace.id.as_str(), workspace))
            .collect();
        let mut owners: HashMap<&'a str, &'a str> =
            HashMap::with_capacity(state.session_workspaces.len());
        for (session, workspace) in &state.session_workspaces {
            owners.entry(session.as_str()).or_insert(workspace.as_str());
        }
        let mut projects: HashMap<&'a str, &'a str> =
            HashMap::with_capacity(state.session_projects.len());
        for (session, project) in &state.session_projects {
            projects.entry(session.as_str()).or_insert(project.as_str());
        }
        Self {
            first_workspace,
            known_workspaces,
            owners,
            projects,
        }
    }

    /// The workspace `session_id` belongs to, following [`workspace_of_session`].
    #[must_use]
    pub(crate) fn workspace_of(&self, session_id: &str) -> Option<&'a mycode_config::WorkspaceDef> {
        self.owners
            .get(session_id)
            .and_then(|id| self.known_workspaces.get(id).copied())
            .or(self.first_workspace)
    }

    /// The project path bound to `session_id`, if any.
    #[must_use]
    pub(crate) fn project_of(&self, session_id: &str) -> Option<&'a str> {
        self.projects.get(session_id).copied()
    }
}

/// The workspace the sidebar shows. A stale active id falls back to the
/// first workspace, which is also where unbound sessions live.
#[must_use]
pub fn active_workspace(state: &WorkspaceState) -> Option<&mycode_config::WorkspaceDef> {
    match state.active_workspace.as_deref() {
        Some(id) => state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .or_else(|| state.workspaces.first()),
        None => state.workspaces.first(),
    }
}

/// The workspace a session belongs to; a session without a binding predates
/// named workspaces and belongs to the first one. A binding naming a removed
/// workspace is treated as unbound — the session stays visible in the first
/// workspace instead of hiding from every sidebar, and can be rebound there.
#[must_use]
pub fn workspace_of_session<'a>(
    state: &'a WorkspaceState,
    session_id: &str,
) -> Option<&'a mycode_config::WorkspaceDef> {
    let bound = state
        .session_workspaces
        .iter()
        .find(|(existing, _)| existing == session_id)
        .map(|(_, workspace)| workspace.as_str());
    match bound {
        Some(id) => state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .or_else(|| state.workspaces.first()),
        None => state.workspaces.first(),
    }
}

/// The sessions `workspace_id` lists, in stored order, each with its
/// project binding. Ownership follows [`workspace_of_session`]; the
/// membership and binding tables are indexed once instead of scanned per
/// session, which made the sidebar quadratic in the session count.
#[must_use]
pub(crate) fn workspace_session_rows<'a>(
    state: &'a WorkspaceState,
    workspace_id: &str,
) -> Vec<(&'a SessionSummary, Option<&'a str>)> {
    if state.workspaces.is_empty() {
        return Vec::new();
    }
    let index = SessionBindingsIndex::new(state);
    state
        .sessions
        .iter()
        .filter(|session| {
            index
                .workspace_of(&session.session_id)
                .is_some_and(|owner| owner.id == workspace_id)
        })
        .map(|session| (session, index.project_of(&session.session_id)))
        .collect()
}

/// Counts how many sessions belong to each workspace in a single pass.
/// Ownership follows [`workspace_of_session`], so unbound or stale-bound
/// sessions are counted under the first workspace.
#[must_use]
pub(crate) fn workspace_session_counts(state: &WorkspaceState) -> HashMap<&str, usize> {
    let mut counts: HashMap<&str, usize> = HashMap::with_capacity(state.workspaces.len());
    if state.workspaces.is_empty() {
        return counts;
    }
    let index = SessionBindingsIndex::new(state);
    for session in &state.sessions {
        if let Some(owner) = index.workspace_of(&session.session_id) {
            *counts.entry(owner.id.as_str()).or_insert(0) += 1;
        }
    }
    counts
}

/// Task cards belong to the open session's project. Leaving that project
/// hides them instead of leaving another project's work on screen.
#[must_use]
pub(crate) fn task_surface_visible(state: &WorkspaceState) -> bool {
    let Some(session_id) = state
        .active
        .as_ref()
        .map(|conversation| conversation.session_id.as_str())
    else {
        return false;
    };
    let Some(project) = state.project_dir.as_deref() else {
        return false;
    };
    state
        .session_projects
        .iter()
        .any(|(id, bound)| id == session_id && same_project_path(bound, project))
}

/// The folder bound to one session, if it has one.
#[must_use]
pub(crate) fn project_of_session<'a>(
    bindings: &'a [(String, String)],
    session_id: &str,
) -> Option<&'a str> {
    bindings
        .iter()
        .find(|(id, _)| id == session_id)
        .map(|(_, project)| project.as_str())
}

/// Whether a folder is open, so a new session has a working directory.
///
/// A named workspace with an empty folder list does not count. Fresh launch
/// always has that workspace and still must not offer a second action.
#[must_use]
pub fn has_open_folder(state: &WorkspaceState) -> bool {
    path_open(state.project_dir.as_deref())
        || state
            .workspace_roots
            .iter()
            .any(|path| path_open(Some(path.as_str())))
}

fn path_open(path: Option<&str>) -> bool {
    path.is_some_and(|path| !path.trim().is_empty())
}

/// The open chat is already a blank task.
///
/// New task (welcome, sidebar, and `/new`) should stay on it. Creating
/// another session lists a second empty row that looks like a copy. A turn
/// in flight, a loaded message, or history still on disk is a real
/// conversation and still needs its own session.
#[must_use]
pub fn open_session_is_empty(state: &WorkspaceState) -> bool {
    if state.sending {
        return false;
    }
    state.active.as_ref().is_some_and(|conversation| {
        conversation.entries.is_empty()
            && conversation.older_before.is_none()
            && conversation.streaming.is_none()
    })
}

/// Newest session already bound to `project`. `sessions` is newest-first.
#[must_use]
pub(crate) fn newest_session_in_project<'a>(
    sessions: &'a [SessionSummary],
    bindings: &[(String, String)],
    project: &str,
) -> Option<&'a str> {
    sessions.iter().find_map(|session| {
        let bound = project_of_session(bindings, &session.session_id)?;
        same_project_path(bound, project).then_some(session.session_id.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::{has_open_folder, open_session_is_empty};
    use crate::view_model::WorkspaceState;

    #[test]
    fn fresh_workspace_without_a_folder_stays_closed() {
        let state = WorkspaceState {
            workspaces: vec![mycode_config::WorkspaceDef {
                id: "ws".to_owned(),
                name: "Default".to_owned(),
                folders: Vec::new(),
            }],
            active_workspace: Some("ws".to_owned()),
            project_dir: Some("   ".to_owned()),
            workspace_roots: vec![" ".to_owned()],
            ..WorkspaceState::default()
        };
        assert!(!has_open_folder(&state));
    }

    #[test]
    fn workspace_rows_match_workspace_of_session() {
        let workspace = |id: &str| mycode_config::WorkspaceDef {
            id: id.to_owned(),
            name: id.to_owned(),
            folders: Vec::new(),
        };
        let session = |id: &str| crate::view_model::SessionSummary {
            session_id: id.to_owned(),
            root_branch_id: "br".to_owned(),
            title: String::new(),
            updated_at_ms: None,
            event_count: 1,
            active: false,
            corrupt: false,
        };
        let pair = |left: &str, right: &str| (left.to_owned(), right.to_owned());
        let state = WorkspaceState {
            workspaces: vec![workspace("first"), workspace("second")],
            sessions: ["unbound", "in-second", "stale", "in-first"]
                .into_iter()
                .map(session)
                .collect(),
            session_workspaces: vec![
                pair("in-second", "second"),
                pair("stale", "removed"),
                pair("in-first", "first"),
                pair("in-first", "second"),
            ],
            session_projects: vec![pair("in-first", "/a"), pair("in-first", "/b")],
            ..WorkspaceState::default()
        };
        let counts = super::workspace_session_counts(&state);
        for id in ["first", "second"] {
            let rows = super::workspace_session_rows(&state, id);
            let expected: Vec<&str> = state
                .sessions
                .iter()
                .filter(|session| {
                    super::workspace_of_session(&state, &session.session_id)
                        .is_some_and(|owner| owner.id == id)
                })
                .map(|session| session.session_id.as_str())
                .collect();
            let listed: Vec<&str> = rows
                .iter()
                .map(|(session, _)| session.session_id.as_str())
                .collect();
            assert_eq!(listed, expected, "{id}");
            assert_eq!(counts.get(id).copied().unwrap_or(0), expected.len(), "{id}");
        }
        let first = super::workspace_session_rows(&state, "first");
        let bound = first
            .iter()
            .find(|(session, _)| session.session_id == "in-first")
            .and_then(|(_, project)| *project);
        assert_eq!(bound, Some("/a"), "the first binding wins, as before");
        assert!(super::workspace_session_rows(&WorkspaceState::default(), "first").is_empty());
    }

    #[test]
    fn an_open_project_directory_unlocks_a_new_task() {
        let state = WorkspaceState {
            project_dir: Some("/tmp/app".to_owned()),
            ..WorkspaceState::default()
        };
        assert!(has_open_folder(&state));
    }

    #[test]
    fn a_workspace_root_unlocks_a_new_task() {
        let state = WorkspaceState {
            workspace_roots: vec!["/tmp/app".to_owned()],
            ..WorkspaceState::default()
        };
        assert!(has_open_folder(&state));
    }

    #[test]
    fn an_open_empty_idle_chat_is_reused_for_a_new_task() {
        let blank = WorkspaceState {
            sending: false,
            active: Some(mycode_app::ActiveConversation {
                session_id: "blank".to_owned(),
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
        assert!(open_session_is_empty(&blank));

        let mut with_messages = blank.clone();
        with_messages
            .active
            .as_mut()
            .expect("open")
            .entries
            .push(mycode_app::ConversationEntry {
                event_id: "e1".to_owned(),
                kind: mycode_app::EntryKind::UserMessage,
                text: "hello".into(),
                call_id: None,
                thinking: String::new(),
            });
        assert!(!open_session_is_empty(&with_messages));

        let mut sending = blank.clone();
        sending.sending = true;
        assert!(!open_session_is_empty(&sending));

        let mut hidden_history = blank;
        hidden_history.active.as_mut().expect("open").older_before = Some("e0".to_owned());
        assert!(!open_session_is_empty(&hidden_history));

        assert!(!open_session_is_empty(&WorkspaceState::default()));
    }
}
