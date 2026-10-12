//! Composer and transcript vocabulary: mention autocomplete and live
//! subagent cards. The transcript window itself lives on disk; this module
//! names the composer triggers and the live job cards.

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
    /// An artisan is waiting because every concurrency slot is taken.
    pub capped: bool,
}

impl LiveJob {
    /// Role plus the short brief, so a row reads as `scout · map the crate`.
    #[must_use]
    pub(crate) fn title(&self) -> String {
        let role = if self.role.is_empty() {
            "agent"
        } else {
            self.role.as_str()
        };
        if self.label.is_empty() {
            role.to_owned()
        } else {
            format!("{role} · {}", self.label)
        }
    }

    /// Progress line for a row. Token steps are localized; a cap warning is
    /// already a sentence.
    #[must_use]
    pub(crate) fn step_label(&self) -> String {
        match self.step.as_str() {
            "" | "starting" => crate::i18n::t("starting", "启动中").to_owned(),
            "queued" => crate::i18n::t("queued", "排队中").to_owned(),
            other => other.to_owned(),
        }
    }
}

/// How many listed jobs are artisans waiting past the concurrency cap.
#[must_use]
pub(crate) fn capped_artisan_count(jobs: &[LiveJob]) -> usize {
    jobs.iter()
        .filter(|job| job.capped && job.role == "artisan")
        .count()
}

/// Status and banner copy for artisans queued past the concurrency cap.
#[must_use]
pub(crate) fn capped_artisan_status(count: usize) -> String {
    if count == 1 {
        crate::i18n::t(
            "1 artisan queued past the concurrency cap",
            "1 个 artisan 超过并发上限在排队",
        )
        .to_owned()
    } else {
        format!(
            "{count} {}",
            crate::i18n::t(
                "artisans queued past the concurrency cap",
                "个 artisan 超过并发上限在排队",
            )
        )
    }
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
    fn a_live_job_title_names_the_role() {
        let job = super::LiveJob {
            role: "scout".to_owned(),
            label: "map tokio".to_owned(),
            ..super::LiveJob::default()
        };
        assert_eq!(job.title(), "scout · map tokio");
        assert_eq!(
            super::LiveJob {
                role: "artisan".to_owned(),
                ..super::LiveJob::default()
            }
            .title(),
            "artisan"
        );
    }

    #[test]
    fn title_filter_is_case_insensitive_and_blank_matches_all() {
        assert!(session_title_matches("Hello Title", ""));
        assert!(session_title_matches("Hello Title", "  hello  "));
        assert!(!session_title_matches("Hello Title", "other"));
        assert!(session_title_matches("", ""));
        assert!(!session_title_matches("", "x"));
    }
}
