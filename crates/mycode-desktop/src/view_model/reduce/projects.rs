//! Project- and workspace-shaped transitions: session bindings, named
//! workspaces, and the folder list of the workspace the sidebar shows.

use crate::view_model::{WorkspaceState, same_project_path};

/// Mirrors the active workspace's folders into `workspace_roots` so UI that
/// predates named workspaces keeps one source of truth.
pub(super) fn sync_workspace_roots(state: &mut WorkspaceState) {
    state.workspace_roots = active_folders(state)
        .map(|folders| folders.to_vec())
        .unwrap_or_default();
}

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
        .truncate(mycode_config::MAX_SESSION_PROJECTS);
}

/// The `WorkspaceRootAdded` transition: a folder was added to the workspace
/// the sidebar shows.
pub(super) fn workspace_root_added(state: &mut WorkspaceState, project: String) {
    if project.trim().is_empty() {
        return;
    }
    if let Some(workspace) = mutable_active_workspace(state) {
        workspace
            .folders
            .retain(|existing| !same_project_path(existing, &project));
        workspace.folders.insert(0, project.clone());
        workspace
            .folders
            .truncate(mycode_config::MAX_WORKSPACE_ROOTS);
    }
    state.recents.retain(|existing| existing != &project);
    state.recents.insert(0, project.clone());
    state.recents.truncate(mycode_config::MAX_RECENT_PROJECTS);
    if state.project_dir.is_none() {
        state.project_dir = Some(project);
    }
    sync_workspace_roots(state);
}

/// The `WorkspaceRootRemoved` transition: a folder left the active
/// workspace.
pub(super) fn workspace_root_removed(state: &mut WorkspaceState, project: String) {
    if let Some(workspace) = mutable_active_workspace(state) {
        workspace
            .folders
            .retain(|existing| !same_project_path(existing, &project));
    }
    sync_workspace_roots(state);
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

/// The `WorkspaceCreated` transition: a new workspace joined the list and
/// becomes the one the sidebar shows.
pub(super) fn workspace_created(
    state: &mut WorkspaceState,
    workspace: mycode_config::WorkspaceDef,
) {
    let id = workspace.id.clone();
    // Only the identity is unique: display names may repeat, and replacing
    // an existing workspace on a name collision would drop its folders.
    state.workspaces.retain(|existing| existing.id != id);
    state.workspaces.insert(0, workspace);
    state.workspaces.truncate(mycode_config::MAX_WORKSPACES);
    state.active_workspace = Some(id);
    sync_workspace_roots(state);
}

/// The `WorkspaceSwitched` transition: the sidebar shows another workspace.
pub(super) fn workspace_switched(state: &mut WorkspaceState, id: String) {
    if state.workspaces.iter().any(|workspace| workspace.id == id) {
        state.active_workspace = Some(id);
    }
    sync_workspace_roots(state);
}

/// The `WorkspaceRenamed` transition: the active workspace got a new name.
pub(super) fn workspace_renamed(state: &mut WorkspaceState, name: String) {
    let name: String = name
        .trim()
        .chars()
        .take(mycode_config::MAX_WORKSPACE_NAME_CHARS)
        .collect();
    if name.is_empty() {
        return;
    }
    if let Some(workspace) = mutable_active_workspace(state) {
        workspace.name = name;
    }
}

/// The `WorkspaceRemoved` transition: a workspace is gone; its sessions move
/// to the first survivor, which also becomes active. Removing the last
/// workspace is a no-op — the sidebar always needs one.
pub(super) fn workspace_removed(state: &mut WorkspaceState, id: String) {
    if state.workspaces.len() < 2 {
        return;
    }
    let Some(position) = state.workspaces.iter().position(|w| w.id == id) else {
        return;
    };
    state.workspaces.remove(position);
    let survivor = state
        .workspaces
        .first()
        .map(|workspace| workspace.id.clone())
        .expect("at least one workspace remains");
    for (_, bound) in &mut state.session_workspaces {
        if *bound == id {
            *bound = survivor.clone();
        }
    }
    if state.active_workspace.as_deref() == Some(id.as_str()) {
        state.active_workspace = Some(survivor);
    }
    sync_workspace_roots(state);
}

/// The `SessionWorkspaceBound` transition: a session joined a workspace.
pub(super) fn session_workspace_bound(
    state: &mut WorkspaceState,
    session_id: String,
    workspace_id: String,
) {
    if !state
        .workspaces
        .iter()
        .any(|workspace| workspace.id == workspace_id)
    {
        return;
    }
    state
        .session_workspaces
        .retain(|(existing, _)| existing != &session_id);
    state
        .session_workspaces
        .insert(0, (session_id, workspace_id));
    state
        .session_workspaces
        .truncate(mycode_config::MAX_SESSION_WORKSPACES);
}

/// The `SessionBindingsForgotten` transition: a session's durable data was
/// deleted, so no map should remember it.
pub(super) fn session_bindings_forgotten(state: &mut WorkspaceState, session_id: String) {
    state
        .session_projects
        .retain(|(existing, _)| existing != &session_id);
    state
        .session_workspaces
        .retain(|(existing, _)| existing != &session_id);
    state
        .sessions
        .retain(|session| session.session_id != session_id);
}

/// The active workspace's folder list, shared by the mirror and the
/// mutating transitions.
fn active_folders(state: &WorkspaceState) -> Option<&[String]> {
    match state.active_workspace.as_deref() {
        Some(id) => state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .map(|workspace| workspace.folders.as_slice())
            .or_else(|| state.workspaces.first().map(|w| w.folders.as_slice())),
        None => state.workspaces.first().map(|w| w.folders.as_slice()),
    }
}

/// The active workspace, mutably. `sync_workspace_roots` callers keep the
/// mirror in step with whatever this hands out.
fn mutable_active_workspace(
    state: &mut WorkspaceState,
) -> Option<&mut mycode_config::WorkspaceDef> {
    let active = state.active_workspace.clone();
    let index = state
        .workspaces
        .iter()
        .position(|workspace| Some(workspace.id.as_str()) == active.as_deref())
        .unwrap_or(0);
    state.workspaces.get_mut(index)
}
