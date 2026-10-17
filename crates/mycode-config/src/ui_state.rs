//! Durable UI state for the desktop: advisory, disposable, never a source
//! of truth for credentials or product behavior.
//!
//! Holds recent projects, workspace folders, session bindings, the update
//! preference, and the model picker's recent and per-session pins
//! so the desktop reopens where the user left off. Missing or invalid
//! documents reset to defaults.
use serde::{Deserialize, Serialize};

use crate::ConfigError;
use crate::error::ConfigErrorKind;
use crate::secure_fs::owned_file::locked_update_owned_file;

/// UI state path below the owned home.
const UI_STATE_PATH: &str = "ui.json";
/// Maximum encoded UI state size.
///
/// Session model pins sit beside the project bindings, so the document is
/// larger than the original recent-folder list.
const MAX_UI_STATE_BYTES: usize = 256 * 1024;
/// UI state format version.
const UI_STATE_FORMAT_VERSION: u32 = 1;
/// UI state kind tag.
const UI_STATE_KIND: &str = "mycode-ui-state";
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
/// Maximum length of one remembered session id.
const MAX_SESSION_ID_BYTES: usize = 64;
/// Maximum length of one remembered project path.
const MAX_PROJECT_PATH_BYTES: usize = 1024;
/// Recent model pins kept at the front of the picker.
const MAX_RECENT_MODELS: usize = 8;
/// Maximum characters in one provider or model id on a pin.
const MAX_MODEL_PIN_CHARS: usize = 256;
/// Per-session model pins. Each chat keeps its own provider, model, and
/// reasoning effort so one session cannot change the others.
const MAX_SESSION_MODELS: usize = 128;
/// Largest accepted window edge in logical pixels. Well past any sane
/// multi-monitor span; it only guards the document against absurd values.
pub const MAX_WINDOW_EDGE_PX: u32 = 16_384;

/// The windowed size restored on the next launch, in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct WindowSize {
    /// Window width.
    pub width: u32,
    /// Window height.
    pub height: u32,
}

/// One named workspace: a set of folders plus the chats grouped under it.
///
/// Sessions reference a workspace by `id`; `folders` are absolute paths the
/// chat tools can use.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
/// `MAX_RECENT_MODELS`. Invalid ids are ignored.
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

/// Provider, model, and reasoning effort for one session.
///
/// The picker shows this pin while the session is open. Other sessions keep
/// their own pins, so a model that one chat cannot use does not leak.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

/// Returns whether `value` is a syntactically valid effort token (`1..=32`
/// ASCII alphanumeric, `-`, or `_` bytes).
#[must_use]
pub fn is_valid_effort_token(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn valid_effort_token(value: &str) -> bool {
    is_valid_effort_token(value)
}

/// Durable desktop UI state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
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
    /// Named workspaces. Sessions are grouped under them by
    /// `session_workspaces`.
    #[serde(default)]
    pub workspaces: Vec<WorkspaceDef>,
    /// Session-to-workspace bindings (session id, workspace id), most recent
    /// first. A session missing here belongs to the first workspace.
    #[serde(default)]
    pub session_workspaces: Vec<(String, String)>,
    /// The workspace the sidebar shows.
    #[serde(default)]
    pub active_workspace: Option<String>,
    /// Models picked from the composer or the default-model control, newest
    /// first. Advisory: a missing provider is dropped at render time.
    #[serde(default)]
    pub recent_models: Vec<ModelPin>,
    /// Per-session provider, model, and reasoning. Newest first.
    #[serde(default)]
    pub session_models: Vec<SessionModelPin>,
    /// Reasoning effort new sessions start from. The last effort picked
    /// in the composer; per-session pins keep overriding it.
    #[serde(default)]
    pub selected_reasoning: Option<String>,
    /// Whether thinking traces show as one collapsed row. One switch for
    /// every trace in every session; traces are expanded by default.
    #[serde(default)]
    pub thinking_collapsed: bool,
    /// The Details panel is open. Restored so a resident panel survives a
    /// restart instead of silently closing.
    #[serde(default)]
    pub inspector_open: bool,
    /// The Details panel is pinned beside the chat. A pinned panel ignores
    /// the floating drawer's dismiss gestures.
    #[serde(default)]
    pub inspector_pinned: bool,
    /// Last windowed size; the next launch restores it. `None` keeps the
    /// computed default bounds.
    #[serde(default)]
    pub window_size: Option<WindowSize>,
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
            workspaces: Vec::new(),
            session_workspaces: Vec::new(),
            active_workspace: None,
            recent_models: Vec::new(),
            session_models: Vec::new(),
            selected_reasoning: None,
            thinking_collapsed: false,
            inspector_open: false,
            inspector_pinned: false,
            window_size: None,
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
        if let Some(provider) = &self.selected_provider
            && !valid_pin_part(provider)
        {
            return Err(invalid());
        }
        if let Some(model) = &self.selected_model
            && !valid_pin_part(model)
        {
            return Err(invalid());
        }
        if let Some(reasoning) = &self.selected_reasoning
            && !valid_effort_token(reasoning)
        {
            return Err(invalid());
        }
        if let Some(size) = self.window_size
            && (size.width == 0
                || size.height == 0
                || size.width > MAX_WINDOW_EDGE_PX
                || size.height > MAX_WINDOW_EDGE_PX)
        {
            return Err(invalid());
        }
        validate_pins(&self.recent_models, MAX_RECENT_MODELS)?;
        if self.session_models.len() > MAX_SESSION_MODELS {
            return Err(invalid());
        }
        for (index, pin) in self.session_models.iter().enumerate() {
            if !valid_session_id(&pin.session_id)
                || !valid_pin_part(&pin.provider)
                || !valid_pin_part(&pin.model)
                || pin
                    .reasoning
                    .as_deref()
                    .is_some_and(|level| !valid_effort_token(level))
                || self.session_models[..index]
                    .iter()
                    .any(|earlier| earlier.session_id == pin.session_id)
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

/// Canonical project path for sidebar comparison.
///
/// Trims surrounding whitespace, drops a trailing slash, and on Windows
/// folds slash direction and ASCII case. An empty result means the path was
/// only slashes.
#[must_use]
fn normalize_project_path(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        trimmed.replace('/', "\\").to_ascii_lowercase()
    } else {
        trimmed.to_owned()
    }
}

/// Whether two project paths name the same folder.
///
/// The sidebar and the project lists all use this so a trailing slash (and,
/// on Windows, letter case) cannot split one folder into two.
#[must_use]
pub fn same_project_path(left: &str, right: &str) -> bool {
    let left = normalize_project_path(left);
    let right = normalize_project_path(right);
    !left.is_empty() && left == right
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_SESSION_ID_BYTES && !value.chars().any(char::is_control)
}

/// Reads the UI state.
///
/// A missing document yields the defaults. A document that cannot be parsed
/// or validated is backed up and replaced, keeping whatever still decodes.
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
/// file was republished. Workspaces and recent folders in that backup are
/// not deleted from disk elsewhere; session ledgers stay put.
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
        decode_ui_state,
        UiState::default,
        salvage_ui_state,
        |state| replace_ui_state(home, state),
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
    kind: &'static str,
    #[serde(flatten)]
    state: UiState,
}

fn replace_bytes(state: &UiState) -> Result<Vec<u8>, ConfigError> {
    let document = SerializedUiState {
        format_version: UI_STATE_FORMAT_VERSION,
        kind: UI_STATE_KIND,
        state: state.clone(),
    };
    let mut bytes = serde_json::to_vec_pretty(&document)
        .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn decode_ui_state(bytes: &[u8]) -> Result<UiState, ConfigError> {
    // `deny_unknown_fields` on `UiState` is inert under `#[serde(flatten)]`,
    // so the envelope captures the state keys in a map and the state decodes
    // from that value, where an unknown key is rejected again.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Envelope {
        format_version: u32,
        kind: String,
        #[serde(flatten)]
        fields: serde_json::Map<String, serde_json::Value>,
    }
    let rejection = || {
        ConfigError::authority_rejection()
            .with_detail("ui.json: unknown field, wrong type, or malformed JSON".to_owned())
    };
    let envelope = serde_json::from_slice::<Envelope>(bytes).map_err(|_| rejection())?;
    if envelope.format_version != UI_STATE_FORMAT_VERSION || envelope.kind != UI_STATE_KIND {
        return Err(ConfigError::authority_rejection()
            .with_detail("ui.json: formatVersion or kind does not match this build"));
    }
    let state = serde_json::from_value::<UiState>(serde_json::Value::Object(envelope.fields))
        .map_err(|_| rejection())?;
    state.validate()?;
    Ok(state)
}

/// Extracts the still-valid fields of a document the strict read rejected,
/// so a reset keeps whatever still decodes. `None` when the bytes are not a
/// JSON object or the result still fails validation.
#[must_use]
pub fn salvage_ui_state(document: &serde_json::Value) -> Option<UiState> {
    let object = document.as_object()?;
    let valid = |probe: &UiState| probe.validate().is_ok();
    let mut state = UiState::default();
    for project in crate::document_repair::salvage_list::<String>(object, "recentProjects") {
        if state.recent_projects.len() >= MAX_RECENT_PROJECTS {
            break;
        }
        crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
            probe.recent_projects.push(project);
        });
    }
    crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
        if let Some(value) =
            crate::document_repair::salvage_field::<Option<String>>(object, "lastProject")
        {
            probe.last_project = value;
        }
    });
    crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
        if let Some(value) = crate::document_repair::salvage_field::<bool>(object, "autoUpdate") {
            probe.auto_update = value;
        }
    });
    crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
        if let Some(value) =
            crate::document_repair::salvage_field::<Option<String>>(object, "selectedProvider")
        {
            probe.selected_provider = value;
        }
    });
    crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
        if let Some(value) =
            crate::document_repair::salvage_field::<Option<String>>(object, "selectedModel")
        {
            probe.selected_model = value;
        }
    });
    for entry in crate::document_repair::salvage_list::<(String, String)>(object, "sessionProjects")
    {
        if state.session_projects.len() >= MAX_SESSION_PROJECTS {
            break;
        }
        crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
            probe.session_projects.push(entry);
        });
    }
    for workspace in crate::document_repair::salvage_list::<WorkspaceDef>(object, "workspaces") {
        if state.workspaces.len() >= MAX_WORKSPACES {
            break;
        }
        crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
            probe.workspaces.push(workspace);
        });
    }
    for entry in
        crate::document_repair::salvage_list::<(String, String)>(object, "sessionWorkspaces")
    {
        if state.session_workspaces.len() >= MAX_SESSION_WORKSPACES {
            break;
        }
        crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
            probe.session_workspaces.push(entry);
        });
    }
    crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
        if let Some(value) =
            crate::document_repair::salvage_field::<Option<String>>(object, "activeWorkspace")
        {
            probe.active_workspace = value;
        }
    });
    for pin in crate::document_repair::salvage_list::<ModelPin>(object, "recentModels") {
        if state.recent_models.len() >= MAX_RECENT_MODELS {
            break;
        }
        crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
            probe.recent_models.push(pin);
        });
    }
    for pin in crate::document_repair::salvage_list::<SessionModelPin>(object, "sessionModels") {
        if state.session_models.len() >= MAX_SESSION_MODELS {
            break;
        }
        crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
            probe.session_models.push(pin);
        });
    }
    crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
        if let Some(value) =
            crate::document_repair::salvage_field::<Option<String>>(object, "selectedReasoning")
        {
            probe.selected_reasoning = value;
        }
    });
    crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
        if let Some(value) =
            crate::document_repair::salvage_field::<bool>(object, "thinkingCollapsed")
        {
            probe.thinking_collapsed = value;
        }
    });
    crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
        if let Some(value) = crate::document_repair::salvage_field::<bool>(object, "inspectorOpen")
        {
            probe.inspector_open = value;
        }
    });
    crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
        if let Some(value) =
            crate::document_repair::salvage_field::<bool>(object, "inspectorPinned")
        {
            probe.inspector_pinned = value;
        }
    });
    crate::document_repair::salvage_adopt(&mut state, valid, |probe| {
        if let Some(value) =
            crate::document_repair::salvage_field::<Option<WindowSize>>(object, "windowSize")
        {
            probe.window_size = value;
        }
    });
    valid(&state).then_some(state)
}

#[cfg(test)]
mod tests {
    use super::{MAX_RECENT_MODELS, ModelPin, decode_ui_state, remember_model, salvage_ui_state};

    #[test]
    fn missing_pin_fields_decode_empty() {
        let bytes = br#"{"formatVersion":1,"kind":"mycode-ui-state"}"#;
        let state = decode_ui_state(bytes).expect("ui.json");
        assert!(state.recent_models.is_empty());
        assert!(state.auto_update);
    }

    #[test]
    fn retired_fields_are_document_damage_now() {
        for document in [
            &br#"{"formatVersion":1,"kind":"mycode-ui-state","state":{"starredModels":[{"provider":"openai","model":"gpt"}]}}"#[..],
            &br#"{"formatVersion":1,"kind":"mycode-ui-state","trustedProjects":["/legacy/project"],"autoUpdate":false}"#[..],
            &br#"{"formatVersion":1,"kind":"mycode-ui-state","workspaceRoots":["/legacy/root"]}"#[..],
        ] {
            assert!(
                decode_ui_state(document).is_err(),
                "a removed key is rejected, not silently ignored"
            );
        }
        // The salvage still keeps the fields that are still known.
        let damaged: serde_json::Value = serde_json::from_slice(
            br#"{"formatVersion":1,"kind":"mycode-ui-state","trustedProjects":["/x"],"autoUpdate":false,"workspaceRoots":["/legacy/root"],"selectedModel":"gpt-4o"}"#,
        )
        .expect("fixture");
        let state = salvage_ui_state(&damaged).expect("salvage");
        assert!(!state.auto_update);
        assert_eq!(state.selected_model.as_deref(), Some("gpt-4o"));
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
    fn unknown_and_nested_fields_are_damage_but_pins_stay_validated() {
        let unknown_top = br#"{
            "formatVersion": 1,
            "kind": "mycode-ui-state",
            "autoUpdates": false
        }"#;
        assert!(decode_ui_state(unknown_top).is_err());

        let unknown_nested = br#"{
            "formatVersion": 1,
            "kind": "mycode-ui-state",
            "recentModels": [
                { "provider": "openai", "model": "gpt-4o", "extra": 1 },
                { "provider": "anthropic", "model": "claude-sonnet-4-5" }
            ]
        }"#;
        assert!(decode_ui_state(unknown_nested).is_err());
        // The salvage drops the corrupt pin row but keeps the good ones.
        let damaged: serde_json::Value = serde_json::from_slice(unknown_nested).expect("fixture");
        let salvaged = salvage_ui_state(&damaged).expect("salvage");
        assert_eq!(
            salvaged.recent_models,
            [ModelPin::new("anthropic", "claude-sonnet-4-5")]
        );

        let mut state = super::UiState {
            selected_provider: Some("bad\0provider".to_owned()),
            ..super::UiState::default()
        };
        assert!(state.validate().is_err());

        state.selected_provider = Some("openai".to_owned());
        state.selected_model = Some(String::new());
        assert!(state.validate().is_err());

        state.selected_model = Some("gpt-4o".to_owned());
        state.selected_reasoning = Some("not a valid token!".to_owned());
        assert!(state.validate().is_err());

        state.selected_reasoning = Some("high".to_owned());
        assert!(state.validate().is_ok());

        state.session_models.push(super::SessionModelPin {
            session_id: "ses-1".to_owned(),
            provider: "openai".to_owned(),
            model: "gpt-4o".to_owned(),
            reasoning: None,
        });
        state.session_models.push(super::SessionModelPin {
            session_id: "ses-1".to_owned(),
            provider: "anthropic".to_owned(),
            model: "claude".to_owned(),
            reasoning: None,
        });
        assert!(
            state.validate().is_err(),
            "duplicate session model pins must fail validation"
        );
    }

    #[test]
    fn same_project_path_collapses_a_trailing_slash() {
        assert!(super::same_project_path("/tmp/app/", "/tmp/app"));
        assert!(!super::same_project_path("/tmp/app", "/tmp/other"));
        let path = std::env::temp_dir().join("mycode-path-slash");
        assert!(super::same_project_path(
            &format!("{}/", path.display()),
            &path.to_string_lossy()
        ));
    }

    #[cfg(windows)]
    #[test]
    fn same_project_path_matches_windows_drive_case() {
        assert!(super::same_project_path("C:\\Work\\App\\", "c:/work/app"));
        assert_eq!(
            super::normalize_project_path("C:/Work/App/"),
            "c:\\work\\app"
        );
    }
}
