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
        "\n\nYou decide whether to delegate. A narrow lookup is one `run_code` whose code calls `tools.grep` or `tools.find`. One edit is `run_code` whose code calls `tools.edit` or `tools.write`. Web research and a repository-wide map may call `tools.agent` with agent \"scout\" inside that program. Delegating broad research to scout is your judgment. Do not call any tool except `run_code` directly.",
    );
    let tools = tool_specs(&registry);
    let client = reqwest::Client::new();
    let url = completions_url(&base);
    let mut failures = Vec::new();
    for scenario in choice_scenarios() {
        match score_scenario(&client, &url, &key, &model, &system, &tools, scenario).await {
            Ok(line) => eprintln!("{line}"),
            Err(message) => {
                eprintln!("ptc live eval FAIL {message}");
                failures.push(message);
            }
        }
    }
    assert!(
        failures.is_empty(),
        "ptc live eval scored every scenario; failures:\n{}",
        failures.join("\n")
    );
}

fn completions_url(base: &str) -> String {
    format!("{}/chat/completions", base.trim().trim_end_matches('/'))
}

async fn score_scenario(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    model: &str,
    system: &str,
    tools: &[Value],
    scenario: &mycode_agent::ChoiceScenario,
) -> Result<String, String> {
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
        .post(url)
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("{}: {error}", scenario.id))?;
    let status = response.status();
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("{}: {error}", scenario.id))?;
    if !status.is_success() {
        return Err(format!("{}: {status} {payload}", scenario.id));
    }
    let Some((name, arguments)) = first_tool(&payload) else {
        return Err(format!("{}: no tool call in {payload}", scenario.id));
    };
    if !accept_choice(scenario.weight, &name, &arguments) {
        return Err(format!(
            "{}: {name} does not match {:?} args={arguments}",
            scenario.id, scenario.weight
        ));
    }
    Ok(format!(
        "ptc live eval {}: tool={name} args={arguments}",
        scenario.id
    ))
}

#[test]
fn completions_url_accepts_an_openai_compatible_base() {
    assert_eq!(
        completions_url("https://api.openai.com/v1"),
        "https://api.openai.com/v1/chat/completions"
    );
    assert_eq!(
        completions_url("https://api.deepseek.com/"),
        "https://api.deepseek.com/chat/completions"
    );
}

fn tool_specs(registry: &ToolRegistry) -> Vec<Value> {
    registry
        .model_specs()
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
