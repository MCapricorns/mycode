//! Durable UI state for the desktop: advisory, disposable, never a source
//! of truth for credentials or product behavior.
//!
//! Holds recent projects, workspace folders, session bindings, the update
//! preference, trusted project paths, and the model picker's recent,
//! starred, and per-session pins so the desktop reopens where the user
//! left off. Missing or invalid documents reset to defaults.
use serde::{Deserialize, Serialize};

use crate::ConfigError;
use crate::error::ConfigErrorKind;
use crate::secure_fs::owned_file::locked_update_owned_file;

/// UI state path below the owned home.
pub const UI_STATE_PATH: &str = "ui.json";
/// Maximum encoded UI state size.
///
/// Session model pins sit beside the project bindings, so the document is
/// larger than the original recent-folder list.
pub const MAX_UI_STATE_BYTES: usize = 256 * 1024;
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
/// Per-session model pins. Each chat keeps its own provider, model, and
/// reasoning effort so one session cannot change the others.
pub const MAX_SESSION_MODELS: usize = 128;

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

/// Provider, model, and reasoning effort for one session.
///
/// The picker shows this pin while the session is open. Other sessions keep
/// their own pins, so a model that one chat cannot use does not leak.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModelPin {
    /// Session identity spelling.
    pub session_id: String,
    /// Provider id from settings.
    pub provider: String,
    /// Model id offered by that provider.
    pub model: String,
    /// Requested reasoning effort. Absent leaves the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

/// Inserts or replaces one session's model pin, newest first.
///
/// Invalid ids are ignored. `reasoning` that is not a short token is dropped
/// rather than stored, so a bad value cannot fail the whole document later.
pub fn upsert_session_model(
    pins: &mut Vec<SessionModelPin>,
    session_id: &str,
    provider: &str,
    model: &str,
    reasoning: Option<String>,
) {
    if !valid_session_id(session_id) || !valid_pin_part(provider) || !valid_pin_part(model) {
        return;
    }
    let reasoning = reasoning.filter(|level| valid_effort_token(level));
    pins.retain(|pin| pin.session_id != session_id);
    pins.insert(
        0,
        SessionModelPin {
            session_id: session_id.to_owned(),
            provider: provider.to_owned(),
            model: model.to_owned(),
            reasoning,
        },
    );
    pins.truncate(MAX_SESSION_MODELS);
}

/// The pin for `session_id`, if one was stored.
#[must_use]
pub fn session_model<'a>(
    pins: &'a [SessionModelPin],
    session_id: &str,
) -> Option<&'a SessionModelPin> {
    pins.iter().find(|pin| pin.session_id == session_id)
}

fn valid_effort_token(value: &str) -> bool {
    let len = value.chars().count();
    (1..=32).contains(&len)
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
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
    /// Per-session provider, model, and reasoning. Newest first.
    #[serde(default)]
    pub session_models: Vec<SessionModelPin>,
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
            session_models: Vec::new(),
        }
    }
}

impl UiState {
    /// Drops one directory from the remembered projects, and clears the
    /// last-project pin when it matches. The next recent project, if any,
    /// becomes the pin.
    pub fn remove_recent(&mut self, project: &str) {
        self.recent_projects.retain(|existing| existing != project);
        if self.last_project.as_deref() == Some(project) {
            self.last_project = self.recent_projects.first().cloned();
        }
    }

    /// Drops every remembered binding for one session (project and
    /// workspace). Called when a session's durable data is deleted so the
    /// lists never accumulate ids nothing resolves anymore.
    pub fn forget_session(&mut self, session_id: &str) {
        self.session_projects
            .retain(|(existing, _)| existing != session_id);
        self.session_workspaces
            .retain(|(existing, _)| existing != session_id);
        self.session_models
            .retain(|pin| pin.session_id != session_id);
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
        if self.session_models.len() > MAX_SESSION_MODELS {
            return Err(invalid());
        }
        for pin in &self.session_models {
            if !valid_session_id(&pin.session_id)
                || !valid_pin_part(&pin.provider)
                || !valid_pin_part(&pin.model)
                || pin
                    .reasoning
                    .as_deref()
                    .is_some_and(|level| !valid_effort_token(level))
            {
                return Err(invalid());
            }
        }
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

/// Canonical project path for trust and sidebar comparison.
///
/// Trims surrounding whitespace, drops a trailing slash, and on Windows
/// folds slash direction and ASCII case. An empty result means the path was
/// only slashes.
#[must_use]
pub fn normalize_project_path(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        trimmed.replace('/', "\\").to_ascii_lowercase()
    } else {
        trimmed.to_owned()
    }
}

/// Whether two project paths name the same folder.
///
/// The sidebar, MCP project trust, and the trusted-project list all use this
/// so a trailing slash (and, on Windows, letter case) cannot split one folder
/// into two.
#[must_use]
pub fn same_project_path(left: &str, right: &str) -> bool {
    let left = normalize_project_path(left);
    let right = normalize_project_path(right);
    !left.is_empty() && left == right
}

/// Records `path` as allowed to contribute `.mycode/mcp.json`.
///
/// Opening a folder does not call this. An invalid path, or a new path when
/// the list is already full, returns false and leaves the list unchanged. A
/// path that is already trusted moves to the front.
#[must_use]
pub fn trust_project(projects: &mut Vec<String>, path: &str) -> bool {
    let Some(path) = canonical_trusted_project(path) else {
        return false;
    };
    if let Some(index) = projects
        .iter()
        .position(|existing| same_project_path(existing, &path))
    {
        projects.remove(index);
        projects.insert(0, path);
        return true;
    }
    if projects.len() >= MAX_TRUSTED_PROJECTS {
        return false;
    }
    projects.insert(0, path);
    true
}

/// Removes `path` from the trusted project list.
///
/// Returns whether the path was present. An invalid path returns false.
#[must_use]
pub fn revoke_project_trust(projects: &mut Vec<String>, path: &str) -> bool {
    let Some(path) = canonical_trusted_project(path) else {
        return false;
    };
    let before = projects.len();
    projects.retain(|existing| !same_project_path(existing, &path));
    projects.len() != before
}

fn canonical_trusted_project(path: &str) -> Option<String> {
    let path = valid_project_path(path)?;
    let path = normalize_project_path(&path);
    (!path.is_empty()).then_some(path)
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_SESSION_ID_BYTES && !value.chars().any(char::is_control)
}

/// Reads the UI state.
///
/// A missing document yields the defaults. A trailing comma is accepted and
/// a canonical rewrite is attempted. A document that cannot be parsed or
/// validated is backed up and replaced with defaults.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security or when a damaged document
/// cannot be copied aside.
pub fn read_ui_state(home: &crate::HomeLayout) -> Result<UiState, ConfigError> {
    Ok(read_ui_state_with_repair(home)?.0)
}

/// Reads `ui.json`, repairing a damaged document.
///
/// The second value is set when the previous bytes were copied aside and the
/// file was replaced with defaults. Workspaces and recent folders in that
/// backup are not deleted from disk elsewhere; session ledgers stay put.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security or when the backup or the
/// replacement cannot be published.
pub fn read_ui_state_with_repair(
    home: &crate::HomeLayout,
) -> Result<(UiState, Option<crate::DocumentRepair>), ConfigError> {
    let loaded = crate::document_repair::load_or_reset(
        home,
        UI_STATE_PATH,
        MAX_UI_STATE_BYTES,
        |bytes| {
            let (state, migrated) = decode_ui_state(bytes)?;
            if migrated {
                replace_ui_state(home, &state)?;
            }
            Ok(state)
        },
        UiState::default,
        || replace_ui_state(home, &UiState::default()),
    )?;
    Ok((loaded.value, loaded.repair))
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

    #[test]
    fn trust_project_round_trips_and_rejects_a_relative_path() {
        let mut projects = Vec::new();
        assert!(!super::trust_project(&mut projects, "relative/path"));
        assert!(projects.is_empty());
        assert!(super::trust_project(&mut projects, "/tmp/mycode-trust-a"));
        assert!(super::trust_project(&mut projects, "/tmp/mycode-trust-b"));
        assert_eq!(
            projects,
            vec![
                "/tmp/mycode-trust-b".to_owned(),
                "/tmp/mycode-trust-a".to_owned()
            ]
        );
        assert!(super::revoke_project_trust(
            &mut projects,
            "/tmp/mycode-trust-a"
        ));
        assert!(!super::revoke_project_trust(
            &mut projects,
            "/tmp/mycode-trust-a"
        ));
        assert_eq!(projects, vec!["/tmp/mycode-trust-b".to_owned()]);

        let parent = std::env::temp_dir().join(format!(
            "mycode-ui-trust-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&parent).expect("temp parent");
        let root = parent.join("home");
        std::fs::create_dir(&root).expect("home");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
        }
        let home = crate::HomeLayout::from_root(&root).expect("layout");
        let mut state = super::UiState::default();
        assert!(super::trust_project(
            &mut state.trusted_projects,
            "/tmp/mycode-trust-b"
        ));
        super::replace_ui_state(&home, &state).expect("write");
        let loaded = super::read_ui_state(&home).expect("read");
        assert_eq!(
            loaded.trusted_projects,
            vec!["/tmp/mycode-trust-b".to_owned()]
        );
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn trusted_project_collapses_a_trailing_slash() {
        assert!(super::same_project_path("/tmp/app/", "/tmp/app"));
        assert!(!super::same_project_path("/tmp/app", "/tmp/other"));
        let mut projects = Vec::new();
        assert!(super::trust_project(
            &mut projects,
            "/tmp/mycode-trust-slash/"
        ));
        assert_eq!(projects, vec!["/tmp/mycode-trust-slash".to_owned()]);
        assert!(super::trust_project(
            &mut projects,
            "/tmp/mycode-trust-slash"
        ));
        assert_eq!(projects.len(), 1);
        assert!(super::revoke_project_trust(
            &mut projects,
            "/tmp/mycode-trust-slash/"
        ));
        assert!(projects.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn trusted_project_matches_windows_drive_case() {
        assert!(super::same_project_path("C:\\Work\\App\\", "c:/work/app"));
        assert_eq!(
            super::normalize_project_path("C:/Work/App/"),
            "c:\\work\\app"
        );
        let mut projects = Vec::new();
        assert!(super::trust_project(&mut projects, "C:\\Work\\App\\"));
        assert_eq!(projects, vec!["c:\\work\\app".to_owned()]);
        assert!(super::trust_project(&mut projects, "c:/work/app"));
        assert_eq!(projects.len(), 1);
        assert!(super::revoke_project_trust(&mut projects, "C:/Work/App"));
        assert!(projects.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn trailing_comma_repair_surfaces_a_rewrite_failure() {
        use std::os::unix::fs::PermissionsExt;

        let parent = std::env::temp_dir().join(format!(
            "mycode-ui-comma-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&parent).expect("temp parent");
        let root = parent.join("home");
        std::fs::create_dir(&root).expect("home");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
        let home = crate::HomeLayout::from_root(&root).expect("layout");
        let path = root.join("ui.json");
        std::fs::write(&path, br#"{"formatVersion":1,"kind":"mycode-ui-state",}"#).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("file mode");
        std::fs::create_dir(root.join("ui.json.lock")).expect("lock dir");
        let error = super::read_ui_state(&home).expect_err("rewrite must fail");
        assert_ne!(error.kind(), crate::ConfigErrorKind::AuthorityValidation);
        let kept = std::fs::read(&path).expect("original remains");
        assert!(kept.windows(2).any(|window| window == b",}"), "{kept:?}");
        let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&parent);
    }
}
