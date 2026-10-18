//! Composer and transcript vocabulary: mention autocomplete and live
//! subagent cards. The transcript window itself lives on disk; this module
//! names the composer triggers and the live job cards.

/// Composer mention autocomplete: `@` files or `/` commands, skills, and MCP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerMention {
    /// Trigger kind active in the draft.
    pub kind: MentionKind,
    /// Typed text after the trigger character.
    pub fragment: String,
    /// Rows the menu can accept. Order is the keyboard order.
    pub items: Vec<MentionItem>,
}

/// One autocomplete row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionItem {
    /// Token passed to accept: `/new`, `/slug`, `mcp:<server>`, or a file path.
    pub insert: String,
    /// Secondary label. Command labels are English ids resolved at render.
    pub label: String,
    /// Which section the row belongs to.
    pub group: MentionGroup,
}

/// Section of a composer autocomplete row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MentionGroup {
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
pub struct LiveJob {
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
    /// Unix milliseconds when the job was first seen, for its running-time
    /// line. `0` means unknown (defaulted state), not epoch.
    pub started_ms: u64,
}

impl LiveJob {
    /// Role plus the short brief, so a row reads as `scout · map the crate`.
    /// A job whose progress arrived before its role did falls back to the
    /// localized generic label.
    #[must_use]
    pub fn title(&self) -> String {
        let role = if self.role.is_empty() {
            crate::i18n::t("Subagent", "子代理")
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
    pub fn step_label(&self) -> String {
        match self.step.as_str() {
            "" | "starting" => crate::i18n::t("starting", "启动中").to_owned(),
            "queued" => crate::i18n::t("queued", "排队中").to_owned(),
            other => other.to_owned(),
        }
    }

    /// Milliseconds this job has been running. A defaulted job (`started_ms`
    /// of 0) reports no time rather than a duration since the epoch.
    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        if self.started_ms == 0 {
            return 0;
        }
        crate::unix_now()
            .as_millis()
            .saturating_sub(u128::from(self.started_ms)) as u64
    }
}

/// `42s`, `3m 05s`, `1h 02m` — compact enough for a status line tail.
/// Seconds granularity is all a row that repaints once a second can show.
#[must_use]
pub fn elapsed_label(ms: u64) -> String {
    let total = ms / 1_000;
    if total < 60 {
        format!("{total}s")
    } else if total < 3_600 {
        format!("{}m {:02}s", total / 60, total % 60)
    } else {
        format!("{}h {:02}m", total / 3_600, (total % 3_600) / 60)
    }
}

/// How many listed jobs are artisans waiting past the concurrency cap.
#[must_use]
pub fn capped_artisan_count(jobs: &[LiveJob]) -> usize {
    jobs.iter()
        .filter(|job| job.capped && job.role == "artisan")
        .count()
}

/// Status and banner copy for artisans queued past the concurrency cap.
#[must_use]
pub fn capped_artisan_status(count: usize) -> String {
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

/// Status-line copy while subagents run. The transcript row of each `agent`
/// call already names its task, so the line only says the turn is waiting;
/// a cap warning is the one detail it still carries.
#[must_use]
pub fn subagent_wait_status(jobs: &[LiveJob]) -> Option<String> {
    let capped = capped_artisan_count(jobs);
    match jobs.len() {
        0 => None,
        _ if capped > 0 => Some(capped_artisan_status(capped)),
        1 => Some(crate::i18n::t("Waiting for the agent to finish", "等待代理结束").to_owned()),
        count => Some(if crate::i18n::is_chinese() {
            format!("等待 {count} 个代理结束")
        } else {
            format!("Waiting for {count} agents to finish")
        }),
    }
}

/// Whether a transcript `agent` row still owns a running subagent and so
/// shows a cancel `x`. A finished call, a stopped turn, or a job that has
/// already left the live list shows none.
#[must_use]
pub fn subagent_cancellable(
    call_id: Option<&str>,
    finished: bool,
    turn_running: bool,
    jobs: &[LiveJob],
) -> bool {
    !finished
        && turn_running
        && call_id
            .filter(|id| !id.is_empty())
            .is_some_and(|id| jobs.iter().any(|job| job.call_id == id))
}

/// The mention trigger parsed from the composer draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MentionKind {
    /// `@` file reference against the session project.
    File,
    /// `/` command at the very start of the draft.
    Command,
}

/// Built-in slash commands offered by the composer menu.
pub const COMPOSER_COMMANDS: &[(&str, &str)] = &[
    ("/new", "new chat"),
    ("/compact", "compact context"),
    ("/settings", "settings"),
];

/// Whether `title` matches a sidebar filter. An empty filter matches every
/// title. The comparison is case-insensitive and does not touch disk.
#[must_use]
pub fn session_title_matches(title: &str, filter: &str) -> bool {
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
    fn the_elapsed_label_stays_compact() {
        assert_eq!(super::elapsed_label(0), "0s");
        assert_eq!(super::elapsed_label(42_000), "42s");
        assert_eq!(super::elapsed_label(59_999), "59s");
        assert_eq!(super::elapsed_label(60_000), "1m 00s");
        assert_eq!(super::elapsed_label(185_000), "3m 05s");
        assert_eq!(super::elapsed_label(3_725_000), "1h 02m");
    }

    #[test]
    fn a_defaulted_job_reports_no_elapsed_time() {
        let job = super::LiveJob::default();
        assert_eq!(job.elapsed_ms(), 0);
        let job = super::LiveJob {
            started_ms: crate::unix_now().as_millis() as u64 - 5_000,
            ..super::LiveJob::default()
        };
        let elapsed = job.elapsed_ms();
        assert!((4_900..=6_000).contains(&elapsed), "{elapsed}");
    }

    #[test]
    fn only_a_live_unfinished_agent_row_can_be_cancelled() {
        let jobs = vec![super::LiveJob {
            call_id: "c1".to_owned(),
            ..super::LiveJob::default()
        }];
        assert!(super::subagent_cancellable(Some("c1"), false, true, &jobs));
        assert!(!super::subagent_cancellable(Some("c1"), true, true, &jobs));
        assert!(!super::subagent_cancellable(
            Some("c1"),
            false,
            false,
            &jobs
        ));
        assert!(!super::subagent_cancellable(Some("c2"), false, true, &jobs));
        assert!(!super::subagent_cancellable(None, false, true, &jobs));
        assert!(!super::subagent_cancellable(Some(""), false, true, &[]));
    }

    #[test]
    fn the_wait_status_names_no_agent_step() {
        assert_eq!(super::subagent_wait_status(&[]), None);
        let job = super::LiveJob {
            call_id: "c1".to_owned(),
            role: "scout".to_owned(),
            label: "map tokio".to_owned(),
            step: "running read src/lib.rs".to_owned(),
            ..super::LiveJob::default()
        };
        let one = super::subagent_wait_status(std::slice::from_ref(&job)).expect("one job");
        assert!(!one.contains("tokio") && !one.contains("read"), "{one}");
        let two = super::subagent_wait_status(&[job.clone(), job]).expect("two jobs");
        assert!(two.contains('2'), "{two}");
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
