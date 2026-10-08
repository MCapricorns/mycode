//! Whole-window state and the action vocabulary the render layer feeds back.

use std::sync::Arc;

use mycode_app::{
    ActiveConversation, AskPrompt, ConversationEntry, PreparedUpdate, SessionSummary,
};

use super::chat::{ComposerMention, LiveJob};
use super::settings::{
    CopilotSignIn, McpSubview, ModelsSubview, SettingsSection, SettingsState, SkillEntry,
    WebSubview,
};
use super::usage::{TurnStats, UsageTotal};

/// The main area view: chat or full-page settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MainView {
    /// Conversation with the agent.
    #[default]
    Chat,
    /// Full-page visual settings.
    Settings,
}

/// Self-update progress shown in settings and banners.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateState {
    /// No check has run yet.
    #[default]
    Idle,
    /// A check is in flight.
    Checking,
    /// The running version is the latest.
    UpToDate,
    /// A newer release is published.
    Available {
        /// New version (no `v` prefix).
        version: String,
        /// Release page URL.
        notes_url: String,
    },
    /// The update is downloading and verifying.
    Downloading {
        /// New version (no `v` prefix).
        version: String,
    },
    /// The staged update is ready; a restart installs it.
    Ready {
        /// New version (no `v` prefix).
        version: String,
    },
    /// The last check or download failed.
    Failed(String),
}

/// Whole-window state.
#[derive(Clone, Debug, Default)]
pub(crate) struct WorkspaceState {
    /// Sidebar sessions, newest-relevant order preserved from the core.
    pub sessions: Vec<SessionSummary>,
    /// Open conversation, when any.
    pub active: Option<ActiveConversation>,
    /// Case-insensitive filter over the titles already in the session list.
    pub session_filter: String,
    /// An older transcript page is in flight for the open conversation.
    pub history_loading: bool,
    /// Composer draft text.
    pub composer_draft: String,
    /// Follow-ups waiting for the in-flight turn to finish.
    pub queued: Vec<String>,
    /// A send is in flight.
    pub sending: bool,
    /// Per-server tool names from the last listing, keyed by server id.
    pub mcp_tools: Vec<(String, Vec<String>)>,
    /// MCP server ids with a tools probe in flight.
    pub mcp_probing: Vec<String>,
    /// Prompt resources for the open session: (name, path).
    pub resources: Vec<(String, String)>,
    /// Slash-command skills from the project and user `.agents` trees.
    pub skills: Vec<SkillEntry>,
    /// Live `agent` runs for the inspector panel.
    pub live_jobs: Vec<LiveJob>,
    /// Call id of the subagent detail window, when open.
    pub subagent_window: Option<String>,
    /// Whether the full working-tree changes drawer is open.
    pub changes_panel_open: bool,
    /// Whether the right inspector is open. It starts closed.
    pub inspector_open: bool,
    /// Whether a wide window docks the inspector instead of covering the chat.
    pub inspector_pinned: bool,
    /// Pending ask rows awaiting user answers.
    pub pending_ask: Option<Vec<AskPrompt>>,
    /// Draft answers aligned with [`Self::pending_ask`].
    pub ask_answers: Vec<String>,
    /// Cumulative usage per provider/model, in first-seen order.
    pub usage_totals: Vec<UsageTotal>,
    /// Most recent committed turn's timing and token metrics.
    pub last_turn: Option<TurnStats>,
    /// Token counts for the turn that is still running, when the provider
    /// has reported any. Cleared when the turn commits or fails.
    pub live_turn: Option<TurnStats>,
    /// Active composer mention autocomplete, when a trigger is typed.
    pub mention: Option<ComposerMention>,
    /// In-flight Copilot device-flow sign-in, when any.
    pub copilot_sign_in: Option<CopilotSignIn>,
    /// The last Copilot sign-in failure, shown in the sign-in panel.
    pub copilot_error: Option<String>,
    /// The editable settings projection.
    pub settings: Option<SettingsState>,
    /// Last terminal error surfaced to the user.
    pub error: Option<String>,
    /// Chat or settings main view.
    pub view: MainView,
    /// Settings navigation section.
    pub settings_section: SettingsSection,
    /// Whether the sidebar project switcher dropdown is open.
    pub project_menu_open: bool,
    /// Whether the sidebar workspace switcher dropdown is open.
    pub workspace_menu_open: bool,
    /// Whether the workspace switcher is in rename mode for the active
    /// workspace.
    pub workspace_rename_open: bool,
    /// The resolved provider catalog.
    pub catalog: Option<Arc<mycode_providers::catalog::CatalogDocument>>,
    /// Unix seconds of the catalog's last successful cloud fetch.
    pub catalog_fetched_at: u64,
    /// Project directory bound to the open session.
    pub project_dir: Option<String>,
    /// Recent project directories, most recent first.
    pub recents: Vec<String>,
    /// Session-to-project bindings (session id, project path), most recent
    /// first. Restores each chat's tool working directory.
    pub session_projects: Vec<(String, String)>,
    /// Named workspaces; the sidebar shows one at a time.
    pub workspaces: Vec<mycode_config::WorkspaceDef>,
    /// Session-to-workspace bindings (session id, workspace id). A session
    /// missing here belongs to the first workspace (upgrade window only).
    pub session_workspaces: Vec<(String, String)>,
    /// Projects allowed to contribute `.mycode/mcp.json`. Not set by opening
    /// a folder.
    pub trusted_projects: Vec<String>,
    /// The workspace the sidebar currently shows.
    pub active_workspace: Option<String>,
    /// Folders in the open workspace, most recently added first. Mirrors the
    /// active workspace's folders so unrelated UI keeps one source.
    pub workspace_roots: Vec<String>,
    /// Whether update checks run automatically.
    pub auto_update: bool,
    /// Self-update progress.
    pub update: UpdateState,
    /// Whether the update dialog (download/install prompt) is open.
    pub update_dialog_open: bool,
    /// The staged update waiting for a restart, when any.
    pub prepared_update: Option<PreparedUpdate>,
    /// The newest release offer, when one is available.
    pub last_offer: Option<mycode_app::UpdateOffer>,
    /// Selected provider id in the model picker.
    pub selected_provider: Option<String>,
    /// Selected model id for the selected provider.
    pub selected_model: Option<String>,
    /// Whether the model picker is open.
    pub model_menu_open: bool,
    /// Provider whose models the open picker is showing. `None` is the
    /// provider step. Does not change the session until a model row is picked.
    pub model_menu_browse: Option<String>,
    /// Filter text for the model step of the picker.
    pub picker_query: String,
    /// Models picked from the session picker, newest first.
    pub recent_models: Vec<mycode_config::ModelPin>,
    /// Models starred in the picker, newest first.
    pub starred_models: Vec<mycode_config::ModelPin>,
    /// Provider id open on the Models page, when the user drilled in.
    pub provider_detail: Option<String>,
    /// Filter text for the settings navigation.
    pub settings_query: String,
    /// Filter text for the add-from-catalog model checklist.
    pub preset_model_query: String,
    /// Whether the thinking-effort submenu is open.
    pub reasoning_menu_open: bool,
    /// Open Agents-page model picker: (role name, field). The field is
    /// `model`.
    pub subagent_menu: Option<(String, String)>,
    /// Whether the Shell page's shell-kind dropdown is open.
    pub shell_kind_menu_open: bool,
    /// Whether the General page's language dropdown is open.
    pub language_menu_open: bool,
    /// Whether the General page's font-family dropdown is open.
    pub font_family_menu_open: bool,
    /// Filter text for the provider preset picker.
    pub preset_search: String,
    /// The catalog provider currently being added, when any.
    pub active_preset: Option<String>,
    /// Models checked in the active preset form; empty means the catalog's
    /// first model is used as the sole default.
    pub preset_models: Vec<String>,
    /// Whether the preset form's model dropdown is open.
    pub preset_model_menu_open: bool,
    /// The Models settings sub-page.
    pub models_subview: ModelsSubview,
    /// The Web search settings sub-page.
    pub web_subview: WebSubview,
    /// The MCP settings sub-page.
    pub mcp_subview: McpSubview,
    /// Whether the custom provider form's protocol dropdown is open.
    pub provider_kind_menu_open: bool,
    /// Whether the custom MCP form's transport dropdown is open.
    pub mcp_transport_menu_open: bool,
}
/// Everything the UI can do to the state.
#[derive(Clone, Debug, PartialEq)]
pub enum DesktopAction {
    /// The core returned the session list.
    SessionsLoaded(Vec<SessionSummary>),
    /// A session was created and opened.
    SessionCreated(SessionSummary),
    /// A session finished recovery and its conversation is ready.
    ConversationOpened(ActiveConversation),
    /// The open session's data was deleted; drop the conversation.
    SessionDeleted,
    /// The open conversation belongs to another folder. Hide it without
    /// deleting the session or stopping its turn.
    ConversationParked,
    /// The composer text changed.
    ComposerChanged(String),
    /// The composer sent; the entry was durably committed.
    MessageSent {
        /// New head spelling.
        head: String,
        /// The committed entry.
        entry: ConversationEntry,
    },
    /// A request failed.
    Failed(String),
    /// Settings loaded from the core.
    SettingsLoaded(SettingsState),
    /// The user picked a UI language (`auto`, `en`, `zh`); applies live and
    /// persists with settings.
    SettingsLanguageSelected(String),
    /// The user picked a color palette; persists with settings.
    SettingsPaletteSelected(String),
    /// The user picked an interface font size (`s`, `m`, `l`, `xl`).
    SettingsFontSizeSelected(String),
    /// The user picked a UI font family. Empty or `system` is the OS UI font.
    SettingsFontFamilySelected(String),
    /// Opens, closes, or pins the right inspector. Layout only.
    InspectorChanged {
        /// Whether the inspector is visible.
        open: bool,
        /// Whether a wide window docks it beside the conversation.
        pinned: bool,
    },
    /// The user dismissed the composer mention menu without picking a row.
    /// The settings editor changed the User-Agent.
    SettingsUserAgentChanged(String),
    /// The settings editor toggled a provider row's enabled flag.
    SettingsProviderToggled(usize, bool),
    /// The settings editor replaced one provider endpoint.
    ///
    /// The value is the trimmed base URL. API keys are not part of this edit.
    SettingsProviderBaseUrlChanged {
        /// Provider id whose endpoint changed.
        id: String,
        /// New base URL, already accepted by settings validation.
        base_url: String,
    },
    /// The settings editor added a provider row.
    SettingsProviderAdded(mycode_config::ProviderSettings),
    /// The settings editor removed a provider row.
    SettingsProviderRemoved(usize),
    /// The settings editor added a web backend row.
    SettingsBackendAdded(mycode_config::WebBackendSettings),
    /// The settings editor removed a web backend row.
    SettingsBackendRemoved(usize),
    /// The settings editor toggled a web backend.
    SettingsBackendToggled(usize, bool),
    /// The settings editor changed subagent role routes.
    SettingsSubagentsChanged(mycode_config::SubagentSettings),
    /// The settings editor changed the platform shell.
    SettingsToolsChanged(mycode_config::ToolsSettings),
    /// The Agents-page provider/model/thinking dropdown opened or closed.
    SubagentMenuToggled(Option<(String, String)>),
    /// The Shell-page shell-kind dropdown opened or closed.
    ShellKindMenuToggled(bool),
    /// The General-page language dropdown opened or closed.
    LanguageMenuToggled(bool),
    /// The General-page font-family dropdown opened or closed.
    FontFamilyMenuToggled(bool),
    /// Unbound sessions inherit the active project (repairs the missing bind).
    /// The settings editor toggled durable usage records.
    SettingsUsageToggled(bool),
    /// The settings editor added an MCP server.
    SettingsMcpAdded(mycode_config::McpServerSettings),
    /// The settings editor removed an MCP server.
    SettingsMcpRemoved(usize),
    /// The settings editor toggled an MCP server.
    SettingsMcpToggled(usize, bool),
    /// A tools probe started for one MCP server (the row shows a spinner
    /// state until the listing or a failure arrives).
    McpProbeStarted(String),
    /// A server's tools listing arrived.
    McpToolsListed {
        server_id: String,
        tools: Vec<String>,
    },
    /// A server's tools probe failed; the row shows the reason inline.
    McpProbeFailed { server_id: String, message: String },
    /// A tool call started on the open conversation.
    ToolStarted {
        call_id: String,
        name: String,
        target: String,
    },
    /// Incremental tool or subagent progress for the live status line.
    ToolProgress {
        call_id: String,
        name: String,
        message: String,
    },
    /// A committed tool-result entry arrived.
    ToolResultAppended(ConversationEntry),
    /// Prompt resources discovered for the open session.
    ResourcesLoaded(Vec<(String, String)>),
    /// Discovered slash-command skills for the current project.
    SkillsLoaded(Vec<SkillEntry>),
    /// File matches for the active `@` mention arrived from the bridge.
    MentionFiles(Vec<String>),
    /// The Copilot device flow started; show the user code.
    CopilotSignInStarted(CopilotSignIn),
    /// The Copilot device flow finished; `Err` keeps the panel with a message.
    CopilotSignInFinished(Result<(), String>),
    /// Live token counts for the turn that is still running.
    UsageSnapshot {
        model: String,
        input: u64,
        output: u64,
        cache: Option<u64>,
        elapsed_ms: u64,
    },
    /// A durable usage record arrived.
    UsageRecorded {
        provider: String,
        model: String,
        input: u64,
        output: u64,
        cache: Option<u64>,
        elapsed_ms: u64,
        entry: ConversationEntry,
    },
    /// The agent asked the user structured questions.
    AskRequested(Vec<AskPrompt>),
    /// The user picked one choice on a pending ask question.
    AskChoicePicked { index: usize, answer: String },
    /// The user submitted answers locally; clear the pending panel.
    AskAnswered,
    /// The sidebar session-title filter changed.
    SessionFilterChanged(String),
    /// An older-page read started.
    HistoryLoadStarted,
    /// An older-page read finished without changing the transcript.
    HistoryLoadFinished,
    /// An older page arrived. Applied only when `requested_before` is still
    /// the open window's cursor.
    OlderLoaded {
        /// Session the page belongs to.
        session_id: String,
        /// Entries in ledger order, strictly before the cursor.
        entries: Vec<ConversationEntry>,
        /// Next backward cursor, or `None` at the branch start.
        older_before: Option<String>,
        /// Cursor the UI asked for.
        requested_before: String,
    },
    /// Settings were persisted under CAS.
    ///
    /// `edit_epoch` is the epoch captured when the save was dispatched. The
    /// reducer clears `dirty` only when the editor has not changed since.
    SettingsSaved {
        /// Revision the store assigned.
        revision: u64,
        /// Editor epoch at dispatch.
        edit_epoch: u64,
    },
    /// A settings save failed. The document stays dirty and a chat turn's
    /// `sending` flag is left alone.
    SettingsSaveFailed(String),
    /// One provider's API key was stored or cleared; refreshes key markers.
    ProviderKeySaved {
        /// Provider ids with a stored key.
        provider_keys: Vec<String>,
        /// MCP server ids with a stored key.
        mcp_keys: Vec<String>,
    },
    /// The user queued a follow-up while a turn is in flight.
    MessageQueued(String),
    /// The user dismissed one queued follow-up.
    QueuedMessageRemoved(usize),
    /// The next queued follow-up was taken to send.
    QueuedMessageTaken,
    /// One queued follow-up moved to the front so an interrupt can send it.
    QueuedMessagePromoted(usize),
    /// Opens or closes the subagent detail window.
    SubagentWindowChanged(Option<String>),
    /// The user closed one running subagent from its row.
    SubagentDismissed(String),
    /// Opens or closes the full changes drawer.
    ChangesPanelToggled(bool),
    /// The user sent a prompt; show a working status before the first token.
    TurnArmed,
    /// Incremental assistant text from the active model turn.
    ChatDelta(String),
    /// Incremental assistant reasoning from the active model turn.
    ChatThinkingDelta(String),
    /// An intermediate assistant step (with tool calls) was committed; the
    /// streamed text so far belongs to it, so the live bubble resets.
    AssistantStepCommitted(ConversationEntry),
    /// The model turn finished and its entry was committed.
    ChatDone {
        head: String,
        entry: ConversationEntry,
    },
    /// The model turn failed without committing anything.
    ChatFailed(String),
    /// Clear the surfaced error.
    /// Switch the main area between chat and settings.
    ShowMainView(MainView),
    /// Switch the settings secondary menu.
    ShowSettingsSection(SettingsSection),
    /// The sidebar project switcher opened or closed.
    ProjectMenuToggled(bool),
    /// The provider catalog resolved (bundled or cloud).
    CatalogLoaded {
        /// The catalog document.
        document: Arc<mycode_providers::catalog::CatalogDocument>,
        /// Unix seconds of the last cloud fetch.
        fetched_at: u64,
    },
    /// The durable UI state loaded.
    UiStateLoaded {
        /// Recent project directories.
        recents: Vec<String>,
        /// Last opened project directory.
        last_project: Option<String>,
        /// Whether update checks run automatically.
        auto_update: bool,
        /// Last selected provider id.
        selected_provider: Option<String>,
        /// Last selected model id.
        selected_model: Option<String>,
        /// Session-to-project bindings.
        session_projects: Vec<(String, String)>,
        /// Named workspaces.
        workspaces: Vec<mycode_config::WorkspaceDef>,
        /// Session-to-workspace bindings.
        session_workspaces: Vec<(String, String)>,
        /// Projects trusted for project MCP configuration.
        trusted_projects: Vec<String>,
        /// The workspace the sidebar shows.
        active_workspace: Option<String>,
        /// Recent model pins, newest first.
        recent_models: Vec<mycode_config::ModelPin>,
        /// Starred model pins, newest first.
        starred_models: Vec<mycode_config::ModelPin>,
    },
    /// The sidebar workspace switcher opened or closed.
    WorkspaceMenuToggled(bool),
    /// A workspace was created and became the active one.
    WorkspaceCreated(mycode_config::WorkspaceDef),
    /// The sidebar switched to another workspace.
    WorkspaceSwitched(String),
    /// The active workspace was renamed.
    WorkspaceRenamed(String),
    /// A workspace was removed; its sessions move to the first remaining
    /// workspace.
    WorkspaceRemoved(String),
    /// A session joined a workspace in the durable map.
    SessionWorkspaceBound {
        /// Session identity spelling.
        session_id: String,
        /// Workspace identity.
        workspace_id: String,
    },
    /// Every remembered binding for one session was dropped (its data was
    /// deleted).
    SessionBindingsForgotten(String),
    /// The workspace rename editor opened or closed.
    WorkspaceRenameToggled(bool),
    /// A folder was added to the workspace.
    WorkspaceRootAdded(String),
    /// A folder was removed from the workspace.
    WorkspaceRootRemoved(String),
    /// A project directory was bound to the open session.
    ProjectOpened(String),
    /// A session was bound to a project in the durable map.
    ///
    /// The first folder sticks. A later bind to a different folder is ignored
    /// so opening another directory cannot move a chat that already has one.
    SessionProjectBound {
        /// Session identity spelling.
        session_id: String,
        /// Project directory path.
        project: String,
    },
    /// The open chat's working directory moved to another folder in this
    /// workspace. The other folders stay members of the same workspace.
    WorkspaceFolderFocused {
        /// Session identity spelling.
        session_id: String,
        /// Folder that becomes the session working directory.
        project: String,
    },
    /// The active project filter changed (sidebar project switcher).
    ActiveProjectChanged(Option<String>),
    /// A remembered project was removed from the recent list.
    RecentRemoved(String),
    /// The model picker selected a provider.
    ProviderSelected(String),
    /// The model picker selected a model; an unknown model joins the
    /// provider row so the next turn can use it.
    ModelSelected(String),
    /// The model picker opened or closed. Opening starts on the provider step.
    ModelMenuToggled(bool),
    /// The open picker moved into one provider, or back to the provider step
    /// when the id is absent. Does not itself open the picker.
    ModelMenuBrowse(Option<String>),
    /// The model step's search box changed.
    PickerQueryChanged(String),
    /// The user starred or unstarred one model.
    ModelStarToggled {
        /// Provider id.
        provider: String,
        /// Model id.
        model: String,
    },
    /// The settings navigation filter changed.
    SettingsQueryChanged(String),
    /// The Models page opened or closed one provider's detail.
    ProviderDetailOpened(Option<String>),
    /// The add-from-catalog model checklist filter changed.
    PresetModelQueryChanged(String),
    /// The thinking-effort submenu opened or closed.
    ReasoningMenuToggled(bool),
    /// The composer's thinking-effort pick; persists through settings.
    SettingsReasoningChanged(String),
    /// The preset picker filter changed.
    PresetSearchChanged(String),
    /// A preset form opened or closed.
    ActivePresetChanged(Option<String>),
    /// The active preset form toggled one model's checkbox.
    PresetModelToggled(String),
    /// The preset form's model dropdown opened or closed.
    PresetModelMenuToggled(bool),
    /// The Models settings sub-page changed.
    ShowModelsSubview(ModelsSubview),
    /// The Web search settings sub-page changed.
    ShowWebSubview(WebSubview),
    /// The MCP settings sub-page changed.
    ShowMcpSubview(McpSubview),
    /// The custom provider form's protocol dropdown opened or closed.
    ProviderKindMenuToggled(bool),
    /// The custom MCP form's transport dropdown opened or closed.
    McpTransportMenuToggled(bool),
    /// Self-update progress changed.
    UpdateStateChanged(UpdateState),
    /// A release offer was resolved for the available update.
    UpdateOfferFound(mycode_app::UpdateOffer),
    /// The auto-update preference changed.
    AutoUpdateToggled(bool),
    /// A verified update is staged and waiting for a restart.
    UpdateStaged(PreparedUpdate),
    /// The update dialog opened or closed.
    UpdateDialogToggled(bool),
}

/// Maximum composer text before the send is rejected locally.
pub(crate) const MAX_COMPOSER_CHARS: usize = 64 * 1024;
/// Follow-ups waiting behind one in-flight turn.
pub(crate) const MAX_QUEUED_MESSAGES: usize = 8;
