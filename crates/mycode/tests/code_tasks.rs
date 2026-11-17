//! Fixed code-task regression suite. Offline cases exercise the runtime with
//! scripted decisions; the ignored live entry measures a selected Magpie model.
#[path = "code_tasks/support.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use mycode::agent::TurnBudget;
use mycode::core::{
    AssistantMessage, ContentBlock, Message, Provider, StopReason, ToolResultMessage,
};
use mycode::tools::builtin::AgentTool;
use mycode::tools::{CodeCells, ToolRegistry, enable_code_mode};
use serde_json::json;
use support::{
    ChildHost, Fixture, Frame, REQUEST_LIMIT, Receipts, RecordedAgentTool, ScriptedProvider, call,
    check_handoff, drive,
};

fn clamp_plan() -> Vec<Frame> {
    vec![
        Frame::calls(vec![call("read", "read", json!({"path":"clamp.rs"}))]),
        Frame::calls(vec![call("edit", "edit", json!({"path":"clamp.rs","old_string":"value.max(100)","new_string":"value.min(100)"}))])
            .after("read", false, "value.max(100)"),
        Frame::calls(vec![call("verify", "grep", json!({"path":"clamp.rs","pattern":"value.min(100)"}))])
            .after("edit", false, ""),
        Frame::finish("Changed clamp.rs; checked the updated expression with grep. Independent tests remain for the parent.")
            .after("verify", false, "value.min(100)"),
    ]
}

async fn offline(
    fixture: &Fixture,
    tools: &ToolRegistry,
    frames: Vec<Frame>,
) -> mycode::agent::Agent {
    let provider = ScriptedProvider::new(frames);
    let result = drive(
        &provider,
        tools,
        &fixture.workspace,
        fixture.task,
        Arc::new(TurnBudget::new(REQUEST_LIMIT)),
        vec![],
        Duration::from_secs(60),
    )
    .await;
    fixture.settle_children().await.expect("children settle");
    let agent = result.expect("offline task finishes normally");
    provider.assert_finished();
    agent
}

#[tokio::test]
async fn ct01_precise_boundary_fix_preserves_unrelated_work() {
    let fixture = Fixture::new("clamp");
    offline(&fixture, &fixture.tools(), clamp_plan()).await;
    fixture
        .grade(&["clamp.rs"])
        .await
        .expect("percentage boundary oracle");
}

#[tokio::test]
async fn ct02_multifile_discovery_edit_and_verification_through_codemode() {
    let fixture = Fixture::new("invoice");
    let tools = fixture.tools();
    enable_code_mode(&tools, &Arc::new(CodeCells::new()), "ct02");
    let frames = vec![
        Frame::calls(vec![call("inspect", "run_code", json!({"script":r#"
const hits = await callAsync("grep", {pattern:"units", path:"."});
if (hits.is_error) throw "discovery failed";
const files = await Promise.all([
    callAsync("read", {path:"invoice.rs"}),
    callAsync("read", {path:"policy.rs"})
]);
if (files.some(file => file.is_error)) throw "read failed";
complete(files.map(file => file.content[0].Text).join("\n"));
"#}))]),
        Frame::calls(vec![call("patch", "run_code", json!({"script":r#"
const invoice = await callAsync("edit", {path:"invoice.rs", old_string:"units.max(1) * rate", new_string:"units * rate"});
if (invoice.is_error) throw "invoice edit failed";
const policy = await callAsync("edit", {path:"policy.rs", old_string:"units >= 10", new_string:"units > 10"});
if (policy.is_error) throw "policy edit failed";
const check = await callAsync("grep", {path:".", pattern:"units"});
if (check.is_error) throw "verification failed";
complete(check.content[0].Text);
"#}))]).after("inspect", false, "units.max(1)").after("inspect", false, "units >= 10"),
        Frame::finish("Changed invoice.rs and policy.rs; verified both rules with grep.")
            .after("patch", false, "units * rate").after("patch", false, "units > 10"),
    ];
    offline(&fixture, &tools, frames).await;
    fixture
        .grade(&["invoice.rs", "policy.rs"])
        .await
        .expect("invoice boundary oracle");
}

#[tokio::test]
async fn ct03_ambiguous_edit_error_is_visible_and_recoverable() {
    let fixture = Fixture::new("ambiguous");
    let frames = vec![
        Frame::calls(vec![call("broad", "edit", json!({"path":"ranges.rs","old_string":"value + 1","new_string":"value - 1"}))]),
        Frame::calls(vec![call("inspect", "read", json!({"path":"ranges.rs"}))]).after("broad", true, ""),
        Frame::calls(vec![call("precise", "edit", json!({"path":"ranges.rs","old_string":"pub fn previous(value: i32) -> i32 {\n    value + 1\n}","new_string":"pub fn previous(value: i32) -> i32 {\n    value - 1\n}"}))])
            .after("inspect", false, "pub fn next(value: i32) -> i32 {\n    value + 1")
            .after("inspect", false, "pub fn previous(value: i32) -> i32 {\n    value + 1"),
        Frame::calls(vec![call("verify", "read", json!({"path":"ranges.rs"}))]).after("precise", false, ""),
        Frame::finish("Recovered from the ambiguous edit; next is unchanged and previous now decrements.")
            .after("verify", false, "value - 1"),
    ];
    offline(&fixture, &fixture.tools(), frames).await;
    fixture
        .grade(&["ranges.rs"])
        .await
        .expect("paired functions oracle");
}

#[tokio::test]
async fn ct04_invalid_tools_have_no_effect_and_valid_work_continues() {
    let fixture = Fixture::new("clamp");
    let mut frames = vec![
        Frame::calls(vec![call(
            "unknown",
            "invented_mutation",
            json!({"path":"clamp.rs"}),
        )]),
        Frame::calls(vec![call("malformed", "write", json!({"path":"clamp.rs"}))])
            .after("unknown", true, ""),
    ];
    let mut repair = clamp_plan();
    repair[0] = Frame::calls(vec![call("read", "read", json!({"path":"clamp.rs"}))]).after(
        "malformed",
        true,
        "",
    );
    frames.extend(repair);
    offline(&fixture, &fixture.tools(), frames).await;
    fixture
        .grade(&["clamp.rs"])
        .await
        .expect("valid work after rejected tools");
}

fn install_child(
    fixture: &Fixture,
    tools: &ToolRegistry,
    provider: Arc<dyn Provider>,
    budget: Arc<TurnBudget>,
) -> Receipts {
    let host = Arc::new(ChildHost {
        provider,
        tools: fixture.child_tools(),
        cwd: fixture.workspace.clone(),
        budget,
        _lifetime: fixture.lifetime.clone(),
    });
    let receipts = Receipts::default();
    tools.register(Arc::new(RecordedAgentTool {
        inner: Arc::new(AgentTool::new(host).with_runtime(fixture.children.clone())),
        receipts: receipts.clone(),
        events: fixture.events.clone(),
    }));
    receipts
}

#[tokio::test]
async fn ct05_child_handoff_is_inspectable_and_parent_verifies_the_change() {
    let fixture = Fixture::new("clamp");
    let tools = fixture.tools();
    let child = Arc::new(ScriptedProvider::new(clamp_plan()));
    let budget = Arc::new(TurnBudget::new(REQUEST_LIMIT));
    let receipts = install_child(&fixture, &tools, child.clone(), budget.clone());
    let parent = ScriptedProvider::new(vec![
        Frame::calls(vec![call(
            "delegate",
            "agent",
            json!({"agent":"artisan","prompt":fixture.task,"description":"Repair percentage bounds","isolation":"shared","phase_id":"clamp-fix","scope":{"paths":["clamp.rs"]}}),
        )]),
        Frame::calls(vec![call(
            "parent-read",
            "read",
            json!({"path":"clamp.rs"}),
        )])
        .after("delegate", false, "Run: run-")
        .after("delegate", false, "Review handoff"),
        Frame::finish(
            "Reviewed the child's actual source change; external oracle still owns acceptance.",
        )
        .after("parent-read", false, "value.min(100)"),
    ]);
    let result = drive(
        &parent,
        &tools,
        &fixture.workspace,
        fixture.task,
        budget.clone(),
        vec![],
        Duration::from_secs(60),
    )
    .await;
    fixture.settle_children().await.expect("children settle");
    result.expect("parent and child finish");
    parent.assert_finished();
    child.assert_finished();
    assert_eq!(
        budget.model_requests_started(),
        7,
        "parent and child share the allowance"
    );
    check_handoff(&receipts, &fixture).expect("usable child result");
    fixture
        .grade(&["clamp.rs"])
        .await
        .expect("delegated percentage oracle");
}

fn rejected_history() -> Vec<Arc<Message>> {
    vec![
        Arc::new(Message::User(mycode::core::UserMessage::text(
            "Fix the percentage bounds.",
        ))),
        Arc::new(Message::Assistant(AssistantMessage {
            blocks: vec![ContentBlock::ToolCall(call(
                "rejected-write",
                "write",
                json!({"path":"clamp.rs"}),
            ))],
            usage: None,
            stop_reason: StopReason::ToolUse,
        })),
        Arc::new(Message::ToolResult(ToolResultMessage {
            tool_call_id: "rejected-write".into(),
            content: vec![ContentBlock::Text(mycode::core::TextBlock::new(
                "Invalid arguments: content is required. The file was not changed.",
            ))],
            is_error: true,
            details: None,
        })),
    ]
}

/// Explicit opt-in only. The default test suite never constructs a network provider.
#[tokio::test]
#[ignore = "uses the configured paid Magpie model; requires MYCODE_EVAL_LIVE=1"]
async fn live_magpie_fixed_code_tasks() {
    assert_eq!(
        std::env::var("MYCODE_EVAL_LIVE").as_deref(),
        Ok("1"),
        "set MYCODE_EVAL_LIVE=1 to authorize live evaluation requests"
    );
    let home = mycode::config::HomeLayout::from_env(mycode::config::HomeEnv::from_process())
        .unwrap_or_else(|_| panic!("MYCode home is unavailable"));
    let settings = mycode::config::read_app_settings(&home)
        .unwrap_or_else(|_| panic!("MYCode settings are unavailable or invalid"));
    let row = mycode::app::magpie_provider_row(&settings.magpie, &Default::default())
        .unwrap_or_else(|| panic!("configure an active Magpie model in MYCode first"));
    let resolved = mycode::providers::ResolvedProvider::resolve(
        &row,
        &settings.magpie.model,
        &settings.magpie.api_key,
        "mycode-code-task-eval",
    )
    .unwrap_or_else(|_| panic!("configured Magpie route is invalid"));
    let transport = mycode::providers::ReqwestTransport::new()
        .unwrap_or_else(|_| panic!("model transport is unavailable"));
    let provider: Arc<dyn Provider> = Arc::new(mycode::providers::WireProvider::new(
        resolved,
        Arc::new(transport),
    ));
    let tasks = ["ct01", "ct02", "ct03", "ct04", "ct05"];
    let selected = std::env::var("MYCODE_EVAL_TASK").ok();
    assert!(
        selected.as_deref().is_none_or(|task| tasks.contains(&task)),
        "MYCODE_EVAL_TASK must be ct01 through ct05"
    );
    let mut failed = 0;
    for id in tasks
        .into_iter()
        .filter(|id| selected.as_deref().is_none_or(|selected| selected == *id))
    {
        let fixture = Fixture::new(match id {
            "ct02" => "invoice",
            "ct03" => "ambiguous",
            _ => "clamp",
        });
        let tools = fixture.tools();
        let budget = Arc::new(TurnBudget::new(REQUEST_LIMIT));
        let receipts = if id == "ct05" {
            Some(install_child(
                &fixture,
                &tools,
                provider.clone(),
                budget.clone(),
            ))
        } else {
            None
        };
        enable_code_mode(&tools, &Arc::new(CodeCells::new()), id);
        let task = if id == "ct05" {
            format!(
                "Delegate the implementation to artisan using shared isolation. After it returns, independently read the changed file before answering. {}",
                fixture.task
            )
        } else {
            fixture.task.to_owned()
        };
        let seed = if id == "ct04" {
            rejected_history()
        } else {
            vec![]
        };
        let result = drive(
            provider.as_ref(),
            &tools,
            &fixture.workspace,
            &task,
            budget.clone(),
            seed,
            Duration::from_secs(300),
        )
        .await;
        let settled = fixture.settle_children().await;
        let result = result.and_then(|agent| settled.map(|()| agent));
        let verdict = match result {
            Ok(_) => {
                let handoff = receipts
                    .as_ref()
                    .map_or(Ok(()), |receipts| check_handoff(receipts, &fixture));
                match handoff {
                    Err(error) => Err(error),
                    Ok(()) => {
                        fixture
                            .grade(if id == "ct02" {
                                &["invoice.rs", "policy.rs"]
                            } else if id == "ct03" {
                                &["ranges.rs"]
                            } else {
                                &["clamp.rs"]
                            })
                            .await
                    }
                }
            }
            Err(error) => Err(error),
        };
        eprintln!(
            "task={id} model={} result={} requests={}/{}",
            settings.magpie.model,
            if verdict.is_ok() { "PASS" } else { "FAIL" },
            budget.model_requests_started(),
            REQUEST_LIMIT
        );
        if let Err(reason) = verdict {
            failed += 1;
            eprintln!("task={id} reason={reason}");
        }
    }
    assert_eq!(failed, 0, "live code-task acceptance failures");
}
