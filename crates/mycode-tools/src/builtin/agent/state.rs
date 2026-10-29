//! Typed, replayable snapshots for agent progress and retained results.
use serde::{Deserialize, Serialize};

/// Prefix of JSON snapshot progress messages.
pub const AGENT_STATE_PREFIX: &str = "agent_state|";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    Queued,
    Running,
    Settling,
    Completed,
    Failed,
    Cancelled,
    Blocked,
}
impl AgentRunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Blocked
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentWorkflowStatus {
    Queued,
    Running,
    WaitingIntegration,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunSnapshot {
    pub id: String,
    pub call_id: String,
    pub role: String,
    pub description: String,
    pub prompt: String,
    pub status: AgentRunStatus,
    pub step: String,
    pub path: String,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub artifact_path: Option<String>,
    pub workflow_id: Option<String>,
    pub node_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkflowNode {
    pub id: String,
    pub role: String,
    pub description: String,
    pub dependencies: Vec<String>,
    pub status: AgentRunStatus,
    pub run_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkflowSnapshot {
    pub id: String,
    pub name: String,
    /// Explicit rework generation; old snapshots deserialize as revision zero.
    #[serde(default)]
    pub revision: u32,
    pub status: AgentWorkflowStatus,
    pub nodes: Vec<AgentWorkflowNode>,
    pub acceptance: Option<String>,
    pub result: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "snapshot", rename_all = "snake_case")]
pub enum AgentStateEvent {
    Run(AgentRunSnapshot),
    Workflow(AgentWorkflowSnapshot),
}

pub(super) fn publish(stream: &crate::stream::ToolStream, event: AgentStateEvent) {
    if let Ok(json) = serde_json::to_string(&event) {
        stream.progress(format!("{AGENT_STATE_PREFIX}{json}"));
    }
}
