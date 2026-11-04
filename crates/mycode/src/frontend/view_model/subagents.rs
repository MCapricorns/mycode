//! Session-scoped agent snapshots. Runtime records survive turn boundaries, not app restarts.
use crate::tools::builtin::{AgentRunSnapshot, AgentRunStatus};

/// Recent snapshots and activity for one session in this process.
#[derive(Clone, Debug, Default)]
pub struct SubagentSession {
    /// Session identity; empty only in headless tests without an open conversation.
    pub session_id: String,
    /// Runs in first-seen order, including terminal results.
    pub runs: Vec<AgentRunSnapshot>,
    /// Bounded recent activity keyed by stable run id.
    pub activity: Vec<(String, Vec<String>)>,
    /// Transcript ledger identities paired with stable run ids.
    pub call_ids: Vec<(String, String)>,
}

/// A bounded cache of sessions visited during this process.
#[derive(Clone, Debug, Default)]
pub struct SubagentHistory {
    pub(crate) sessions: Vec<SubagentSession>,
}

impl SubagentHistory {
    /// Snapshots belonging to the session currently on screen.
    pub fn session(&self, id: &str) -> Option<&SubagentSession> {
        self.sessions
            .iter()
            .find(|session| session.session_id == id)
    }

    pub(crate) fn session_mut(&mut self, id: &str) -> &mut SubagentSession {
        let index = self
            .sessions
            .iter()
            .position(|session| session.session_id == id);
        let session = index
            .map(|index| self.sessions.remove(index))
            .unwrap_or_else(|| SubagentSession {
                session_id: id.to_owned(),
                ..Default::default()
            });
        self.sessions.push(session);
        if self.sessions.len() > 8 {
            self.sessions.remove(0);
        }
        self.sessions.last_mut().expect("session just inserted")
    }

    pub(crate) fn forget(&mut self, id: &str) {
        self.sessions.retain(|session| session.session_id != id);
    }
}

impl SubagentSession {
    /// Resolves a selected run id, actual child id, or compatibility transcript id.
    pub fn run(&self, id: &str) -> Option<&AgentRunSnapshot> {
        let mapped = self
            .call_ids
            .iter()
            .find(|(call, _)| call == id)
            .map(|(_, run)| run.as_str());
        self.runs.iter().find(|run| {
            run.id == id
                || run.call_id == id
                || mapped == Some(run.id.as_str())
                || run.id == format!("legacy:{id}")
        })
    }

    pub(crate) fn upsert_run(&mut self, mut run: AgentRunSnapshot) -> bool {
        if run.id.is_empty() || run.call_id.is_empty() {
            return false;
        }
        if let Some(previous) = self.runs.iter_mut().find(|previous| previous.id == run.id) {
            if previous.status.is_terminal() && previous.status != run.status {
                return false;
            }
            if previous.status == AgentRunStatus::Settling
                && matches!(run.status, AgentRunStatus::Queued | AgentRunStatus::Running)
            {
                return false;
            }
            if run.result.is_none() {
                run.result = previous.result.clone();
            }
            if run.error.is_none() {
                run.error = previous.error.clone();
            }
            if run.artifact_path.is_none() {
                run.artifact_path = previous.artifact_path.clone();
            }
            if run.finished_ms.is_none() {
                run.finished_ms = previous.finished_ms;
            }
            *previous = run.clone();
        } else {
            self.runs.push(run.clone());
        }
        if !run.step.is_empty() {
            if let Some((_, log)) = self.activity.iter_mut().find(|(id, _)| id == &run.id) {
                if log.last() != Some(&run.step) {
                    log.push(run.step);
                }
                if log.len() > 48 {
                    log.remove(0);
                }
            } else {
                self.activity.push((run.id.clone(), vec![run.step]));
            }
        }
        while self.runs.len() > 128 {
            let Some(index) = self.runs.iter().position(|run| run.status.is_terminal()) else {
                break;
            };
            let removed = self.runs.remove(index);
            self.activity.retain(|(id, _)| id != &removed.id);
            self.call_ids.retain(|(_, id)| id != &removed.id);
        }
        true
    }
}

/// Localized state vocabulary for run rows.
pub fn agent_run_status_label(status: AgentRunStatus) -> &'static str {
    use crate::frontend::i18n::t;
    match status {
        AgentRunStatus::Queued => t("Queued", "排队中"),
        AgentRunStatus::Running => t("Running", "运行中"),
        AgentRunStatus::Settling => t("Settling", "收尾中"),
        AgentRunStatus::Completed => t("Completed", "已完成"),
        AgentRunStatus::Failed => t("Failed", "失败"),
        AgentRunStatus::Cancelled => t("Cancelled", "已停止"),
        AgentRunStatus::Blocked => t("Blocked", "受阻"),
    }
}

/// Completed clocks stop at the runtime's finish time.
pub fn agent_run_elapsed_ms(run: &AgentRunSnapshot) -> u64 {
    if run.started_ms == 0 {
        return 0;
    }
    run.finished_ms
        .unwrap_or_else(|| crate::core::unix_now().as_millis() as u64)
        .saturating_sub(run.started_ms)
}
