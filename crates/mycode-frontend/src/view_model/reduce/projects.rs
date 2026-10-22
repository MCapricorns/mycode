//! Project-shaped transitions: session bindings and the flat folder list.

use crate::view_model::{WorkspaceState, same_project_path};

pub(super) fn bind_session_project(
    state: &mut WorkspaceState,
    session_id: String,
    project: String,
) {
    state
        .session_projects
        .retain(|(existing, _)| existing != &session_id);
    state.session_projects.insert(0, (session_id, project));
    state
        .session_projects
        .truncate(mycode_app::MAX_SESSION_PROJECTS);
}

/// The `WorkspaceRootAdded` transition: a folder joined the list.
pub(super) fn workspace_root_added(state: &mut WorkspaceState, project: String) {
    if project.trim().is_empty() {
        return;
    }
    state
        .workspace_roots
        .retain(|existing| !same_project_path(existing, &project));
    // A list merged from legacy named workspaces may already exceed the
    // cap; a full list only ever drops its oldest folder, never the merge.
    let cap = state
        .workspace_roots
        .len()
        .max(mycode_app::MAX_WORKSPACE_ROOTS);
    state.workspace_roots.insert(0, project.clone());
    state.workspace_roots.truncate(cap);
    // Same spelling-fold comparison as the roots list above, so a dropped
    // path and a picked path do not leave two recents rows for one folder.
    state
        .recents
        .retain(|existing| !same_project_path(existing, &project));
    state.recents.insert(0, project.clone());
    state.recents.truncate(mycode_app::MAX_RECENT_PROJECTS);
    if state.project_dir.is_none() {
        state.project_dir = Some(project);
    }
}

/// The `WorkspaceRootRemoved` transition: a folder left the list.
pub(super) fn workspace_root_removed(state: &mut WorkspaceState, project: String) {
    state
        .workspace_roots
        .retain(|existing| !same_project_path(existing, &project));
}

/// The `SessionProjectBound` transition: a session was bound to a project in
/// the durable map.
pub(super) fn session_project_bound(
    state: &mut WorkspaceState,
    session_id: String,
    project: String,
) {
    // A session keeps the first folder it was bound to. Opening
    // another folder starts a different session instead of moving
    // this conversation's tool directory.
    let locked = state
        .session_projects
        .iter()
        .find(|(existing, _)| existing == &session_id)
        .is_some_and(|(_, existing)| !same_project_path(existing, &project));
    if !locked {
        bind_session_project(state, session_id, project);
    }
}

/// The `SessionBindingsForgotten` transition: a session's durable data was
/// deleted, so no map should remember it.
pub(super) fn session_bindings_forgotten(state: &mut WorkspaceState, session_id: String) {
    state
        .session_projects
        .retain(|(existing, _)| existing != &session_id);
    state
        .session_models
        .retain(|pin| pin.session_id != session_id);
    state
        .sessions
        .retain(|session| session.session_id != session_id);
    state
        .parked_steers
        .retain(|(parked, _)| parked != &session_id);
}

#[cfg(test)]
mod tests {
    use crate::view_model::WorkspaceState;

    #[test]
    fn adding_a_folder_keeps_a_merged_legacy_list() {
        let legacy: Vec<String> = (0..12).map(|index| format!("/legacy/{index}")).collect();
        let mut state = WorkspaceState {
            workspace_roots: legacy.clone(),
            ..WorkspaceState::default()
        };
        super::workspace_root_added(&mut state, "/new".to_owned());
        assert_eq!(state.workspace_roots.len(), legacy.len());
        assert_eq!(state.workspace_roots[0], "/new");
        assert_eq!(state.workspace_roots[1..], legacy[..legacy.len() - 1]);

        let mut fresh = WorkspaceState::default();
        for index in 0..mycode_app::MAX_WORKSPACE_ROOTS + 2 {
            super::workspace_root_added(&mut fresh, format!("/f/{index}"));
        }
        assert_eq!(fresh.workspace_roots.len(), mycode_app::MAX_WORKSPACE_ROOTS);
    }
}
