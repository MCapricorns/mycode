//! Composer mention parsing: turns the draft into an active `@` file or `/`
//! command autocomplete. A leading `/` lists built-in commands, discovered
//! skills, and enabled MCP servers without waiting for a longer prefix.

use super::super::chat::{
    COMPOSER_COMMANDS, ComposerMention, MentionGroup, MentionItem, MentionKind,
};
use super::super::projects::has_open_folder;
use super::super::settings::SkillEntry;
use super::super::state::WorkspaceState;

/// What Enter or the send arrow should do with the current draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerSubmit {
    /// An open session can take the text.
    Send,
    /// No session yet, and a folder is open. Create one, then send `text`.
    StartTask(String),
    /// A new task needs an open folder.
    NeedFolder,
    /// Nothing to send.
    Ignore,
}

/// Decides Enter / send. The welcome desk has no session, so a non-empty
/// draft starts a task instead of being dropped.
#[must_use]
pub fn composer_submit(state: &WorkspaceState, draft: &str) -> ComposerSubmit {
    if state.active.is_some() {
        return ComposerSubmit::Send;
    }
    if draft.trim().is_empty() {
        return ComposerSubmit::Ignore;
    }
    if has_open_folder(state) {
        ComposerSubmit::StartTask(draft.to_owned())
    } else {
        ComposerSubmit::NeedFolder
    }
}

/// How many slash rows the menu keeps. Commands and matching skills stay;
/// extra MCP tool rows are the first to drop.
const MAX_SLASH_ROWS: usize = 40;

/// Parses the composer draft into an active mention, if any: a trailing
/// `@fragment` token selects files; a leading `/name` (still the whole
/// draft) selects commands, skills, and MCP. Command rows are filled by
/// [`slash_items`] so they can see the loaded catalog.
pub(super) fn parse_mention(text: &str) -> Option<ComposerMention> {
    if text.ends_with(char::is_whitespace) || text.is_empty() {
        return None;
    }
    let token = text.split_whitespace().last().unwrap_or_default();
    if let Some(fragment) = token.strip_prefix('@')
        && !fragment.contains('@')
    {
        return Some(ComposerMention {
            kind: MentionKind::File,
            fragment: fragment.to_owned(),
            items: Vec::new(),
        });
    }
    if let Some(fragment) = text.strip_prefix('/')
        && !fragment.contains(char::is_whitespace)
    {
        return Some(ComposerMention {
            kind: MentionKind::Command,
            fragment: fragment.to_owned(),
            items: Vec::new(),
        });
    }
    None
}

/// Commands, then skills, then enabled MCP servers and their listed tools.
///
/// An empty fragment shows every command, every skill, and every enabled
/// server. Tool rows appear once the fragment matches that server or tool,
/// so `/` itself stays a short explicit catalog instead of every schema.
pub(super) fn slash_items(
    fragment: &str,
    skills: &[SkillEntry],
    servers: &[mycode_config::McpServerSettings],
    tools: &[(String, Vec<String>)],
) -> Vec<MentionItem> {
    let fragment = fragment.to_lowercase();
    let mut items = Vec::new();
    let mut fallback = Vec::new();
    for (name, english, chinese) in COMPOSER_COMMANDS {
        let slug = &name[1..];
        let label = crate::i18n::t(english, chinese);
        let item = || MentionItem {
            insert: (*name).to_owned(),
            label: label.to_owned(),
            group: MentionGroup::Command,
        };
        if fragment.is_empty() || slug.starts_with(&fragment) {
            items.push(item());
        } else if slug.contains(&fragment)
            || english.to_ascii_lowercase().contains(&fragment)
            || chinese.contains(&fragment)
        {
            fallback.push(item());
        }
    }
    for skill in skills {
        let slug = skill.slug.to_lowercase();
        let item = || MentionItem {
            insert: format!("/{}", skill.slug),
            label: skill.title.clone(),
            group: MentionGroup::Skill,
        };
        if fragment.is_empty() || slug.starts_with(&fragment) {
            items.push(item());
        } else if slug.contains(&fragment) || skill.title.to_lowercase().contains(&fragment) {
            fallback.push(item());
        }
    }
    for server in servers.iter().filter(|server| server.enabled) {
        let id = server.id.to_lowercase();
        if fragment.is_empty() || id.starts_with(&fragment) {
            items.push(MentionItem {
                insert: format!("mcp:{}", server.id),
                label: server.id.clone(),
                group: MentionGroup::Mcp,
            });
        } else if id.contains(&fragment) {
            fallback.push(MentionItem {
                insert: format!("mcp:{}", server.id),
                label: server.id.clone(),
                group: MentionGroup::Mcp,
            });
        }
        let Some(names) = tools
            .iter()
            .find(|(server_id, _)| server_id == &server.id)
            .map(|(_, names)| names)
        else {
            continue;
        };
        for tool in names {
            let tool_key = tool.to_lowercase();
            let qualified = format!("{id}/{tool_key}");
            if !fragment.is_empty()
                && (tool_key.starts_with(&fragment)
                    || qualified.starts_with(&fragment)
                    || id.starts_with(&fragment))
            {
                items.push(MentionItem {
                    insert: format!("mcp:{}/{}", server.id, tool),
                    label: format!("{} / {tool}", server.id),
                    group: MentionGroup::Mcp,
                });
            } else if !fragment.is_empty() && qualified.contains(&fragment) {
                fallback.push(MentionItem {
                    insert: format!("mcp:{}/{}", server.id, tool),
                    label: format!("{} / {tool}", server.id),
                    group: MentionGroup::Mcp,
                });
            }
        }
    }
    items.extend(fallback);
    items.truncate(MAX_SLASH_ROWS);
    items
}

/// Row Enter should accept.
///
/// An exact `/fragment` or `mcp:fragment` wins. Otherwise the first row,
/// which the menu paints as the keyboard target.
pub fn preferred_slash_index(fragment: &str, items: &[MentionItem]) -> usize {
    if items.is_empty() {
        return 0;
    }
    let typed = format!("/{}", fragment.to_ascii_lowercase());
    let mcp = format!("mcp:{}", fragment.to_ascii_lowercase());
    items
        .iter()
        .position(|item| {
            let insert = item.insert.to_ascii_lowercase();
            insert == typed || insert == mcp
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{preferred_slash_index, slash_items};
    use crate::view_model::{MentionGroup, SkillEntry};

    fn skill(slug: &str, title: &str) -> SkillEntry {
        SkillEntry {
            slug: slug.to_owned(),
            title: title.to_owned(),
            path: format!("/tmp/{slug}/SKILL.md"),
            global: false,
        }
    }

    fn server(id: &str, enabled: bool) -> mycode_config::McpServerSettings {
        mycode_config::McpServerSettings {
            id: id.to_owned(),
            enabled,
            transport: "http".to_owned(),
            command: None,
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            endpoint: None,
            key_header: None,
        }
    }

    #[test]
    fn slash_lists_commands_skills_and_enabled_mcp_without_a_prefix() {
        let skills = vec![skill("review", "Review")];
        let servers = vec![server("context7", true), server("off", false)];
        let tools = vec![("context7".to_owned(), vec!["resolve-library-id".to_owned()])];
        let items = slash_items("", &skills, &servers, &tools);
        assert!(items.iter().any(|item| item.insert == "/new"));
        assert!(items.iter().any(|item| item.insert == "/settings"));
        assert!(
            items
                .iter()
                .any(|item| { item.insert == "/review" && item.group == MentionGroup::Skill })
        );
        assert!(items.iter().any(|item| item.insert == "mcp:context7"));
        assert!(!items.iter().any(|item| item.insert.contains("off")));
        assert!(!items.iter().any(|item| item.insert.contains("resolve")));
    }

    #[test]
    fn slash_prefix_includes_matching_mcp_tools() {
        let servers = vec![server("context7", true)];
        let tools = vec![("context7".to_owned(), vec!["resolve-library-id".to_owned()])];
        let items = slash_items("resolve", &[], &servers, &tools);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert, "mcp:context7/resolve-library-id");
        assert_eq!(items[0].group, MentionGroup::Mcp);
    }

    #[test]
    fn enter_prefers_an_exact_command_over_the_first_row() {
        let items = slash_items("settings", &[], &[], &[]);
        assert_eq!(preferred_slash_index("settings", &items), 0);
        assert_eq!(items[0].insert, "/settings");
        let broad = slash_items("", &[], &[], &[]);
        assert_eq!(broad[0].insert, "/new");
        assert_eq!(preferred_slash_index("", &broad), 0);
        assert_eq!(
            preferred_slash_index("settings", &broad),
            broad
                .iter()
                .position(|item| item.insert == "/settings")
                .unwrap()
        );
    }

    #[test]
    fn slash_matches_substrings_after_prefix_hits() {
        let skills = vec![
            skill("ferris-git", "Git Repair and History"),
            skill("ferris-windows", "Windows Engineering"),
            skill("git-helper", "Git Helper"),
        ];
        let items = slash_items("git", &skills, &[], &[]);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].insert, "/git-helper", "prefix match comes first");
        assert_eq!(items[1].insert, "/ferris-git", "substring match follows");
    }
}
