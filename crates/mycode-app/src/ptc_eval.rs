//! Choice eval for PTC-only `run_code`.
//!
//! The rubric is [`mycode_agent::accept_scenario`]. Every task is `run_code`.
//! A narrow lookup calls `tools.grep`, `tools.find`, or `tools.read`. One edit
//! calls `tools.edit` or `tools.write`. A group calls at least two file tools.
//! A skill is `tools.read` on its `SKILL.md` path. An MCP or keyword tool is
//! the SDK name (`server__tool`, or `tools["name"]` when the name is not an
//! identifier). A process command is `tools.shell`. Several research briefs
//! call `tools.agent` with agent `"scout"`. Broad research may delegate or
//! stay inline.
//!
//! [`rubric_scores_run_code_shapes`] is the durable check and does not call a
//! model. [`live_model_chooses_by_difficulty_when_configured`] calls a model
//! only when `MYCODE_PTC_EVAL_API_KEY` or `OPENAI_API_KEY` is set, plus
//! `MYCODE_PTC_EVAL_MODEL` and `MYCODE_PTC_EVAL_BASE_URL` (default
//! `https://api.openai.com/v1`). Scenarios marked slow are skipped unless
//! `MYCODE_PTC_EVAL_SLOW=1`, so a configured run stays on the original three
//! tasks unless the extra set is requested.

use mycode_agent::{accept_scenario, build_system_prompt, choice_scenarios};
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
    let slow = slow_live_enabled();
    let all = choice_scenarios();
    let scenarios: Vec<_> = all
        .iter()
        .filter(|scenario| slow || !scenario.slow)
        .collect();
    if !slow {
        let skipped = all.len().saturating_sub(scenarios.len());
        if skipped > 0 {
            eprintln!(
                "ptc live eval: skipped {skipped} slow scenarios; set MYCODE_PTC_EVAL_SLOW=1 to score them"
            );
        }
    }
    let registry = ToolRegistry::new();
    mycode_tools::register_builtins(&registry);
    let mut system = build_system_prompt(&registry);
    system.push_str(
        "\n\nYou decide whether to delegate. Call no tool except `run_code`. Inside the program, call tools as `tools.name(...)` or `tools[\"name\"](...)`. A name that is not a Python identifier, including a keyword, is only the bracket form.\n\
A narrow lookup of one file or one identifier uses `tools.grep`, `tools.find`, or `tools.read` in that program. Do not delegate that to `tools.agent`, and do not use `tools.shell`.\n\
One typo, a single-file edit, or one new file uses `tools.edit` or `tools.write` in that program. Do not use `tools.shell`, `tools.bash`, or `tools.agent`.\n\
Several reads or edits in one step stay in that same program (`asyncio.gather` for independent reads). Do not delegate that group.\n\
When a skill matches, load its SKILL.md with `tools.read` on the catalog path inside the program. Do not call `read` directly and do not `tools.shell` it.\n\
An MCP tool that is already in the SDK is `server__tool`, for example `tools.docs__lookup` or `tools.docs__get_page`. Do not call `search_tool` or `use_tool` for those, and do not call the unsanitized server name.\n\
A build, test, or git command is `tools.shell` inside the program, not `tools.bash` and not a direct shell call.\n\
Web research and a repository-wide map may call `tools.agent` with agent \"scout\", or stay inline. Several independent scout briefs use `asyncio.gather`. Do not use artisan for that research. A fetch_content HTTP 4xx is final for that URL. Delegating broad research to scout is your judgment.",
    );
    let tools = tool_specs(&registry);
    let client = reqwest::Client::new();
    let url = completions_url(&base);
    let mut failures = Vec::new();
    for scenario in &scenarios {
        match score_scenario(&client, &url, &key, &model, &system, &tools, scenario).await {
            Ok(line) => eprintln!("{line}"),
            Err(message) => {
                eprintln!("ptc live eval FAIL {message}");
                failures.push(message);
            }
        }
    }
    let passed = scenarios.len() - failures.len();
    eprintln!("ptc live eval score {passed}/{}", scenarios.len());
    assert!(
        failures.is_empty(),
        "ptc live eval scored every selected scenario; failures:\n{}",
        failures.join("\n")
    );
}

fn slow_live_enabled() -> bool {
    matches!(
        std::env::var("MYCODE_PTC_EVAL_SLOW")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1" | "true" | "yes")
    )
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
    if !accept_scenario(scenario, &name, &arguments) {
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

#[test]
fn choice_scenarios_are_unique_and_mark_slow_cases() {
    let scenarios = choice_scenarios();
    let mut ids = std::collections::BTreeSet::new();
    let mut slow = 0;
    for scenario in scenarios {
        assert!(ids.insert(scenario.id), "duplicate {}", scenario.id);
        if scenario.slow {
            slow += 1;
        }
        if scenario.weight == mycode_agent::TaskWeight::SdkCall {
            assert!(
                !scenario.require.is_empty(),
                "{} must name the SDK tool",
                scenario.id
            );
        }
    }
    assert!(
        scenarios.iter().any(|scenario| !scenario.slow),
        "the original live set stays on by default"
    );
    assert!(
        slow >= 8,
        "expanded scenarios must be marked slow, got {slow}"
    );
}

#[test]
fn rubric_scores_run_code_shapes() {
    let cases = rubric_cases();
    let mut passed = std::collections::BTreeSet::new();
    let mut failed = std::collections::BTreeSet::new();
    for case in &cases {
        let scenario = choice_scenarios()
            .iter()
            .find(|scenario| scenario.id == case.id)
            .unwrap_or_else(|| panic!("unknown scenario {}", case.id));
        let got = accept_scenario(scenario, case.tool, &case.args);
        assert_eq!(
            got, case.pass,
            "{} tool={} args={}",
            case.id, case.tool, case.args
        );
        if case.pass {
            passed.insert(case.id);
        } else {
            failed.insert(case.id);
        }
    }
    for scenario in choice_scenarios() {
        assert!(
            passed.contains(scenario.id),
            "no passing fixture for {}",
            scenario.id
        );
        assert!(
            failed.contains(scenario.id),
            "no failing fixture for {}",
            scenario.id
        );
    }
}

struct RubricCase {
    id: &'static str,
    tool: &'static str,
    args: Value,
    pass: bool,
}

fn program(description: &str, code: &str) -> Value {
    json!({"description": description, "code": code})
}

fn rubric_cases() -> Vec<RubricCase> {
    vec![
        RubricCase {
            id: "narrow-search",
            tool: "run_code",
            args: program(
                "Find dispatch",
                "hits = await tools.grep(pattern='dispatch_tool', include='src/turn.rs')\nreturn hits",
            ),
            pass: true,
        },
        RubricCase {
            id: "narrow-search",
            tool: "run_code",
            args: program(
                "Find dispatch",
                "hits = await tools[\"grep\"](pattern='dispatch_tool')\nreturn hits",
            ),
            pass: true,
        },
        RubricCase {
            id: "narrow-search",
            tool: "run_code",
            args: program(
                "Find dispatch",
                "hits = await tools[ 'grep' ](pattern='dispatch_tool')\nreturn hits",
            ),
            pass: true,
        },
        RubricCase {
            id: "narrow-search",
            tool: "grep",
            args: json!({"pattern": "dispatch_tool"}),
            pass: false,
        },
        RubricCase {
            id: "narrow-search",
            tool: "bash",
            args: json!({"command": "rg dispatch src/turn.rs"}),
            pass: false,
        },
        RubricCase {
            id: "narrow-search",
            tool: "run_code",
            args: program(
                "Find dispatch",
                "# await tools.grep(pattern='dispatch_tool')\nreturn 'src/turn.rs:1'",
            ),
            pass: false,
        },
        RubricCase {
            id: "narrow-search",
            tool: "run_code",
            args: program(
                "Find dispatch",
                "note = \"await tools.grep(pattern='dispatch_tool')\"\nreturn note",
            ),
            pass: false,
        },
        RubricCase {
            id: "narrow-search",
            tool: "run_code",
            args: program(
                "Find dispatch",
                "return await mytools.grep(pattern='dispatch_tool')",
            ),
            pass: false,
        },
        RubricCase {
            id: "narrow-search",
            tool: "run_code",
            args: program(
                "Find dispatch",
                "return await tools.grep_extra(pattern='dispatch_tool')",
            ),
            pass: false,
        },
        RubricCase {
            id: "narrow-search",
            tool: "run_code",
            args: program(
                "Find dispatch",
                "await tools.grep(pattern='dispatch_tool')\nawait tools.shell(command='rg dispatch')",
            ),
            pass: false,
        },
        RubricCase {
            id: "narrow-search",
            tool: "run_code",
            args: program(
                "Delegate",
                "return await tools.agent(agent='scout', prompt='find dispatch')",
            ),
            pass: false,
        },
        RubricCase {
            id: "one-edit",
            tool: "run_code",
            args: program(
                "One edit",
                "await tools.edit(path='src/a.rs', old='recieve', new='receive')\nreturn 'edited'",
            ),
            pass: true,
        },
        RubricCase {
            id: "one-edit",
            tool: "run_code",
            args: program(
                "One edit",
                "await tools[\"edit\"](path='src/a.rs', old='recieve', new='receive')",
            ),
            pass: true,
        },
        RubricCase {
            id: "one-edit",
            tool: "edit",
            args: json!({"path": "src/a.rs"}),
            pass: false,
        },
        RubricCase {
            id: "one-edit",
            tool: "run_code",
            args: program("One edit", "return 1"),
            pass: false,
        },
        RubricCase {
            id: "one-edit",
            tool: "run_code",
            args: program(
                "One edit",
                "await tools.read(path='src/a.rs')\nreturn 'seen'",
            ),
            pass: false,
        },
        RubricCase {
            id: "one-edit",
            tool: "run_code",
            args: program(
                "One edit",
                "await tools.shell(command='sed -i s/recieve/receive/ src/a.rs')",
            ),
            pass: false,
        },
        RubricCase {
            id: "one-edit",
            tool: "run_code",
            args: program(
                "One edit",
                "await tools.edit(path='src/a.rs')\nawait tools.bash(command='true')",
            ),
            pass: false,
        },
        RubricCase {
            id: "one-edit",
            tool: "run_code",
            args: program(
                "One edit",
                "await tools.edit(path='src/a.rs')\nawait tools.powershell(command='echo hi')",
            ),
            pass: false,
        },
        RubricCase {
            id: "broad-research",
            tool: "run_code",
            args: program(
                "Map auth",
                "return await tools.agent(agent='scout', prompt='map auth')",
            ),
            pass: true,
        },
        RubricCase {
            id: "broad-research",
            tool: "run_code",
            args: program("Map auth inline", "return 1"),
            pass: true,
        },
        RubricCase {
            id: "broad-research",
            tool: "web_search",
            args: json!({"query": "auth"}),
            pass: false,
        },
        RubricCase {
            id: "broad-research",
            tool: "run_code",
            args: json!({"description": "Map auth", "code": "   "}),
            pass: false,
        },
        RubricCase {
            id: "broad-research",
            tool: "agent",
            args: json!({"agent": "scout", "prompt": "map auth"}),
            pass: false,
        },
        RubricCase {
            id: "grouped-reads",
            tool: "run_code",
            args: program(
                "Read both",
                "a, b = await asyncio.gather(tools.read(path=\"src/a.rs\"), tools.read(path=\"src/b.rs\"))\nreturn a + b",
            ),
            pass: true,
        },
        RubricCase {
            id: "grouped-reads",
            tool: "run_code",
            args: program("Read one", "return await tools.read(path='src/a.rs')"),
            pass: false,
        },
        RubricCase {
            id: "grouped-reads",
            tool: "run_code",
            args: program("Empty group", "return 1"),
            pass: false,
        },
        RubricCase {
            id: "grouped-reads",
            tool: "read",
            args: json!({"path": "src/a.rs"}),
            pass: false,
        },
        RubricCase {
            id: "grouped-reads",
            tool: "run_code",
            args: program(
                "Delegate reads",
                "a = await tools.read(path='src/a.rs')\nb = await tools.read(path='src/b.rs')\nawait tools.agent(agent='scout', prompt='summarize')\nreturn a",
            ),
            pass: false,
        },
        RubricCase {
            id: "grouped-edits",
            tool: "run_code",
            args: program(
                "Edit both",
                "await tools.edit(path='src/a.rs', old='recieve', new='receive')\nawait tools.edit(path='src/b.rs', old='recieve', new='receive')\nreturn 'edited'",
            ),
            pass: true,
        },
        RubricCase {
            id: "grouped-edits",
            tool: "run_code",
            args: program(
                "Sed both",
                "await tools.shell(command='sed -i s/recieve/receive/ src/a.rs src/b.rs')\nreturn 'edited'",
            ),
            pass: false,
        },
        RubricCase {
            id: "grouped-edits",
            tool: "run_code",
            args: program(
                "Edit once",
                "await tools.edit(path='src/a.rs', old='recieve', new='receive')\nreturn 'edited'",
            ),
            pass: false,
        },
        RubricCase {
            id: "one-write",
            tool: "run_code",
            args: program(
                "Write flag",
                "await tools.write(path='notes/flag.txt', content='ok')\nreturn 'wrote'",
            ),
            pass: true,
        },
        RubricCase {
            id: "one-write",
            tool: "write",
            args: json!({"path": "notes/flag.txt", "content": "ok"}),
            pass: false,
        },
        RubricCase {
            id: "one-write",
            tool: "run_code",
            args: program(
                "Edit instead",
                "await tools.edit(path='notes/flag.txt', old='no', new='ok')\nreturn 'edited'",
            ),
            pass: false,
        },
        RubricCase {
            id: "one-write",
            tool: "run_code",
            args: program(
                "Shell write",
                "await tools.shell(command='printf ok > notes/flag.txt')\nreturn 'wrote'",
            ),
            pass: false,
        },
        RubricCase {
            id: "skill-review",
            tool: "run_code",
            args: program(
                "Load review",
                "text = await tools.read(path=\".agents/skills/review/SKILL.md\")\nreturn text",
            ),
            pass: true,
        },
        RubricCase {
            id: "skill-review",
            tool: "run_code",
            args: program(
                "Load review",
                "text = await tools[\"read\"](path=\".agents/skills/review/SKILL.md\")  # tools.shell\nreturn text",
            ),
            pass: true,
        },
        RubricCase {
            id: "skill-review",
            tool: "read",
            args: json!({"path": ".agents/skills/review/SKILL.md"}),
            pass: false,
        },
        RubricCase {
            id: "skill-review",
            tool: "run_code",
            args: program(
                "Cat skill",
                "text = await tools.shell(command='cat .agents/skills/review/SKILL.md')\nreturn text",
            ),
            pass: false,
        },
        RubricCase {
            id: "skill-review",
            tool: "run_code",
            args: program(
                "Wrong file",
                "text = await tools.read(path='README.md')\nreturn text",
            ),
            pass: false,
        },
        RubricCase {
            id: "skill-review",
            tool: "run_code",
            args: program(
                "Comment only",
                "# .agents/skills/review/SKILL.md\ntext = await tools.read(path='README.md')\nreturn text",
            ),
            pass: false,
        },
        RubricCase {
            id: "skill-review",
            tool: "run_code",
            args: program(
                "Edit skill",
                "await tools.read(path='.agents/skills/review/SKILL.md')\nawait tools.edit(path='.agents/skills/review/SKILL.md', old='a', new='b')",
            ),
            pass: false,
        },
        RubricCase {
            id: "skill-review",
            tool: "run_code",
            args: program("Fake skill tool", "return await tools.skill(name='review')"),
            pass: false,
        },
        RubricCase {
            id: "mcp-docs-lookup",
            tool: "run_code",
            args: program(
                "Look up refresh",
                "text = await tools.docs__lookup(query=\"token refresh\")\nreturn text",
            ),
            pass: true,
        },
        RubricCase {
            id: "mcp-docs-lookup",
            tool: "run_code",
            args: program(
                "Look up refresh",
                "text = await tools[\"docs__lookup\"](query=\"token refresh\")\nreturn text",
            ),
            pass: true,
        },
        RubricCase {
            id: "mcp-docs-lookup",
            tool: "docs__lookup",
            args: json!({"query": "token refresh"}),
            pass: false,
        },
        RubricCase {
            id: "mcp-docs-lookup",
            tool: "run_code",
            args: program(
                "Deferred lookup",
                "schema = await tools.search_tool(name='lookup')\nreturn await tools.use_tool(name='lookup', arguments={'query': 'token refresh'})",
            ),
            pass: false,
        },
        RubricCase {
            id: "mcp-docs-lookup",
            tool: "run_code",
            args: program(
                "Wrong tool",
                "text = await tools.docs__write(query=\"token refresh\")\nreturn text",
            ),
            pass: false,
        },
        RubricCase {
            id: "mcp-docs-lookup",
            tool: "run_code",
            args: program(
                "Prefix",
                "text = await tools.docs__lookup_v2(query=\"token refresh\")\nreturn text",
            ),
            pass: false,
        },
        RubricCase {
            id: "mcp-get-page",
            tool: "run_code",
            args: program(
                "Get install page",
                "text = await tools.docs__get_page(query=\"install\")\nreturn text",
            ),
            pass: true,
        },
        RubricCase {
            id: "mcp-get-page",
            tool: "run_code",
            args: program(
                "Get install page",
                "text = await tools[\"docs__get_page\"](query=\"install\")\nreturn text",
            ),
            pass: true,
        },
        RubricCase {
            id: "mcp-get-page",
            tool: "run_code",
            args: program(
                "Raw name",
                "text = await tools[\"docs.get-page\"](query=\"install\")\nreturn text",
            ),
            pass: false,
        },
        RubricCase {
            id: "mcp-get-page",
            tool: "run_code",
            args: program(
                "Raw tool",
                "text = await tools[\"get-page\"](query=\"install\")\nreturn text",
            ),
            pass: false,
        },
        RubricCase {
            id: "mcp-get-page",
            tool: "get-page",
            args: json!({"query": "install"}),
            pass: false,
        },
        RubricCase {
            id: "keyword-class",
            tool: "run_code",
            args: program(
                "Call class",
                "text = await tools[\"class\"](query=\"ptc\")\nreturn text",
            ),
            pass: true,
        },
        RubricCase {
            id: "keyword-class",
            tool: "run_code",
            args: program(
                "Call class",
                "text = await tools[ 'class' ](query=\"ptc\")\nreturn text",
            ),
            pass: true,
        },
        RubricCase {
            id: "keyword-class",
            tool: "run_code",
            args: program(
                "Attribute",
                "text = await tools.class(query=\"ptc\")\nreturn text",
            ),
            pass: false,
        },
        RubricCase {
            id: "keyword-class",
            tool: "class",
            args: json!({"query": "ptc"}),
            pass: false,
        },
        RubricCase {
            id: "keyword-class",
            tool: "run_code",
            args: program("Empty", "return \"ptc\""),
            pass: false,
        },
        RubricCase {
            id: "scout-gather",
            tool: "run_code",
            args: program(
                "Map shutdown",
                "await asyncio.gather(\n  tools.agent(agent=\"scout\", prompt=\"How Tokio shuts down\"),\n  tools.agent(agent=\"scout\", prompt=\"How async-std shuts down\"),\n  tools.agent(agent=\"scout\", prompt=\"How smol shuts down\"),\n)\nreturn \"mapped\"",
            ),
            pass: true,
        },
        RubricCase {
            id: "scout-gather",
            tool: "run_code",
            args: program(
                "Map shutdown",
                "await asyncio.gather(\n  tools.agent(agent = \"scout\", prompt=\"Tokio\"),\n  tools[\"agent\"](agent='scout', prompt=\"smol\"),\n)\nreturn \"mapped\"",
            ),
            pass: true,
        },
        RubricCase {
            id: "scout-gather",
            tool: "agent",
            args: json!({"agent": "scout", "prompt": "map shutdown"}),
            pass: false,
        },
        RubricCase {
            id: "scout-gather",
            tool: "run_code",
            args: program(
                "One scout",
                "return await tools.agent(agent=\"scout\", prompt=\"map all three\")",
            ),
            pass: false,
        },
        RubricCase {
            id: "scout-gather",
            tool: "run_code",
            args: program(
                "Artisan",
                "await asyncio.gather(\n  tools.agent(agent=\"artisan\", prompt=\"Tokio\"),\n  tools.agent(agent=\"artisan\", prompt=\"smol\"),\n)\nreturn \"mapped\"",
            ),
            pass: false,
        },
        RubricCase {
            id: "scout-gather",
            tool: "run_code",
            args: program(
                "Fetch yourself",
                "await asyncio.gather(\n  tools.agent(agent=\"scout\", prompt=\"Tokio\"),\n  tools.agent(agent=\"scout\", prompt=\"smol\"),\n)\nawait tools.web_search(query=\"shutdown\")\nreturn \"mapped\"",
            ),
            pass: false,
        },
        RubricCase {
            id: "cargo-test",
            tool: "run_code",
            args: program(
                "Run agent tests",
                "out = await tools.shell(command=\"cargo test -p mycode-agent\")\nreturn out",
            ),
            pass: true,
        },
        RubricCase {
            id: "cargo-test",
            tool: "run_code",
            args: program(
                "Run agent tests",
                "out = await tools[\"shell\"](command=\"cargo test -p mycode-agent\")\nreturn out",
            ),
            pass: true,
        },
        RubricCase {
            id: "cargo-test",
            tool: "bash",
            args: json!({"command": "cargo test -p mycode-agent"}),
            pass: false,
        },
        RubricCase {
            id: "cargo-test",
            tool: "run_code",
            args: program(
                "Alias",
                "out = await tools.bash(command=\"cargo test -p mycode-agent\")\nreturn out",
            ),
            pass: false,
        },
        RubricCase {
            id: "cargo-test",
            tool: "run_code",
            args: program("Wrong command", "return await tools.shell(command='ls')"),
            pass: false,
        },
        RubricCase {
            id: "cargo-test",
            tool: "run_code",
            args: program(
                "Edit after test",
                "await tools.shell(command=\"cargo test -p mycode-agent\")\nawait tools.edit(path='src/lib.rs', old='a', new='b')",
            ),
            pass: false,
        },
        RubricCase {
            id: "lookup-no-refetch",
            tool: "run_code",
            args: program(
                "Find refresh",
                "return await tools.grep(pattern=\"refresh_token\", include=\"src/auth.rs\")",
            ),
            pass: true,
        },
        RubricCase {
            id: "lookup-no-refetch",
            tool: "run_code",
            args: program(
                "Read auth",
                "return await tools.read(path=\"src/auth.rs\")\n# refresh_token",
            ),
            pass: false,
        },
        RubricCase {
            id: "lookup-no-refetch",
            tool: "run_code",
            args: program(
                "Retry fetch",
                "await tools.fetch_content(url=\"https://example.invalid/auth\", goal=\"refresh_token\")\nreturn await tools.grep(pattern=\"refresh_token\", include=\"src/auth.rs\")",
            ),
            pass: false,
        },
        RubricCase {
            id: "lookup-no-refetch",
            tool: "fetch_content",
            args: json!({"url": "https://example.invalid/auth"}),
            pass: false,
        },
        RubricCase {
            id: "lookup-no-refetch",
            tool: "ask_user",
            args: json!({"questions": [{"question": "Which file?"}]}),
            pass: false,
        },
    ]
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
