//! Project- and workspace-shaped projections: path comparison, session
//! bindings, and the active-workspace view of the sidebar.

use mycode_app::SessionSummary;

use super::state::WorkspaceState;

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
/// named workspaces and belongs to the first one.
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
        Some(id) => state.workspaces.iter().find(|workspace| workspace.id == id),
        None => state.workspaces.first(),
    }
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

/// Compare project paths the way the sidebar groups them.
#[must_use]
pub(crate) fn same_project_path(left: &str, right: &str) -> bool {
    mycode_config::same_project_path(left, right)
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
    use super::has_open_folder;
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
}
