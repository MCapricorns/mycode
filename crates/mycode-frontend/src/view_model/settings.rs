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

/// The editable settings projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsState {
    /// Revision the editor loaded; `0` when absent.
    pub revision: u64,
    /// Stored proxy preference: empty or a legacy mode word (`auto`, `direct`,
    /// `system`), or a proxy URL.
    pub proxy: String,
    /// The magpie gateway binding (address, key, model, hidden models).
    pub magpie: mycode_app::MagpieSettings,
    /// The one provider row the picker reads: magpie's gateway.
    pub providers: Vec<mycode_app::ProviderSettings>,
    /// MCP servers.
    pub mcp_servers: Vec<mycode_app::McpServerSettings>,
    /// Appearance theme: `dark` or `light`.
    pub theme: String,
    /// UI language: `auto`, `en`, or `zh`.
    pub language: String,
    /// Interface font size: `s`, `m`, `l`, or `xl`.
    pub font_size: String,
    /// Live reasoning effort for the open session's thinking chip.
    /// Session picks write this field and the session pin. They do not
    /// change [`Self::saved_reasoning`], so a later settings save cannot
    /// copy a session overlay into shared `settings.json`.
    pub reasoning: Option<String>,
    /// Reasoning effort stored in the settings document.
    pub saved_reasoning: Option<String>,
    /// MCP key ids (form `mcp-<server>`) that have a stored key.
    pub mcp_with_keys: Vec<String>,
    /// Subagent role enablement, model routes, and thinking overrides.
    pub subagents: mycode_app::SubagentSettings,
    /// Skills switched off on the Skills page.
    pub skills: mycode_app::SkillsSettings,
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
    /// Projects one settings document plus its revision.
    #[must_use]
    pub fn from_settings(settings: &mycode_app::AppSettings, revision: u64) -> Self {
        let providers = if settings.magpie.is_active() {
            vec![mycode_app::ProviderSettings::magpie(
                settings.magpie.endpoint_base(),
                vec![settings.magpie.model.clone()],
                settings.magpie.hidden_models.clone(),
            )]
        } else {
            Vec::new()
        };
        Self {
            revision,
            proxy: settings.proxy.clone(),
            magpie: settings.magpie.clone(),
            providers,
            mcp_servers: settings.mcp_servers.clone(),
            theme: settings.effective_theme().to_owned(),
            language: settings.appearance.language.clone(),
            font_size: settings.effective_font_size().to_owned(),
            reasoning: settings.reasoning_effort.clone(),
            saved_reasoning: settings.reasoning_effort.clone(),
            mcp_with_keys: Vec::new(),
            subagents: settings.subagents.clone(),
            skills: settings.skills.clone(),
            saving: false,
            dirty: false,
            edit_epoch: 0,
        }
    }

    /// Builds the document the editor currently shows.
    #[must_use]
    pub fn to_settings(&self) -> mycode_app::AppSettings {
        mycode_app::AppSettings {
            proxy: self.proxy.clone(),
            magpie: self.magpie.clone(),
            mcp_servers: self.mcp_servers.clone(),
            appearance: mycode_app::AppearanceSettings {
                theme: self.theme.clone(),
                language: self.language.clone(),
                font_size: self.font_size.clone(),
            },
            reasoning_effort: self.saved_reasoning.clone(),
            subagents: self.subagents.clone(),
            skills: self.skills.clone(),
        }
    }
}

/// The Magpie settings sub-page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModelsSubview {
    /// Gateway status and the model list.
    #[default]
    List,
    /// Refreshing the gateway's model list.
    Catalog,
}

/// The MCP settings sub-page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum McpSubview {
    /// Configured servers plus the add buttons.
    #[default]
    List,
    /// Paste a Claude Desktop / Cursor mcp.json.
    Json,
    /// The custom stdio or HTTP server form.
    Custom,
}

/// One settings navigation section (the secondary menu).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsSection {
    /// Theme, language, and network.
    #[default]
    General,
    /// The keys the window binds.
    Shortcuts,
    /// magpie's gateway and the model list it publishes.
    Models,
    /// Subagent roles and per-role model routes.
    Agents,
    /// Slash-command skills from magpie's Library and `.agents`.
    Skills,
    /// MCP servers.
    Mcp,
    /// Data export/import.
    Data,
    /// Version and updates.
    About,
}

impl SettingsSection {
    /// Stable nav identifier.
    pub fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Shortcuts => "shortcuts",
            Self::Models => "models",
            Self::Agents => "agents",
            Self::Skills => "skills",
            Self::Mcp => "mcp",
            Self::Data => "data",
            Self::About => "about",
        }
    }

    /// Nav row label.
    pub fn label(self) -> &'static str {
        match self {
            Self::General => t("General", "通用"),
            Self::Shortcuts => t("Keyboard shortcuts", "快捷键"),
            Self::Models => "Magpie",
            Self::Agents => t("Agents", "子代理"),
            Self::Skills => t("Skills", "技能"),
            Self::Mcp => "MCP",
            Self::Data => t("Data", "数据"),
            Self::About => t("About", "关于"),
        }
    }

    /// One-line description shown in the page header and matched by nav search.
    pub fn hint(self) -> &'static str {
        match self {
            Self::General => t("Appearance, language, and identity", "外观、语言与身份"),
            Self::Shortcuts => t(
                "Keys for sending, steering, and getting around",
                "发送、引导与界面导航的按键",
            ),
            Self::Models => t(
                "magpie's gateway, models, and connection",
                "magpie 网关、模型与连接",
            ),
            Self::Agents => t(
                "Scout, Artisan, and roles you add",
                "Scout、Artisan，以及你添加的角色",
            ),
            Self::Skills => t("Slash commands", "斜杠命令"),
            Self::Mcp => t("Tool servers", "工具服务器"),
            Self::Data => t("Export, import", "导出、导入"),
            Self::About => t("Version, updates", "版本、更新"),
        }
    }

    /// Nav group captions, resolved per language at render time.
    pub fn group_label(group: &'static str) -> &'static str {
        match group {
            "Appearance" => t("General / Appearance", "通用 / 外观"),
            "Models" => "Magpie",
            "Agents" => t("Agents", "子代理"),
            "Tools" => t("Tools / MCP", "工具 / MCP"),
            _ => t("Data & Updates", "数据与更新"),
        }
    }

    /// Nav groups in display order with their member sections.
    pub const GROUPS: &'static [(&'static str, &'static [SettingsSection])] = &[
        ("Appearance", &[Self::General, Self::Shortcuts]),
        ("Models", &[Self::Models]),
        ("Agents", &[Self::Agents]),
        ("Tools", &[Self::Skills, Self::Mcp]),
        ("Data", &[Self::Data, Self::About]),
    ];
}

#[cfg(test)]
mod tests {
    use super::SettingsSection;

    #[test]
    fn font_size_round_trips_through_the_editor_projection() {
        let mut document = mycode_app::AppSettings::default();
        document.appearance.font_size = "xl".to_owned();
        let state = super::SettingsState::from_settings(&document, 1);
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
                SettingsSection::Shortcuts,
                SettingsSection::Models,
                SettingsSection::Agents,
                SettingsSection::Skills,
                SettingsSection::Mcp,
                SettingsSection::Data,
                SettingsSection::About,
            ]
        );
    }
}
