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
    /// Interface font size: `s`, `m`, `l`, or `xl`.
    pub font_size: String,
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
            font_size: settings.effective_font_size().to_owned(),
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
                font_size: self.font_size.clone(),
            },
            reasoning_effort: self.reasoning.clone(),
            subagents: self.subagents.clone(),
            tools: self.tools.clone(),
        }
    }

    /// Checks a replacement provider base URL with [`mycode_config::AppSettings::validate`].
    ///
    /// Returns the trimmed URL. Does not mutate this projection: the editor
    /// applies the change only after this succeeds, then the existing settings
    /// save persists it. The document has no secrets, and the validation
    /// detail names the rule rather than the typed value.
    pub fn preview_provider_base_url(&self, id: &str, base_url: &str) -> Result<String, String> {
        let base_url = base_url.trim().to_owned();
        let mut document = self.to_settings();
        let Some(provider) = document
            .providers
            .iter_mut()
            .find(|provider| provider.id == id)
        else {
            return Err("that provider is no longer in settings".to_owned());
        };
        provider.base_url = base_url.clone();
        document
            .validate()
            .map_err(|error| format!("invalid settings: {}", error.summary()))?;
        Ok(base_url)
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
    /// Platform shell used by tool scripts.
    Shell,
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
            Self::Shell => "shell",
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
            Self::Shell => t("Shell", "Shell"),
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
            Self::Shell => IconName::SquareTerminal,
            Self::Mcp => IconName::PlugZap,
            Self::Web => IconName::Globe,
            Self::Data => IconName::Database,
            Self::About => IconName::Info,
        }
    }

    /// One-line description shown in the page header and matched by nav search.
    pub fn hint(self) -> &'static str {
        match self {
            Self::General => t("Appearance, language, and identity", "外观、语言与身份"),
            Self::Models => t("Default model and providers", "默认模型与服务商"),
            Self::Agents => t(
                "Scout, Artisan, and roles you add",
                "Scout、Artisan，以及你添加的角色",
            ),
            Self::Skills => t("Slash commands", "斜杠命令"),
            Self::Shell => t("pwsh or Git bash", "pwsh 或 Git bash"),
            Self::Mcp => t("Tool servers", "工具服务器"),
            Self::Web => t("Search backends", "搜索后端"),
            Self::Data => t("Usage, export", "用量、导出"),
            Self::About => t("Version, updates", "版本、更新"),
        }
    }

    /// Nav group captions, resolved per language at render time.
    pub fn group_label(group: &'static str) -> &'static str {
        match group {
            "Appearance" => t("General / Appearance", "通用 / 外观"),
            "Models" => t("Models & Providers", "模型与服务商"),
            "Agents" => t("Agents", "子代理"),
            "Tools" => t("Tools / MCP / Web", "工具 / MCP / 网页"),
            _ => t("Data & Updates", "数据与更新"),
        }
    }

    /// Nav groups in display order with their member sections.
    pub const GROUPS: &'static [(&'static str, &'static [SettingsSection])] = &[
        ("Appearance", &[Self::General]),
        ("Models", &[Self::Models]),
        ("Agents", &[Self::Agents]),
        ("Tools", &[Self::Skills, Self::Shell, Self::Mcp, Self::Web]),
        ("Data", &[Self::Data, Self::About]),
    ];
}

#[cfg(test)]
mod tests {
    use super::SettingsSection;

    #[test]
    fn provider_endpoint_edit_reuses_settings_validation() {
        let mut document = mycode_config::AppSettings::default();
        document.providers.push(mycode_config::ProviderSettings {
            id: "gateway".to_owned(),
            kind: "openai-completions".to_owned(),
            base_url: "https://api.example.com/v1".to_owned(),
            models: vec!["m".to_owned()],
            enabled: true,
            context_limit: None,
            max_output: None,
        });
        let state = super::SettingsState::from_settings(&document, 3, vec!["gateway".to_owned()]);
        let rejected = state
            .preview_provider_base_url("gateway", "http://api.example.com/v1")
            .expect_err("http endpoints are rejected");
        assert!(rejected.contains("https://"), "{rejected}");
        assert!(!rejected.contains("http://api.example.com"));
        let accepted = state
            .preview_provider_base_url("gateway", " https://gateway.example/v1 ")
            .expect("https endpoint");
        assert_eq!(accepted, "https://gateway.example/v1");
        assert_eq!(state.providers[0].base_url, "https://api.example.com/v1");
        assert_eq!(state.providers_with_keys, vec!["gateway".to_owned()]);

        let mut vm = super::super::WorkspaceState {
            settings: Some(state),
            ..super::super::WorkspaceState::default()
        };
        super::super::reduce(
            &mut vm,
            super::super::DesktopAction::SettingsProviderBaseUrlChanged {
                id: "missing".to_owned(),
                base_url: "https://other.example/v1".to_owned(),
            },
        );
        assert!(!vm.settings.as_ref().expect("settings").dirty);
        super::super::reduce(
            &mut vm,
            super::super::DesktopAction::SettingsProviderBaseUrlChanged {
                id: "gateway".to_owned(),
                base_url: accepted,
            },
        );
        let settings = vm.settings.expect("settings");
        assert!(settings.dirty);
        assert_eq!(settings.providers[0].base_url, "https://gateway.example/v1");
        assert_eq!(settings.providers[0].models, vec!["m".to_owned()]);
        assert_eq!(settings.providers_with_keys, vec!["gateway".to_owned()]);
        assert!(settings.to_settings().validate().is_ok());
    }

    #[test]
    fn font_size_round_trips_through_the_editor_projection() {
        let mut document = mycode_config::AppSettings::default();
        document.appearance.font_size = "xl".to_owned();
        let state = super::SettingsState::from_settings(&document, 1, Vec::new());
        assert_eq!(state.font_size, "xl");
        let stored = state.to_settings();
        assert_eq!(stored.appearance.font_size, "xl");
        assert!(stored.validate().is_ok());
    }

    #[test]
    fn nav_groups_match_the_settings_shell() {
        let ids: Vec<_> = SettingsSection::GROUPS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, ["Appearance", "Models", "Agents", "Tools", "Data"]);
        let mut members = Vec::new();
        for (_, sections) in SettingsSection::GROUPS {
            members.extend(sections.iter().copied());
        }
        assert_eq!(
            members,
            [
                SettingsSection::General,
                SettingsSection::Models,
                SettingsSection::Agents,
                SettingsSection::Skills,
                SettingsSection::Shell,
                SettingsSection::Mcp,
                SettingsSection::Web,
                SettingsSection::Data,
                SettingsSection::About,
            ]
        );
    }
}
