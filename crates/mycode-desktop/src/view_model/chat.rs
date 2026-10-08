//! Composer and transcript vocabulary: mention autocomplete and live
//! subagent cards. The transcript window itself lives on disk; this module
//! only names the composer triggers.

/// Composer mention autocomplete: `@` files or `/` commands, skills, and MCP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposerMention {
    /// Trigger kind active in the draft.
    pub kind: MentionKind,
    /// Typed text after the trigger character.
    pub fragment: String,
    /// Rows the menu can accept. Order is the keyboard order.
    pub items: Vec<MentionItem>,
}

/// One autocomplete row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MentionItem {
    /// Token passed to accept: `/new`, `/slug`, `mcp:<server>`, or a file path.
    pub insert: String,
    /// Secondary label. Command labels are English ids resolved at render.
    pub label: String,
    /// Which section the row belongs to.
    pub group: MentionGroup,
}

/// Section of a composer autocomplete row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MentionGroup {
    /// `@` file path.
    File,
    /// Built-in composer command.
    Command,
    /// Discovered `.agents` skill.
    Skill,
    /// Enabled MCP server, or one of its listed tools.
    Mcp,
}

/// One running `agent` call. Finished jobs drop out of the list once
/// they complete.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LiveJob {
    /// Provider-assigned call id.
    pub call_id: String,
    /// Catalog role when known (`scout`, `artisan`, …).
    pub role: String,
    /// Human-facing brief from the parent turn.
    pub label: String,
    /// Full task brief handed to the subagent.
    pub prompt: String,
    /// Working directory for the child, when known.
    pub path: String,
    /// Latest nested tool or progress line.
    pub step: String,
    /// Recent progress lines for the detail window.
    pub log: Vec<String>,
}

/// The mention trigger parsed from the composer draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MentionKind {
    /// `@` file reference against the session project.
    File,
    /// `/` command at the very start of the draft.
    Command,
}

/// Built-in slash commands offered by the composer menu.
pub(crate) const COMPOSER_COMMANDS: &[(&str, &str)] = &[
    ("/new", "new chat"),
    ("/compact", "compact context"),
    ("/settings", "settings"),
];

/// Whether `title` matches a sidebar filter. An empty filter matches every
/// title. The comparison is case-insensitive and does not touch disk.
#[must_use]
pub(crate) fn session_title_matches(title: &str, filter: &str) -> bool {
    let filter = filter.trim();
    filter.is_empty() || title.to_lowercase().contains(&filter.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::session_title_matches;

    #[test]
    fn title_filter_is_case_insensitive_and_blank_matches_all() {
        assert!(session_title_matches("Hello Title", ""));
        assert!(session_title_matches("Hello Title", "  hello  "));
        assert!(!session_title_matches("Hello Title", "other"));
        assert!(session_title_matches("", ""));
        assert!(!session_title_matches("", "x"));
    }
}
