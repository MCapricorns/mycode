//! Durable UI state for the desktop: advisory, disposable, never a source
//! of truth for credentials or product behavior.
//!
//! Holds the recent project list, the last opened project, the update
//! preference, the last selected provider/model, and the model picker's
//! recent and starred pins so the desktop reopens where the user left off.
//! Missing or invalid documents reset to defaults.
use serde::{Deserialize, Serialize};

use crate::ConfigError;
use crate::error::ConfigErrorKind;
use crate::secure_fs::owned_file::{locked_update_owned_file, read_owned_file};

/// UI state path below the owned home.
pub const UI_STATE_PATH: &str = "ui.json";
/// Maximum encoded UI state size.
pub const MAX_UI_STATE_BYTES: usize = 64 * 1024;
/// UI state format version.
pub const UI_STATE_FORMAT_VERSION: u32 = 1;
/// UI state kind tag.
pub const UI_STATE_KIND: &str = "mycode-ui-state";
/// Maximum remembered recent projects.
pub const MAX_RECENT_PROJECTS: usize = 16;
/// Maximum folders in one workspace.
pub const MAX_WORKSPACE_ROOTS: usize = 8;
/// Maximum remembered session-to-project bindings.
pub const MAX_SESSION_PROJECTS: usize = 256;
/// Maximum workspaces kept in the UI state.
pub const MAX_WORKSPACES: usize = 16;
/// Maximum characters in one workspace name.
pub const MAX_WORKSPACE_NAME_CHARS: usize = 64;
/// Maximum remembered session-to-workspace bindings.
pub const MAX_SESSION_WORKSPACES: usize = 512;
/// Maximum projects explicitly trusted for project MCP config.
pub const MAX_TRUSTED_PROJECTS: usize = 64;
/// Maximum length of one remembered session id.
const MAX_SESSION_ID_BYTES: usize = 64;
/// Maximum length of one remembered project path.
const MAX_PROJECT_PATH_BYTES: usize = 1024;
/// Recent model pins kept at the front of the picker.
pub const MAX_RECENT_MODELS: usize = 8;
/// Starred model pins kept at the front of the picker.
pub const MAX_STARRED_MODELS: usize = 24;
/// Maximum characters in one provider or model id on a pin.
const MAX_MODEL_PIN_CHARS: usize = 256;

/// One named workspace: a set of folders plus the chats grouped under it.
///
/// Sessions reference a workspace by `id`; `folders` are absolute paths the
/// chat tools can use, exactly like the legacy single-workspace roots.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspaceDef {
    /// Stable workspace identity (`ws1-…`).
    pub id: String,
    /// User-visible name.
    pub name: String,
    /// Member folders, most recently added first.
    pub folders: Vec<String>,
}

impl WorkspaceDef {
    /// Mints a workspace with a fresh identity around `name` and `folders`.
    #[must_use]
    pub fn generate(name: &str, folders: Vec<String>) -> Self {
        Self {
            id: format!("ws1-{}", uuid::Uuid::new_v4().simple()),
            name: name.to_owned(),
            folders,
        }
    }
}

/// One provider and model the picker can jump to without scrolling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPin {
    /// Provider id.
    pub provider: String,
    /// Model id.
    pub model: String,
}

impl ModelPin {
    /// Builds one pin.
    #[must_use]
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }
}

/// Moves `provider`/`model` to the front of `recent` and drops the tail past
/// [`MAX_RECENT_MODELS`]. Invalid ids are ignored.
pub fn remember_model(recent: &mut Vec<ModelPin>, provider: &str, model: &str) {
    if !valid_pin_part(provider) || !valid_pin_part(model) {
        return;
    }
    recent.retain(|pin| pin.provider != provider || pin.model != model);
    recent.insert(
        0,
        ModelPin {
            provider: provider.to_owned(),
            model: model.to_owned(),
        },
    );
    recent.truncate(MAX_RECENT_MODELS);
}

/// Stars `provider`/`model`, or removes the pin when it is already starred.
///
/// Returns whether the pair is starred after the toggle. Invalid ids leave
/// the list unchanged and return `false`.
#[must_use]
pub fn toggle_star(starred: &mut Vec<ModelPin>, provider: &str, model: &str) -> bool {
    if !valid_pin_part(provider) || !valid_pin_part(model) {
        return false;
    }
    if let Some(index) = starred
        .iter()
        .position(|pin| pin.provider == provider && pin.model == model)
    {
        starred.remove(index);
        return false;
    }
    starred.insert(
        0,
        ModelPin {
            provider: provider.to_owned(),
            model: model.to_owned(),
        },
    );
    starred.truncate(MAX_STARRED_MODELS);
    true
}

/// Durable desktop UI state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UiState {
    /// Recent project directories, most recent first.
    pub recent_projects: Vec<String>,
    /// Last opened project directory.
    pub last_project: Option<String>,
    /// Check GitHub releases for updates automatically.
    pub auto_update: bool,
    /// Last selected provider id in the model picker.
    pub selected_provider: Option<String>,
    /// Last selected model id.
    pub selected_model: Option<String>,
    /// Session-to-project bindings (session id, project path), most recent
    /// first. Advisory: restores each chat's tool working directory.
    pub session_projects: Vec<(String, String)>,
    /// Folders currently in the workspace, most recently added first.
    /// The open chat still has one cwd; the other roots are extra tool roots.
    #[serde(default)]
    pub workspace_roots: Vec<String>,
    /// Named workspaces. Sessions are grouped under them by
    /// `session_workspaces`; the legacy `workspace_roots` list seeds the
    /// first workspace on upgrade and mirrors the active one's folders.
    #[serde(default)]
    pub workspaces: Vec<WorkspaceDef>,
    /// Session-to-workspace bindings (session id, workspace id), most recent
    /// first. A session missing here belongs to the first workspace.
    #[serde(default)]
    pub session_workspaces: Vec<(String, String)>,
    /// The workspace the sidebar shows.
    #[serde(default)]
    pub active_workspace: Option<String>,
    /// Absolute project paths allowed to contribute `.mycode/mcp.json`.
    /// Opening a folder does not add it here.
    #[serde(default)]
    pub trusted_projects: Vec<String>,
    /// Models picked from the composer or the default-model control, newest
    /// first. Advisory: a missing provider is dropped at render time.
    #[serde(default)]
    pub recent_models: Vec<ModelPin>,
    /// Models the user starred in the picker, newest first.
    #[serde(default)]
    pub starred_models: Vec<ModelPin>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            recent_projects: Vec::new(),
            last_project: None,
            auto_update: true,
            selected_provider: None,
            selected_model: None,
            session_projects: Vec::new(),
            workspace_roots: Vec::new(),
            workspaces: Vec::new(),
            session_workspaces: Vec::new(),
            active_workspace: None,
            trusted_projects: Vec::new(),
            recent_models: Vec::new(),
            starred_models: Vec::new(),
        }
    }
}

impl UiState {
    /// Records one project directory as the most recent.
    ///
    /// Invalid, duplicate, or overflowing entries are dropped silently; the
    /// state is advisory and must never fail product flows.
    pub fn touch_project(&mut self, project: &str) {
        let Some(project) = valid_project_path(project) else {
            return;
        };
        self.recent_projects.retain(|existing| existing != &project);
        self.recent_projects.insert(0, project.clone());
        self.recent_projects.truncate(MAX_RECENT_PROJECTS);
        self.last_project = Some(project);
    }

    /// Records that `project` may contribute project MCP configuration.
    ///
    /// This is not called from project open. Trust stays explicit.
    pub fn trust_project(&mut self, project: &str) {
        let Some(project) = valid_project_path(project) else {
            return;
        };
        self.trusted_projects
            .retain(|existing| existing != &project);
        self.trusted_projects.insert(0, project);
        self.trusted_projects.truncate(MAX_TRUSTED_PROJECTS);
    }

    /// Binds one session to a project directory (upsert, most recent first).
    ///
    /// Invalid ids or paths are dropped silently, like `touch_project`.
    /// Drops one directory from the remembered projects (and last-project
    /// pin when it matches).
    pub fn remove_recent(&mut self, project: &str) {
        self.recent_projects.retain(|existing| existing != project);
        if self.last_project.as_deref() == Some(project) {
            self.last_project = self.recent_projects.first().cloned();
        }
    }

    /// Upserts one session's project binding at the front of the list.
    pub fn set_session_project(&mut self, session_id: &str, project: &str) {
        let Some(project) = valid_project_path(project) else {
            return;
        };
        if !valid_session_id(session_id) {
            return;
        }
        let session_id = session_id.to_owned();
        self.session_projects
            .retain(|(existing, _)| *existing != session_id);
        self.session_projects.insert(0, (session_id, project));
        self.session_projects.truncate(MAX_SESSION_PROJECTS);
    }

    /// The project bound to one session, when remembered.
    #[must_use]
    pub fn project_for_session(&self, session_id: &str) -> Option<&str> {
        self.session_projects
            .iter()
            .find(|(existing, _)| existing == session_id)
            .map(|(_, project)| project.as_str())
    }

    /// Upserts one session's workspace binding at the front of the list.
    ///
    /// Like `set_session_project`, invalid ids or unknown workspaces are
    /// dropped silently.
    pub fn set_session_workspace(&mut self, session_id: &str, workspace_id: &str) {
        if !valid_session_id(session_id)
            || !self
                .workspaces
                .iter()
                .any(|workspace| workspace.id == workspace_id)
        {
            return;
        }
        let session_id = session_id.to_owned();
        self.session_workspaces
            .retain(|(existing, _)| *existing != session_id);
        self.session_workspaces
            .insert(0, (session_id, workspace_id.to_owned()));
        self.session_workspaces.truncate(MAX_SESSION_WORKSPACES);
    }

    /// The workspace bound to one session, when remembered.
    #[must_use]
    pub fn workspace_for_session(&self, session_id: &str) -> Option<&str> {
        self.session_workspaces
            .iter()
            .find(|(existing, _)| existing == session_id)
            .map(|(_, workspace)| workspace.as_str())
    }

    /// Drops every remembered binding for one session (project and
    /// workspace). Called when a session's durable data is deleted so the
    /// lists never accumulate ids nothing resolves anymore.
    pub fn forget_session(&mut self, session_id: &str) {
        self.session_projects
            .retain(|(existing, _)| existing != session_id);
        self.session_workspaces
            .retain(|(existing, _)| existing != session_id);
    }

    /// Validates the document.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] for any bound
    /// violation.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = || ConfigError::authority_rejection();
        if self.recent_projects.len() > MAX_RECENT_PROJECTS {
            return Err(invalid());
        }
        for project in &self.recent_projects {
            if valid_project_path(project).is_none() {
                return Err(invalid());
            }
        }
        if let Some(project) = &self.last_project
            && valid_project_path(project).is_none()
        {
            return Err(invalid());
        }
        if self.session_projects.len() > MAX_SESSION_PROJECTS {
            return Err(invalid());
        }
        for (session_id, project) in &self.session_projects {
            if !valid_session_id(session_id) || valid_project_path(project).is_none() {
                return Err(invalid());
            }
        }
        if self.workspace_roots.len() > MAX_WORKSPACE_ROOTS {
            return Err(invalid());
        }
        for project in &self.workspace_roots {
            if valid_project_path(project).is_none() {
                return Err(invalid());
            }
        }
        if self.workspaces.len() > MAX_WORKSPACES {
            return Err(invalid());
        }
        for (index, workspace) in self.workspaces.iter().enumerate() {
            if !valid_workspace_id(&workspace.id)
                || self.workspaces[..index]
                    .iter()
                    .any(|earlier| earlier.id == workspace.id)
            {
                return Err(invalid());
            }
            let name = workspace.name.trim();
            if name.is_empty()
                || workspace.name.chars().count() > MAX_WORKSPACE_NAME_CHARS
                || workspace.name.chars().any(char::is_control)
            {
                return Err(invalid());
            }
            if workspace.folders.len() > MAX_WORKSPACE_ROOTS {
                return Err(invalid());
            }
            for folder in &workspace.folders {
                if valid_project_path(folder).is_none() {
                    return Err(invalid());
                }
            }
        }
        if let Some(active) = &self.active_workspace
            && !self
                .workspaces
                .iter()
                .any(|workspace| &workspace.id == active)
        {
            return Err(invalid());
        }
        if self.trusted_projects.len() > MAX_TRUSTED_PROJECTS {
            return Err(invalid());
        }
        for project in &self.trusted_projects {
            if valid_project_path(project).is_none() {
                return Err(invalid());
            }
        }
        if self.session_workspaces.len() > MAX_SESSION_WORKSPACES {
            return Err(invalid());
        }
        for (session_id, workspace_id) in &self.session_workspaces {
            if !valid_session_id(session_id)
                || !self
                    .workspaces
                    .iter()
                    .any(|workspace| &workspace.id == workspace_id)
            {
                return Err(invalid());
            }
        }
        validate_pins(&self.recent_models, MAX_RECENT_MODELS)?;
        validate_pins(&self.starred_models, MAX_STARRED_MODELS)?;
        Ok(())
    }
}

fn validate_pins(pins: &[ModelPin], cap: usize) -> Result<(), ConfigError> {
    if pins.len() > cap {
        return Err(ConfigError::authority_rejection());
    }
    if pins
        .iter()
        .any(|pin| !valid_pin_part(&pin.provider) || !valid_pin_part(&pin.model))
    {
        return Err(ConfigError::authority_rejection());
    }
    Ok(())
}

fn valid_pin_part(value: &str) -> bool {
    let len = value.chars().count();
    (1..=MAX_MODEL_PIN_CHARS).contains(&len) && !value.chars().any(char::is_control)
}

fn valid_workspace_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SESSION_ID_BYTES
        && value.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
}

fn valid_project_path(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > MAX_PROJECT_PATH_BYTES
        || value.chars().any(char::is_control)
        || !std::path::Path::new(value).is_absolute()
    {
        return None;
    }
    Some(value.to_owned())
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_SESSION_ID_BYTES && !value.chars().any(char::is_control)
}

/// Reads the UI state; a missing document yields the defaults.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security or oversized content.
pub fn read_ui_state(home: &crate::HomeLayout) -> Result<UiState, ConfigError> {
    let bytes = read_owned_file(home, UI_STATE_PATH, MAX_UI_STATE_BYTES)?;
    let Some(bytes) = bytes else {
        return Ok(UiState::default());
    };
    let (state, migrated) = decode_ui_state(bytes.as_slice())?;
    if migrated {
        let _ = replace_ui_state(home, &state);
    }
    Ok(state)
}

/// Replaces the UI state under the owned-file lock (no revision CAS; the
/// state is advisory and last-writer-wins).
///
/// # Errors
///
/// Returns [`ConfigError`] for validation or transaction failures.
pub fn replace_ui_state(home: &crate::HomeLayout, state: &UiState) -> Result<(), ConfigError> {
    state.validate()?;
    let bytes = replace_bytes(state)?;
    locked_update_owned_file(home, UI_STATE_PATH, MAX_UI_STATE_BYTES, |_| Ok(bytes))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SerializedUiState {
    format_version: u32,
    kind: String,
    #[serde(flatten)]
    state: UiState,
}

fn replace_bytes(state: &UiState) -> Result<Vec<u8>, ConfigError> {
    let document = SerializedUiState {
        format_version: UI_STATE_FORMAT_VERSION,
        kind: UI_STATE_KIND.to_owned(),
        state: state.clone(),
    };
    let mut bytes = serde_json::to_vec_pretty(&document)
        .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn decode_ui_state(bytes: &[u8]) -> Result<(UiState, bool), ConfigError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Envelope {
        format_version: u32,
        kind: String,
        #[serde(flatten)]
        state: UiState,
    }
    let decoded = crate::json_recover::decode_json::<Envelope>(bytes)?;
    let envelope = decoded.value;
    if envelope.format_version != UI_STATE_FORMAT_VERSION || envelope.kind != UI_STATE_KIND {
        return Err(ConfigError::authority_rejection()
            .with_detail("ui.json: formatVersion or kind does not match this build"));
    }
    envelope.state.validate()?;
    Ok((envelope.state, decoded.migrated))
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_RECENT_MODELS, MAX_STARRED_MODELS, ModelPin, decode_ui_state, remember_model,
        toggle_star,
    };

    #[test]
    fn missing_pin_fields_decode_empty() {
        let bytes = br#"{"formatVersion":1,"kind":"mycode-ui-state"}"#;
        let (state, _) = decode_ui_state(bytes).expect("legacy ui.json");
        assert!(state.recent_models.is_empty());
        assert!(state.starred_models.is_empty());
        assert!(state.auto_update);
    }

    #[test]
    fn remember_model_dedupes_and_caps() {
        let mut recent = Vec::new();
        remember_model(&mut recent, "openai", "gpt-4");
        remember_model(&mut recent, "openai", "gpt-4");
        assert_eq!(recent.len(), 1);
        for index in 0..MAX_RECENT_MODELS + 2 {
            remember_model(&mut recent, "openai", &format!("m{index}"));
        }
        assert_eq!(recent.len(), MAX_RECENT_MODELS);
        assert_eq!(recent[0].model, format!("m{}", MAX_RECENT_MODELS + 1));
        remember_model(&mut recent, "openai\n", "bad");
        assert_eq!(recent.len(), MAX_RECENT_MODELS);
    }

    #[test]
    fn toggle_star_inserts_and_removes() {
        let mut starred = Vec::new();
        assert!(toggle_star(&mut starred, "anthropic", "claude"));
        assert!(!toggle_star(&mut starred, "anthropic", "claude"));
        assert!(starred.is_empty());
        for index in 0..MAX_STARRED_MODELS + 3 {
            assert!(toggle_star(&mut starred, "anthropic", &format!("m{index}")));
        }
        assert_eq!(starred.len(), MAX_STARRED_MODELS);
    }

    #[test]
    fn pins_reject_control_characters_and_overflow() {
        let mut state = super::UiState::default();
        state.recent_models.push(ModelPin::new("ok", "bad\u{0001}"));
        assert!(state.validate().is_err());
        state.recent_models.clear();
        for index in 0..=MAX_RECENT_MODELS {
            state
                .recent_models
                .push(ModelPin::new("openai", format!("m{index}")));
        }
        assert!(state.validate().is_err());
    }
}
