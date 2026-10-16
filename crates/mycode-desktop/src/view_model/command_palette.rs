//! Global command palette catalog.
//!
//! The list is a pure projection of [`WorkspaceState`]: sessions, models the
//! picker can already select, slash commands, and settings pages. The GPUI
//! command widget filters that list. A case-insensitive match on the label
//! and keywords is what a query is expected to hit.

use std::collections::HashSet;

use super::chat::COMPOSER_COMMANDS;
use super::projects::{project_of_session, workspace_of_session};
use super::settings::SettingsSection;
use super::state::{MainView, WorkspaceState};
use crate::i18n::t;

/// One thing the palette can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaletteCommand {
    /// Start a task in the open folder.
    NewSession,
    /// Open a session that can be read.
    OpenSession(String),
    /// Switch the session model to one enabled provider's model.
    SelectModel { provider: String, model: String },
    /// The same token the composer `/` menu accepts.
    Slash(String),
    /// Open one settings page.
    OpenSettings(SettingsSection),
}

/// Which block a row belongs to. Order is the palette order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaletteGroupId {
    Sessions,
    Models,
    Commands,
    Settings,
}

/// One searchable row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaletteItem {
    pub command: PaletteCommand,
    pub label: String,
    /// Extra terms matched besides [`Self::label`]. Both languages are
    /// included so a search works after the UI language flips.
    pub keywords: Vec<String>,
    /// The row that is already in effect (open session, current model, page).
    pub checked: bool,
}

/// One headed block of rows. Empty blocks are omitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaletteGroup {
    pub id: PaletteGroupId,
    pub items: Vec<PaletteItem>,
}

/// The full palette, before the query filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaletteCatalog {
    pub groups: Vec<PaletteGroup>,
}

/// Builds the palette from the current desk. Corrupt sessions and disabled
/// providers are left out: those rows cannot be opened or selected.
#[must_use]
pub(crate) fn palette_catalog(state: &WorkspaceState) -> PaletteCatalog {
    let mut groups = Vec::new();
    push_group(&mut groups, PaletteGroupId::Sessions, session_items(state));
    push_group(&mut groups, PaletteGroupId::Models, model_items(state));
    push_group(&mut groups, PaletteGroupId::Commands, command_items(state));
    push_group(&mut groups, PaletteGroupId::Settings, settings_items(state));
    PaletteCatalog { groups }
}

/// The row at one unfiltered group/item coordinate.
///
/// The command widget reports the same coordinates after a query filter, so
/// confirm can resolve the row without storing the filtered list.
#[must_use]
pub(crate) fn palette_item(
    catalog: &PaletteCatalog,
    section: usize,
    row: usize,
) -> Option<&PaletteItem> {
    catalog
        .groups
        .get(section)
        .and_then(|group| group.items.get(row))
}

/// Whether a row matches a query. Empty matches everything. The comparison
/// is case-insensitive and covers the label and the keywords, matching the
/// command widget's own filter.
#[cfg(test)]
#[must_use]
fn palette_matches(item: &PaletteItem, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let query = query.to_lowercase();
    item.label.to_lowercase().contains(&query)
        || item
            .keywords
            .iter()
            .any(|keyword| keyword.to_lowercase().contains(&query))
}

fn push_group(groups: &mut Vec<PaletteGroup>, id: PaletteGroupId, items: Vec<PaletteItem>) {
    if items.is_empty() {
        return;
    }
    groups.push(PaletteGroup { id, items });
}

fn session_items(state: &WorkspaceState) -> Vec<PaletteItem> {
    let open_id = state
        .active
        .as_ref()
        .map(|conversation| conversation.session_id.as_str());
    let mut items = vec![PaletteItem {
        command: PaletteCommand::NewSession,
        label: t("New task", "新建任务").to_owned(),
        keywords: words(&["new", "session", "task", "新建", "会话", "任务"]),
        checked: false,
    }];
    for session in &state.sessions {
        if session.corrupt {
            continue;
        }
        let project = project_of_session(&state.session_projects, &session.session_id);
        let workspace_name =
            workspace_of_session(state, &session.session_id).map(|desk| desk.name.as_str());
        let title = if !session.title.trim().is_empty() {
            one_line(session.title.trim())
        } else if let Some(project) = project {
            folder_name(project)
        } else {
            t("New task", "新建任务").to_owned()
        };
        let mut keywords = words(&["session", "会话"]);
        keywords.push(session.session_id.clone());
        if let Some(project) = project {
            keywords.push(folder_name(project));
        }
        if let Some(name) = workspace_name {
            keywords.push(name.to_owned());
        }
        items.push(PaletteItem {
            command: PaletteCommand::OpenSession(session.session_id.clone()),
            label: title,
            keywords,
            checked: open_id == Some(session.session_id.as_str()),
        });
    }
    items
}

fn model_items(state: &WorkspaceState) -> Vec<PaletteItem> {
    let Some(settings) = state.settings.as_ref() else {
        return Vec::new();
    };
    let enabled: Vec<&mycode_config::ProviderSettings> = settings
        .providers
        .iter()
        .filter(|provider| provider.enabled)
        .collect();
    if enabled.is_empty() {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    let mut push = |provider: &str, model: &str| {
        // A hidden model stays out of the palette as it does the picker.
        let offered = enabled
            .iter()
            .any(|entry| entry.id == provider && entry.shows_model(model));
        if !offered || model.is_empty() {
            return;
        }
        if !seen.insert((provider.to_owned(), model.to_owned())) {
            return;
        }
        let provider_name = provider_name(state, provider);
        let model_name = model_name(state, provider, model);
        let mut keywords = words(&["model", "模型"]);
        keywords.push(provider.to_owned());
        keywords.push(provider_name.clone());
        keywords.push(model.to_owned());
        if model_name != model {
            keywords.push(model_name.clone());
        }
        let checked = state.selected_provider.as_deref() == Some(provider)
            && state.selected_model.as_deref() == Some(model);
        items.push(PaletteItem {
            command: PaletteCommand::SelectModel {
                provider: provider.to_owned(),
                model: model.to_owned(),
            },
            label: format!("{model_name} · {provider_name}"),
            keywords,
            checked,
        });
    };
    for pin in &state.recent_models {
        push(&pin.provider, &pin.model);
    }
    for provider in &enabled {
        for model in &provider.models {
            push(&provider.id, model);
        }
    }
    items
}

fn command_items(state: &WorkspaceState) -> Vec<PaletteItem> {
    let mut items = Vec::new();
    for (name, _) in COMPOSER_COMMANDS {
        let (english, chinese) = command_copy(name);
        let mut keywords = words(&["command", "命令", english, chinese]);
        keywords.push(name.trim_start_matches('/').to_owned());
        items.push(PaletteItem {
            command: PaletteCommand::Slash((*name).to_owned()),
            label: format!("{name} · {}", t(english, chinese)),
            keywords,
            checked: false,
        });
    }
    for skill in &state.enabled_skills() {
        let slug = skill.slug.trim();
        if slug.is_empty() {
            continue;
        }
        let mut keywords = words(&["skill", "技能", "command", "命令"]);
        keywords.push(slug.to_owned());
        if !skill.title.is_empty() {
            keywords.push(skill.title.clone());
        }
        items.push(PaletteItem {
            command: PaletteCommand::Slash(format!("/{slug}")),
            label: format!("/{slug} · {}", one_line(&skill.title)),
            keywords,
            checked: false,
        });
    }
    let servers = state
        .settings
        .as_ref()
        .map(|settings| {
            settings
                .mcp_servers
                .iter()
                .filter(|server| server.enabled)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for server in servers {
        let mut keywords = words(&["mcp", "command", "命令"]);
        keywords.push(server.id.clone());
        items.push(PaletteItem {
            command: PaletteCommand::Slash(format!("mcp:{}", server.id)),
            label: format!("/{} · {}", server.id, t("MCP server", "MCP 服务")),
            keywords,
            checked: false,
        });
        let Some(tools) = state
            .mcp_tools
            .iter()
            .find(|(id, _)| id == &server.id)
            .map(|(_, tools)| tools.clone())
        else {
            continue;
        };
        for tool in tools {
            if tool.is_empty() {
                continue;
            }
            let mut keywords = words(&["mcp", "tool", "工具", "command", "命令"]);
            keywords.push(server.id.clone());
            keywords.push(tool.clone());
            items.push(PaletteItem {
                command: PaletteCommand::Slash(format!("mcp:{}/{}", server.id, tool)),
                label: format!("/{}/{} · {}", server.id, tool, t("MCP tool", "MCP 工具")),
                keywords,
                checked: false,
            });
        }
    }
    items
}

fn settings_items(state: &WorkspaceState) -> Vec<PaletteItem> {
    let mut items = Vec::new();
    for (_, sections) in SettingsSection::GROUPS {
        for section in *sections {
            let mut keywords = words(section_keywords(*section));
            keywords.push(section.id().to_owned());
            keywords.extend(words(&["settings", "设置"]));
            items.push(PaletteItem {
                command: PaletteCommand::OpenSettings(*section),
                label: section.label().to_owned(),
                keywords,
                checked: state.view == MainView::Settings && state.settings_section == *section,
            });
        }
    }
    items
}

fn command_copy(name: &str) -> (&'static str, &'static str) {
    match name {
        "/new" => ("new chat", "新对话"),
        "/compact" => ("compact context", "压缩上下文"),
        "/settings" => ("settings", "设置"),
        _ => ("command", "命令"),
    }
}

fn section_keywords(section: SettingsSection) -> &'static [&'static str] {
    match section {
        SettingsSection::General => &[
            "general",
            "通用",
            "appearance",
            "外观",
            "language",
            "语言",
            "identity",
            "身份",
            "theme",
            "主题",
            "font",
            "字体",
        ],
        SettingsSection::Models => &[
            "models",
            "模型",
            "providers",
            "服务商",
            "default model",
            "默认模型",
        ],
        SettingsSection::Agents => &["agents", "子代理", "scout", "artisan", "roles", "角色"],
        SettingsSection::Skills => &["skills", "技能", "slash", "斜杠"],
        SettingsSection::Mcp => &["mcp", "tool servers", "工具服务器"],
        SettingsSection::Web => &["web", "search", "网页", "搜索"],
        SettingsSection::Data => &["data", "数据", "export", "import", "导出", "导入"],
        SettingsSection::About => &["about", "关于", "version", "updates", "版本", "更新"],
    }
}

fn provider_name(state: &WorkspaceState, id: &str) -> String {
    state
        .catalog
        .as_ref()
        .map(|catalog| catalog.display_name(id))
        .unwrap_or_else(|| id.to_owned())
}

fn model_name(state: &WorkspaceState, provider: &str, model_id: &str) -> String {
    state
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.model(provider, model_id))
        .map(|model| {
            if model.name.is_empty() {
                model.id.clone()
            } else {
                model.name.clone()
            }
        })
        .unwrap_or_else(|| model_id.to_owned())
}

fn folder_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_owned())
}

fn one_line(text: &str) -> String {
    text.chars()
        .take(80)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

fn words(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::{palette_catalog, palette_item, palette_matches};
    use crate::view_model::{
        DesktopAction, MainView, PaletteCommand, PaletteGroupId, SettingsSection, SettingsState,
        SkillEntry, WorkspaceState, reduce,
    };

    fn provider(id: &str, model: &str, enabled: bool) -> mycode_config::ProviderSettings {
        mycode_config::ProviderSettings {
            id: id.to_owned(),
            kind: "openai-completions".to_owned(),
            base_url: "https://api.example.com/v1".to_owned(),
            models: vec![model.to_owned()],
            enabled,
            context_limit: None,
            max_output: None,
            hidden_models: Vec::new(),
        }
    }

    fn session(id: &str, title: &str, corrupt: bool) -> crate::view_model::SessionSummary {
        crate::view_model::SessionSummary {
            session_id: id.to_owned(),
            root_branch_id: "br".to_owned(),
            title: title.to_owned(),
            updated_at_ms: None,
            event_count: 1,
            active: false,
            corrupt,
        }
    }

    fn populated() -> WorkspaceState {
        let mut document = mycode_config::AppSettings::default();
        document.providers.push(provider("openai", "gpt-5", true));
        document
            .providers
            .push(provider("off", "hidden-model", false));
        document.mcp_servers.push(mycode_config::McpServerSettings {
            id: "docs".to_owned(),
            enabled: true,
            transport: "stdio".to_owned(),
            command: None,
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            endpoint: None,
            key_header: None,
        });
        WorkspaceState {
            settings: Some(SettingsState::from_settings(&document, 1, Vec::new())),
            sessions: vec![
                session("ses-live", "Fix the parser", false),
                session("ses-bad", "Broken", true),
            ],
            skills: vec![SkillEntry {
                slug: "review".to_owned(),
                title: "Review diff".to_owned(),
                path: "/tmp/review/SKILL.md".to_owned(),
                global: false,
            }],
            mcp_tools: vec![("docs".to_owned(), vec!["search".to_owned()])],
            selected_provider: Some("openai".to_owned()),
            selected_model: Some("gpt-5".to_owned()),
            recent_models: vec![mycode_config::ModelPin::new("openai", "gpt-5")],
            ..WorkspaceState::default()
        }
    }

    fn labels_matching(state: &WorkspaceState, query: &str) -> Vec<String> {
        palette_catalog(state)
            .groups
            .into_iter()
            .flat_map(|group| group.items)
            .filter(|item| palette_matches(item, query))
            .map(|item| item.label)
            .collect()
    }

    #[test]
    fn catalog_covers_sessions_models_commands_and_settings() {
        let catalog = palette_catalog(&populated());
        let ids: Vec<_> = catalog.groups.iter().map(|group| group.id).collect();
        assert_eq!(
            ids,
            [
                PaletteGroupId::Sessions,
                PaletteGroupId::Models,
                PaletteGroupId::Commands,
                PaletteGroupId::Settings,
            ]
        );
        let sessions = &catalog.groups[0].items;
        assert!(matches!(sessions[0].command, PaletteCommand::NewSession));
        assert!(sessions.iter().any(|item| matches!(
            &item.command,
            PaletteCommand::OpenSession(id) if id == "ses-live"
        )));
        assert!(!sessions.iter().any(|item| matches!(
            &item.command,
            PaletteCommand::OpenSession(id) if id == "ses-bad"
        )));

        let models = &catalog.groups[1].items;
        assert!(models.iter().any(|item| matches!(
            &item.command,
            PaletteCommand::SelectModel { model, .. } if model == "gpt-5"
        )));
        assert!(models.iter().any(|item| item.checked));
        assert!(!models.iter().any(|item| matches!(
            &item.command,
            PaletteCommand::SelectModel { model, .. } if model == "hidden-model"
        )));

        let commands = &catalog.groups[2].items;
        assert!(commands.iter().any(|item| matches!(
            &item.command,
            PaletteCommand::Slash(insert) if insert == "/new"
        )));
        assert!(commands.iter().any(|item| matches!(
            &item.command,
            PaletteCommand::Slash(insert) if insert == "/compact"
        )));
        assert!(commands.iter().any(|item| matches!(
            &item.command,
            PaletteCommand::Slash(insert) if insert == "/settings"
        )));
        assert!(commands.iter().any(|item| matches!(
            &item.command,
            PaletteCommand::Slash(insert) if insert == "/review"
        )));
        assert!(commands.iter().any(|item| matches!(
            &item.command,
            PaletteCommand::Slash(insert) if insert == "mcp:docs"
        )));
        assert!(commands.iter().any(|item| matches!(
            &item.command,
            PaletteCommand::Slash(insert) if insert == "mcp:docs/search"
        )));

        let settings = &catalog.groups[3].items;
        assert_eq!(
            settings.len(),
            SettingsSection::GROUPS
                .iter()
                .map(|(_, sections)| sections.len())
                .sum::<usize>()
        );
        assert!(settings.iter().any(|item| matches!(
            item.command,
            PaletteCommand::OpenSettings(SettingsSection::Mcp)
        )));
    }

    #[test]
    fn switched_off_skill_leaves_the_palette_and_marks_settings_dirty() {
        let mut state = populated();
        reduce(
            &mut state,
            DesktopAction::SettingsSkillToggled("review".to_owned(), false),
        );
        let settings = state.settings.as_ref().expect("settings");
        assert!(settings.dirty);
        assert_eq!(settings.to_settings().skills.disabled, ["review"]);
        assert!(state.enabled_skills().is_empty());
        assert_eq!(state.skills.len(), 1, "the Skills page still lists it");
        assert!(
            !labels_matching(&state, "/review")
                .iter()
                .any(|label| label.starts_with("/review"))
        );

        reduce(
            &mut state,
            DesktopAction::SettingsSkillToggled("review".to_owned(), true),
        );
        assert_eq!(state.enabled_skills().len(), 1);
        assert!(
            labels_matching(&state, "/review")
                .iter()
                .any(|label| label.starts_with("/review"))
        );
    }

    #[test]
    fn search_is_case_insensitive_across_labels_and_keywords() {
        let state = populated();
        let parser = labels_matching(&state, "PARSER");
        assert!(parser.iter().any(|label| label.contains("parser")));
        let model = labels_matching(&state, "gpt-5");
        assert!(model.iter().any(|label| label.contains("gpt-5")));
        let settings = labels_matching(&state, "设置");
        assert!(settings.iter().any(|label| label.contains("设置")
            || label.contains("General")
            || label.contains("通用")
            || label.contains("/settings")));
        assert!(
            labels_matching(&state, "slash")
                .iter()
                .any(|label| label.contains("/review")
                    || label.contains("Skills")
                    || label.contains("技能"))
        );
        assert!(labels_matching(&state, "no-such-row").is_empty());
        assert!(palette_matches(
            &palette_catalog(&state).groups[0].items[0],
            "   "
        ));
    }

    #[test]
    fn confirm_index_uses_the_unfiltered_group_and_row() {
        let catalog = palette_catalog(&populated());
        let item = palette_item(&catalog, 2, 0).expect("first command");
        assert!(matches!(item.command, PaletteCommand::Slash(ref insert) if insert == "/new"));
        assert!(palette_item(&catalog, 9, 0).is_none());
    }

    #[test]
    fn opening_the_palette_closes_other_floating_menus() {
        let mut state = WorkspaceState {
            model_menu_open: true,
            reasoning_menu_open: true,
            view: MainView::Chat,
            ..WorkspaceState::default()
        };
        reduce(&mut state, DesktopAction::CommandPaletteSet(true));
        assert!(state.command_palette_open);
        assert!(!state.model_menu_open);
        assert!(!state.reasoning_menu_open);
        reduce(&mut state, DesktopAction::CommandPaletteSet(false));
        assert!(!state.command_palette_open);
    }

    #[test]
    fn settings_page_is_checked_only_while_that_page_is_open() {
        let mut state = populated();
        state.view = MainView::Settings;
        state.settings_section = SettingsSection::Agents;
        let catalog = palette_catalog(&state);
        let settings = catalog
            .groups
            .iter()
            .find(|group| group.id == PaletteGroupId::Settings)
            .expect("settings");
        let checked: Vec<_> = settings
            .items
            .iter()
            .filter(|item| item.checked)
            .map(|item| match item.command {
                PaletteCommand::OpenSettings(section) => section,
                _ => panic!("settings group only opens pages"),
            })
            .collect();
        assert_eq!(checked, vec![SettingsSection::Agents]);
    }
}
