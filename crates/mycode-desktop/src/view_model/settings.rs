//! The editable settings projection and the settings navigation vocabulary.

use crate::i18n::t;

/// One slash-command skill shown in the settings Skills page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillEntry {
    /// Command slug without the leading `/`.
    pub slug: String,
    /// One-line title from the skill heading.
    pub title: String,
    /// Absolute path of the skill markdown.
    pub path: String,
    /// Whether this file came from the user-global `.agents` tree.
    pub global: bool,
}

/// An in-flight OAuth device-flow sign-in shown in the settings UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopilotSignIn {
    /// Code the user types at the verification page.
    pub user_code: String,
    /// Verification page opened in the browser.
    pub verification_uri: String,
}

/// The editable settings projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsState {
    /// Revision the editor loaded; `0` when absent.
    pub revision: u64,
    /// Raw configured User-Agent (empty string means the pi default).
    pub user_agent: String,
    /// Effective User-Agent preview.
    pub effective_user_agent: String,
    /// Configured providers.
    pub providers: Vec<mycode_config::ProviderSettings>,
    /// Configured search backends.
    pub web_backends: Vec<mycode_config::WebBackendSettings>,
    /// MCP servers.
    pub mcp_servers: Vec<mycode_config::McpServerSettings>,
    /// Appearance theme. Always `dark`; light mode is not painted.
    pub theme: String,
    /// Appearance palette. One of `mycode_config::VALID_PALETTES`; slate is the default.
    pub palette: String,
    /// UI language: `auto`, `en`, or `zh`.
    pub language: String,
    /// Requested reasoning effort from the selected model's catalog options;
    /// `None` keeps the provider default.
    pub reasoning: Option<String>,
    /// Provider ids that have a stored API key.
    pub providers_with_keys: Vec<String>,
    /// MCP key ids (form `mcp-<server>`) that have a stored key.
    pub mcp_with_keys: Vec<String>,
    /// Whether durable usage records are written.
    pub usage_enabled: bool,
    /// Subagent role enablement, model routes, and thinking overrides.
    pub subagents: mycode_config::SubagentSettings,
    /// Tool-runtime preferences, including the platform shell.
    pub tools: mycode_config::ToolsSettings,
    /// A save is in flight.
    pub saving: bool,
    /// Unsaved local edits exist.
    pub dirty: bool,
    /// Bumped on every local edit. A save ack clears `dirty` only when this
    /// still matches the epoch captured at dispatch, so edits made while the
    /// save was in flight are not reported as persisted.
    pub edit_epoch: u64,
}

impl SettingsState {
    /// Projects one settings document plus revision and stored key ids.
    #[must_use]
    pub fn from_settings(
        settings: &mycode_config::AppSettings,
        revision: u64,
        providers_with_keys: Vec<String>,
    ) -> Self {
        Self {
            revision,
            user_agent: settings.user_agent.clone(),
            effective_user_agent: settings.effective_user_agent(),
            providers: settings.providers.clone(),
            web_backends: merge_web_backends(&settings.web.backends),
            mcp_servers: settings.mcp_servers.clone(),
            theme: settings.effective_theme().to_owned(),
            palette: settings.effective_palette().to_owned(),
            language: settings.appearance.language.clone(),
            reasoning: settings.reasoning_effort.clone(),
            providers_with_keys,
            mcp_with_keys: Vec::new(),
            usage_enabled: settings.usage.enabled,
            subagents: settings.subagents.clone(),
            tools: settings.tools.clone(),
            saving: false,
            dirty: false,
            edit_epoch: 0,
        }
    }

    /// Builds the document the editor currently shows.
    #[must_use]
    pub fn to_settings(&self) -> mycode_config::AppSettings {
        mycode_config::AppSettings {
            user_agent: self.user_agent.clone(),
            providers: self.providers.clone(),
            web: mycode_config::WebSettings {
                backends: self.web_backends.clone(),
            },
            usage: mycode_config::UsageSettings {
                enabled: self.usage_enabled,
            },
            mcp_servers: self.mcp_servers.clone(),
            appearance: mycode_config::AppearanceSettings {
                theme: self.theme.clone(),
                palette: self.palette.clone(),
                language: self.language.clone(),
            },
            reasoning_effort: self.reasoning.clone(),
            subagents: self.subagents.clone(),
            tools: self.tools.clone(),
        }
    }
}

/// Built-in Querit / AnySearch rows always appear; user backends append.
fn merge_web_backends(
    configured: &[mycode_config::WebBackendSettings],
) -> Vec<mycode_config::WebBackendSettings> {
    let mut backends = mycode_config::builtin_web_backends();
    for backend in configured {
        if let Some(slot) = backends.iter_mut().find(|item| item.id == backend.id) {
            *slot = backend.clone();
        } else {
            backends.push(backend.clone());
        }
    }
    backends
}

/// The Models settings sub-page: provider list, catalog picker, or the
/// custom-endpoint form.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModelsSubview {
    /// Configured providers plus the two add buttons.
    #[default]
    List,
    /// The models.dev catalog picker (search + provider rows).
    Catalog,
    /// The custom endpoint form.
    Custom,
}

/// The Web search settings sub-page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WebSubview {
    /// Vendor rows and custom backends, keys hidden behind a lock.
    #[default]
    List,
    /// The custom backend form.
    Custom,
}

/// The MCP settings sub-page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum McpSubview {
    /// Configured servers plus the add buttons.
    #[default]
    List,
    /// Built-in servers that are not configured yet.
    Catalog,
    /// Paste a Claude Desktop / Cursor mcp.json.
    Json,
    /// The custom stdio or HTTP server form.
    Custom,
}

/// One settings navigation section (the secondary menu).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsSection {
    /// Theme and request identity.
    #[default]
    General,
    /// Providers, catalog presets, and custom endpoints.
    Models,
    /// Subagent roles and per-role model routes.
    Agents,
    /// Slash-command skills from `.agents`.
    Skills,
    /// MCP servers.
    Mcp,
    /// Web search backends.
    Web,
    /// Usage records and data export/import.
    Data,
    /// Version, updates, and the provider catalog.
    About,
}

impl SettingsSection {
    /// Stable nav identifier.
    pub fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Models => "models",
            Self::Agents => "agents",
            Self::Skills => "skills",
            Self::Mcp => "mcp",
            Self::Web => "web",
            Self::Data => "data",
            Self::About => "about",
        }
    }

    /// Nav row label.
    pub fn label(self) -> &'static str {
        match self {
            Self::General => t("General", "通用"),
            Self::Models => t("Models", "模型"),
            Self::Agents => t("Agents", "子代理"),
            Self::Skills => t("Skills", "技能"),
            Self::Mcp => "MCP",
            Self::Web => t("Web search", "网页搜索"),
            Self::Data => t("Data", "数据"),
            Self::About => t("About", "关于"),
        }
    }

    /// Nav row icon.
    pub fn icon(self) -> gpui_kit::assets::IconName {
        use gpui_kit::assets::IconName;
        match self {
            Self::General => IconName::SlidersHorizontal,
            Self::Models => IconName::Bot,
            Self::Agents => IconName::Sparkles,
            Self::Skills => IconName::Terminal,
            Self::Mcp => IconName::PlugZap,
            Self::Web => IconName::Globe,
            Self::Data => IconName::Database,
            Self::About => IconName::Info,
        }
    }

    /// One-line hint under the nav label.
    pub fn hint(self) -> &'static str {
        match self {
            Self::General => t("Theme, identity, shell", "主题、身份、Shell"),
            Self::Models => t("Providers, keys", "服务商、密钥"),
            Self::Agents => t("Roles, models", "角色、模型"),
            Self::Skills => t("Slash commands", "斜杠命令"),
            Self::Mcp => t("Tool servers", "工具服务器"),
            Self::Web => t("Search backends", "搜索后端"),
            Self::Data => t("Usage, export", "用量、导出"),
            Self::About => t("Version, updates", "版本、更新"),
        }
    }

    /// Nav group captions, resolved per language at render time.
    pub fn group_label(group: &'static str) -> &'static str {
        match group {
            "Workspace" => t("Workspace", "工作区"),
            "Connect" => t("Connect", "连接"),
            _ => t("System", "系统"),
        }
    }

    /// Nav groups in display order with their member sections.
    pub const GROUPS: &'static [(&'static str, &'static [SettingsSection])] = &[
        (
            "Workspace",
            &[Self::General, Self::Models, Self::Agents, Self::Skills],
        ),
        ("Connect", &[Self::Mcp, Self::Web]),
        ("System", &[Self::Data, Self::About]),
    ];
}
