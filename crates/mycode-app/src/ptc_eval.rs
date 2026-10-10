//! Live choice eval for grouping versus scout.
//!
//! The rubric is [`mycode_agent::accept_choice`]. A narrow multi-step task
//! must come back as `run_code`. One edit must be a direct `edit` or
//! `write`. Broad research may be `scout` or inline `run_code`. The test
//! calls a model only when `MYCODE_PTC_EVAL_API_KEY` or `OPENAI_API_KEY`
//! is set, plus `MYCODE_PTC_EVAL_BASE_URL` (default
//! `https://api.openai.com/v1`) and `MYCODE_PTC_EVAL_MODEL`.

use mycode_agent::{accept_choice, build_system_prompt, choice_scenarios};
use mycode_tools::ToolRegistry;
use serde_json::{Value, json};

#[tokio::test]
async fn live_model_chooses_by_difficulty_when_configured() {
    let key = std::env::var("MYCODE_PTC_EVAL_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .ok();
    let Some(key) = key.filter(|value| !value.trim().is_empty()) else {
        eprintln!(
            "ptc live eval skipped: set MYCODE_PTC_EVAL_API_KEY or OPENAI_API_KEY to score a model"
        );
        return;
    };
    let base = std::env::var("MYCODE_PTC_EVAL_BASE_URL")
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned());
    let model = match std::env::var("MYCODE_PTC_EVAL_MODEL") {
        Ok(model) if !model.trim().is_empty() => model,
        _ => {
            eprintln!("ptc live eval skipped: set MYCODE_PTC_EVAL_MODEL");
            return;
        }
    };
    let registry = ToolRegistry::new();
    mycode_tools::register_builtins(&registry);
    let mut system = build_system_prompt(&registry);
    system.push_str(
        "\n\nYou decide whether to delegate. A simple lookup or a few files stays inline: use `run_code` when that step needs several reads, greps, finds, edits, or page fetches, and a direct tool when one call is enough. `scout` fits broad codebase search, web research, or documentation fetches, when a separate read-only pass would keep this context smaller. You choose; a narrow question stays inline.",
    );
    let mut tools = tool_specs(&registry);
    tools.push(json!({
        "type": "function",
        "function": {
            "name": "agent",
            "description": "Delegate one scoped unit. scout is read-only research. artisan is a bounded change. You choose when the task is broad enough.",
            "parameters": {
                "type": "object",
                "properties": {
                    "agent": {"type": "string", "description": "scout or artisan"},
                    "prompt": {"type": "string"},
                    "description": {"type": "string"}
                },
                "required": ["agent", "prompt"]
            }
        }
    }));
    let client = reqwest::Client::new();
    for scenario in choice_scenarios() {
        let body = json!({
            "model": model,
            "temperature": 0,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": scenario.task},
            ],
            "tools": tools,
        });
        let response = client
            .post(format!("{}/chat/completions", base.trim_end_matches('/')))
            .bearer_auth(&key)
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|error| panic!("{}: {error}", scenario.id));
        let status = response.status();
        let payload: Value = response
            .json()
            .await
            .unwrap_or_else(|error| panic!("{}: {error}", scenario.id));
        assert!(status.is_success(), "{}: {status} {payload}", scenario.id);
        let (name, arguments) = first_tool(&payload)
            .unwrap_or_else(|| panic!("{}: no tool call in {payload}", scenario.id));
        eprintln!(
            "ptc live eval {}: tool={name} args={}",
            scenario.id, arguments
        );
        assert!(
            accept_choice(scenario.weight, &name, &arguments),
            "{}: {name} does not match {:?}",
            scenario.id,
            scenario.weight
        );
    }
}

fn tool_specs(registry: &ToolRegistry) -> Vec<Value> {
    registry
        .specs()
        .iter()
        .map(|spec| {
            json!({
                "type": "function",
                "function": {
                    "name": spec.name,
                    "description": spec.description,
                    "parameters": spec.params_schema,
                }
            })
        })
        .collect()
}

fn first_tool(payload: &Value) -> Option<(String, Value)> {
    let call = payload["choices"][0]["message"]["tool_calls"][0].as_object()?;
    let function = call.get("function")?;
    let name = function.get("name")?.as_str()?.to_owned();
    let arguments = match function.get("arguments")? {
        Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
        other => other.clone(),
    };
    Some((name, arguments))
}
