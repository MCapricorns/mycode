//! Composer and transcript vocabulary: mention autocomplete, live subagent
//! cards, and the folded-transcript window constants.

/// Composer mention autocomplete: `@` files or `/` commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposerMention {
    /// Trigger kind active in the draft.
    pub kind: MentionKind,
    /// Typed text after the trigger character.
    pub fragment: String,
    /// (insert, display) rows, bounded.
    pub items: Vec<(String, String)>,
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
pub(crate) const COMPOSER_COMMANDS: &[(&str, &str)] =
    &[("/new", "new chat"), ("/settings", "settings")];
/// Recent transcript blocks that stay mounted. Older ones fold.
pub(super) const TRANSCRIPT_TAIL: usize = 24;
/// How many folded blocks one reveal click mounts.
pub(crate) const TRANSCRIPT_PAGE: usize = 24;

/// First visible display-block index for a folded transcript.
#[must_use]
pub(crate) fn transcript_start(block_count: usize, extra: usize) -> usize {
    block_count.saturating_sub(TRANSCRIPT_TAIL.saturating_add(extra))
}
