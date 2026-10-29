use super::*;
use crate::builtin::{AgentRunSnapshot, AgentRunStatus, AgentStateEvent};
use mycode_agent::session::SessionCallId;
use std::sync::Arc;

fn snapshot() -> AgentStateEvent {
    AgentStateEvent::Run(AgentRunSnapshot {
        id: "run-stable".into(),
        call_id: "provider-child".into(),
        role: "scout".into(),
        description: "Inspect replay".into(),
        prompt: "Find the evidence".into(),
        status: AgentRunStatus::Completed,
        step: "done".into(),
        path: "C:/project".into(),
        started_ms: 100,
        finished_ms: Some(1100),
        result: Some("Readable answer".into()),
        error: None,
        artifact_path: Some("C:/results/answer.md".into()),
        workflow_id: None,
        node_id: None,
    })
}

#[test]
fn live_and_replayed_agent_results_keep_typed_snapshots_out_of_display_text() {
    for code_mode in [false, true] {
        let events = vec![snapshot()];
        let mut details = serde_json::json!({"agent_states": events});
        if code_mode {
            details["inner_calls"] = serde_json::json!([
                {"seq": 0, "name": "agent", "target": "scout", "phase": "completed"}
            ]);
        }
        let result = ToolResultMessage {
            tool_call_id: "provider-outer".into(),
            content: vec![ContentBlock::Text(mycode_agent::TextBlock::new(
                "Readable answer",
            ))],
            is_error: false,
            details: Some(details),
        };
        let live = project_tool_result_message("evt-live", &result);
        assert_eq!(live.agent_states.as_deref(), Some(events.as_slice()));
        assert_eq!(live.call_id.as_deref(), Some("provider-outer"));
        let expected = if code_mode {
            "Readable answer\n[code calls]\nagent  scout"
        } else {
            "Readable answer"
        };
        assert_eq!(live.text.as_ref(), expected);
        assert!(!live.text.contains("agent_states"));
        let cloned = live.clone();
        assert!(Arc::ptr_eq(
            live.agent_states.as_ref().unwrap(),
            cloned.agent_states.as_ref().unwrap()
        ));

        let mut event = tests::event_of_kind(EventKind::ToolResult);
        event.call_id = Some(SessionCallId::generate().unwrap());
        let replayed =
            project_replayed_entry(&event, &serde_json::to_vec(&result).unwrap()).unwrap();
        assert_eq!(replayed.agent_states, live.agent_states);
        assert_eq!(replayed.text, live.text);
        assert_eq!(
            replayed.call_id.as_deref(),
            event.call_id.as_ref().map(|id| id.as_str())
        );
        let roundtrip: ConversationEntry =
            serde_json::from_value(serde_json::to_value(&replayed).unwrap()).unwrap();
        assert_eq!(roundtrip, replayed);
    }
}

#[test]
fn legacy_entries_and_results_without_valid_agent_metadata_still_project() {
    let legacy = serde_json::json!({
        "event_id": "legacy", "kind": "ToolResult", "text": "Original answer",
        "call_id": "child", "thinking": ""
    });
    let entry: ConversationEntry = serde_json::from_value(legacy.clone()).unwrap();
    assert!(entry.agent_states.is_none());
    assert_eq!(serde_json::to_value(entry).unwrap(), legacy);
    for details in [
        None,
        Some(serde_json::json!({"agent_states": "invalid"})),
        Some(serde_json::json!({"agent_states": []})),
    ] {
        let result = ToolResultMessage {
            tool_call_id: "child".into(),
            content: vec![ContentBlock::Text(mycode_agent::TextBlock::new(
                "Original answer",
            ))],
            is_error: false,
            details,
        };
        let entry = project_tool_result_message("evt", &result);
        assert!(entry.agent_states.is_none());
        assert_eq!(entry.text.as_ref(), "Original answer");
    }
}
