//! Project-shaped projections: path comparison, session bindings, and the
//! sidebar's flat view of sessions and folders.

use mycode_app::SessionSummary;
pub use mycode_app::same_project_path;

use super::state::WorkspaceState;

/// Short display label for a project folder path (its trailing component, or
/// the full path when it has none).
#[must_use]
pub fn project_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_owned())
}

/// The folder bound to one session, if it has one.
#[must_use]
pub fn project_of_session<'a>(
    bindings: &'a [(String, String)],
    session_id: &str,
) -> Option<&'a str> {
    bindings
        .iter()
        .find(|(id, _)| id == session_id)
        .map(|(_, project)| project.as_str())
}

/// Every session with its project binding, in stored order. The binding map
/// is indexed once instead of scanned per session.
#[must_use]
pub fn session_rows(state: &WorkspaceState) -> Vec<(&SessionSummary, Option<&str>)> {
    let mut projects: std::collections::HashMap<&str, &str> =
        std::collections::HashMap::with_capacity(state.session_projects.len());
    for (session, project) in &state.session_projects {
        projects.entry(session.as_str()).or_insert(project.as_str());
    }
    state
        .sessions
        .iter()
        .map(|session| (session, projects.get(session.session_id.as_str()).copied()))
        .collect()
}

/// Conversations visible in the current project, in stored order. Unbound
/// conversations are visible only when no project is selected.
#[must_use]
pub fn project_session_rows(state: &WorkspaceState) -> Vec<(&SessionSummary, Option<&str>)> {
    session_rows(state)
        .into_iter()
        .filter(
            |(_, project)| match (state.project_dir.as_deref(), *project) {
                (Some(current), Some(bound)) => same_project_path(current, bound),
                (None, None) => true,
                _ => false,
            },
        )
        .collect()
}

/// Task cards belong to the open session's project. Leaving that project
/// hides them instead of leaving another project's work on screen.
#[must_use]
pub fn task_surface_visible(state: &WorkspaceState) -> bool {
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

/// Whether a folder is open, so a new session has a working directory.
///
/// An empty or whitespace-only path does not count.
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
pub fn newest_session_in_project<'a>(
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
    fn session_rows_pair_each_summary_with_its_project() {
        let session = |id: &str| crate::view_model::SessionSummary {
            session_id: id.to_owned(),
            root_branch_id: "br".to_owned(),
            title: String::new(),
            updated_at_ms: None,
            event_count: 1,
            active: false,
            corrupt: false,
        };
        let state = WorkspaceState {
            sessions: ["a", "b", "c"].into_iter().map(session).collect(),
            session_projects: vec![
                ("b".to_owned(), "/other".to_owned()),
                ("b".to_owned(), "/first-wins".to_owned()),
                ("a".to_owned(), "/app".to_owned()),
            ],
            ..WorkspaceState::default()
        };
        let rows = super::session_rows(&state);
        assert_eq!(rows.len(), 3);
        // First binding wins: the map keeps the earliest stored path.
        assert_eq!(
            rows.iter()
                .find(|(session, _)| session.session_id == "b")
                .and_then(|(_, project)| *project),
            Some("/other"),
            "the first binding wins, matching bind_session_project"
        );
    }

    #[test]
    fn conversation_scope_follows_the_selected_project() {
        let session = |id: &str| crate::view_model::SessionSummary {
            session_id: id.to_owned(),
            root_branch_id: "br".to_owned(),
            title: "Same search result".to_owned(),
            updated_at_ms: None,
            event_count: 1,
            active: false,
            corrupt: false,
        };
        let mut state = WorkspaceState {
            sessions: ["app", "nested", "other", "unbound"]
                .into_iter()
                .map(session)
                .collect(),
            session_projects: vec![
                ("app".to_owned(), "/app/".to_owned()),
                ("nested".to_owned(), "/app/nested".to_owned()),
                ("other".to_owned(), "/other".to_owned()),
            ],
            ..WorkspaceState::default()
        };
        for (project, expected) in [
            (Some("/app"), "app"),
            (Some("/other"), "other"),
            (None, "unbound"),
        ] {
            state.project_dir = project.map(str::to_owned);
            let rows = super::project_session_rows(&state);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].0.session_id, expected);
        }
        state.project_dir = Some("/empty".to_owned());
        assert!(super::project_session_rows(&state).is_empty());
        assert_eq!(
            state.sessions.len(),
            4,
            "switching projects never deletes conversations"
        );
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
    fn a_blank_folder_list_stays_closed() {
        let state = WorkspaceState {
            project_dir: Some("   ".to_owned()),
            workspace_roots: vec![" ".to_owned()],
            ..WorkspaceState::default()
        };
        assert!(!has_open_folder(&state));
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
                running: false,
                compacting: false,
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
                agent_states: None,
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
