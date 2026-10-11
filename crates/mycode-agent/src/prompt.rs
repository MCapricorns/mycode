//! Builds the default system prompt from the active tool registry.

use std::fmt::Write;

use mycode_tools::ToolRegistry;

/// Tool-list header. Deliberately not an identity sentence: every caller
/// (session, subagent, compaction) opens its prompt with its own identity,
/// so a second one here would read as a duplication.
const IDENTITY: &str = "\
Complete the task with the tools listed below. Do not invent tools. \
Tool results are the source of truth; do not claim a command ran unless its result is in the conversation.";

/// Names the one shell tool in `tools`, if the process registered one.
fn shell_tool_name(tools: &ToolRegistry) -> Option<String> {
    let names = tools.names();
    ["powershell", "bash", "zsh", "sh", "cmd"]
        .into_iter()
        .find(|name| names.iter().any(|registered| registered == name))
        .map(str::to_owned)
}

fn edit_hint(shell_tool: &str) -> &'static str {
    match shell_tool {
        "powershell" => {
            "For a shell edit, use `mode` `script` and pipe a literal here-string to `python` or `python3` (`@'...'@ | python -`)."
        }
        "bash" | "zsh" | "sh" => {
            "For a shell edit, use `mode` `script` and run Python (`python3` or `python`) with a quoted heredoc or a short script."
        }
        "cmd" => {
            "Prefer `write` and `edit` for file changes. `mode` `script` is cmd.exe, not bash or PowerShell."
        }
        _ => "For a shell edit, use `mode` `script`.",
    }
}

fn tool_calling(shell_tool: &str, grouped: bool) -> String {
    let hint = edit_hint(shell_tool);
    let grouped_line = if grouped {
        "- When one step needs several reads, greps, finds, edits, or page fetches, call `run_code` once. One obvious call stays direct.\n"
    } else {
        ""
    };
    format!(
        "<tool_calling>\n\
- Independent calls in one response run together. Do not wait between them.\n\
{grouped_line}\
- Read existing content before changing it. Prefer `read`, `grep`, and `find` to inspect files.\n\
- `write`, `edit`, and `{shell_tool}` can all change files. {hint}\n\
- Use `{shell_tool}` with `mode` `program` for one kernel-loadable program and an explicit argument vector. That path does not start a shell.\n\
- Never use `{shell_tool}` to talk to the user.\n\
- Editing or recalling a chat message does not restore or delete workspace files.\n\
</tool_calling>"
    )
}

fn is_shell_name(name: &str) -> bool {
    matches!(name, "powershell" | "bash" | "zsh" | "sh" | "cmd")
}

/// PTC contract plus the generated `tools.*` SDK. Stable for a registry:
/// tool order is sorted and the Python probe is cached for the process.
fn tools_section(tools: &ToolRegistry) -> Option<String> {
    tools.get("run_code")?;
    let mut section = String::from("<tools>\n");
    if let Some(message) = mycode_tools::python_unavailable_message() {
        section
            .push_str("Python is unavailable. `run_code` cannot execute until it is installed.\n");
        section.push_str(&message);
        section.push_str("\n\n");
    }
    section.push_str(
        "`run_code` is the only tool you can call directly — a tool call naming any other tool fails. Reach every tool the SDK declares below from inside the program.\n\n",
    );
    section.push_str("```python\n");
    section.push_str(
        "# run_code(description, code): description is a short summary shown in the UI; code is the body of an async function. Top-level await and return work.\n",
    );
    section.push_str(
        "# Call tools as await tools.name(arg=value) or await tools.name({...}). A failed call raises ToolCallError (e.toolName, e.message); try/except it to continue.\n",
    );
    section.push_str(
        "# Independent read-only calls MAY overlap under asyncio.gather (read, grep, find, web_search, fetch_content run concurrently; mutating calls run alone, in submission order).\n",
    );
    section.push_str(
        "# Only print(...) and return reach the conversation. Other results stay out, so extract just what you need.\n",
    );
    for line in sdk_lines(tools) {
        section.push_str(&line);
        section.push('\n');
    }
    section.push_str("```\n\n");
    section.push_str(
        "Use `tools.shell(...)` to run external programs (build, test, lint, git, package managers, servers, project scripts); `tools.read` / `tools.grep` / `tools.find` / `tools.edit` / `tools.write` for files; `tools.web_search` / `tools.fetch_content` for the web. Never call `subprocess` or `os.system` from Python directly.\n",
    );
    if tools.get("agent").is_some() {
        section
            .push_str("Delegating broad research to scout via `tools.agent` is your judgment.\n");
    }
    section.push_str("</tools>");
    Some(section)
}

fn sdk_lines(tools: &ToolRegistry) -> Vec<String> {
    let mut lines = Vec::new();
    let mut shell_spec = None;
    let mut shell_name = None;
    for name in tools.names() {
        if name == "run_code" {
            continue;
        }
        let Some(tool) = tools.get(&name) else {
            continue;
        };
        let spec = tool.spec();
        if is_shell_name(&name) {
            if shell_spec.is_none() {
                shell_name = Some(name);
                shell_spec = Some(spec);
            }
            continue;
        }
        lines.push(sdk_entry(&name, &spec.description, &spec.params_schema));
    }
    if let Some(spec) = shell_spec {
        let name = shell_name.as_deref().unwrap_or("shell");
        let hint = match name {
            "powershell" => "here-string edits go to python",
            "bash" | "zsh" | "sh" => "quoted heredoc edits go to Python",
            "cmd" => "prefer tools.write and tools.edit",
            _ => "script or program mode",
        };
        let blurb =
            format!("mode `script` runs `{name}` ({hint}); mode `program` launches one executable");
        lines.push(sdk_entry("shell", &blurb, &spec.params_schema));
    }
    lines.sort();
    lines
}

fn sdk_entry(name: &str, description: &str, schema: &serde_json::Value) -> String {
    let comment = first_sentence(description).replace('\n', " ");
    format!("# {name}: {comment}\n{}", python_signature(name, schema))
}

fn first_sentence(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = flat.find(". ").map(|index| index + 1).unwrap_or(flat.len());
    flat.chars().take(cut.min(180)).collect()
}

fn python_signature(name: &str, schema: &serde_json::Value) -> String {
    let required = schema
        .get("required")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut params = Vec::new();
    if let Some(properties) = schema.get("properties").and_then(|value| value.as_object()) {
        for (field, field_schema) in properties {
            let (ty, nullable) = python_type(field_schema);
            let optional = nullable || !required.iter().any(|name| name == field);
            if optional {
                params.push(format!("{field}: {ty} | None = None"));
            } else {
                params.push(format!("{field}: {ty}"));
            }
        }
    }
    if params.is_empty() {
        format!("async def {name}() -> str: ...")
    } else {
        format!("async def {name}(*, {}) -> str: ...", params.join(", "))
    }
}

fn python_type(schema: &serde_json::Value) -> (String, bool) {
    if schema.get("enum").is_some() {
        return ("str".to_owned(), false);
    }
    if let Some(types) = schema.get("type").and_then(|value| value.as_array()) {
        let mut nullable = false;
        let mut ty = None;
        for item in types {
            if item.as_str() == Some("null") {
                nullable = true;
                continue;
            }
            if ty.is_none()
                && let Some(name) = item.as_str()
            {
                ty = Some(scalar_type(name));
            }
        }
        return (ty.unwrap_or_else(|| "object".to_owned()), nullable);
    }
    if let Some(options) = schema
        .get("anyOf")
        .and_then(|value| value.as_array())
        .filter(|items| !items.is_empty())
    {
        let mut nullable = false;
        let mut ty = None;
        for option in options {
            if option.get("type").and_then(|value| value.as_str()) == Some("null") {
                nullable = true;
                continue;
            }
            if ty.is_none() {
                ty = Some(python_type(option).0);
            }
        }
        return (ty.unwrap_or_else(|| "object".to_owned()), nullable);
    }
    let ty = scalar_type(
        schema
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("object"),
    );
    (ty, false)
}

fn scalar_type(name: &str) -> String {
    match name {
        "string" => "str",
        "integer" => "int",
        "number" => "float",
        "boolean" => "bool",
        "array" => "list",
        "object" => "dict",
        "null" => "None",
        _ => "object",
    }
    .to_owned()
}

fn web_section(tools: &ToolRegistry) -> Option<&'static str> {
    let search = tools.get("web_search").is_some();
    let fetch = tools.get("fetch_content").is_some();
    if !search && !fetch {
        return None;
    }
    Some(
        "<web>\n\
`web_search` returns compact hits: title, URL, and a snippet of at most 240 characters. Duplicate URLs are dropped. An identical query is cached for 10 minutes. Snippets are leads, not evidence.\n\
`fetch_content` reads pages. Pass `goal` (the fact you need) and only matching excerpts return, about 1200 characters per page, marked `(excerpts for goal)`. Without `goal`, each page is capped at 8000 characters and marked `(truncated)` when cut. extract_failed, HTTP 422, or Unable to extract is permanent for that URL; do not sleep-retry.\n\
</web>",
    )
}

/// How hard a task is, for the choice eval. The prompt guides this; it does
/// not require one answer when both are reasonable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskWeight {
    /// One obvious call. Stay on the direct tool.
    Direct,
    /// Several lookups or edits in one step. Group them inline with `run_code`.
    GroupInline,
    /// One search. A direct `grep` or `find` is enough; `run_code` is also fine.
    Lookup,
    /// Broad research. `scout` is a fit; inline `run_code` is also allowed.
    Broad,
}

/// One scripted task the choice eval scores.
#[derive(Clone, Copy, Debug)]
pub struct ChoiceScenario {
    /// Stable id for reports.
    pub id: &'static str,
    /// User task, shown to a model when a live eval runs.
    pub task: &'static str,
    /// What the prompt should make reasonable.
    pub weight: TaskWeight,
}

/// Scripted tasks. Simple work stays inline; broad research may go to scout.
#[must_use]
pub fn choice_scenarios() -> &'static [ChoiceScenario] {
    &[
        ChoiceScenario {
            id: "narrow-search",
            task: "In src/turn.rs and src/prompt.rs, which functions mention dispatch_tool? Return path:line only.",
            weight: TaskWeight::Lookup,
        },
        ChoiceScenario {
            id: "one-edit",
            task: "In src/a.rs line 3, change recieve to receive. That is the whole change.",
            weight: TaskWeight::Direct,
        },
        ChoiceScenario {
            id: "broad-research",
            task: "Map how authentication works across this repository and what the vendor's current docs say about token refresh. I need a short map before any edit.",
            weight: TaskWeight::Broad,
        },
    ]
}

fn program_code(arguments: &serde_json::Value) -> &str {
    arguments
        .get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// Whether a model's first tool call matches PTC-only judgment for `weight`.
///
/// Every task starts with `run_code`. A lookup's program calls `tools.grep`
/// or `tools.find`. One edit's program calls `tools.edit` or `tools.write`.
/// Broad research may call `tools.agent` for scout or stay inline; both are
/// `run_code`.
#[must_use]
pub fn accept_choice(weight: TaskWeight, tool: &str, arguments: &serde_json::Value) -> bool {
    if tool != "run_code" {
        return false;
    }
    let code = program_code(arguments);
    match weight {
        TaskWeight::Direct => {
            (code.contains("tools.edit")
                || code.contains("tools.write")
                || code.contains("tools.read"))
                && !code.contains("tools.agent")
        }
        TaskWeight::Lookup => {
            code.contains("tools.grep")
                || code.contains("tools.find")
                || code.contains("tools.read")
        }
        TaskWeight::GroupInline | TaskWeight::Broad => true,
    }
}

/// Whether `prompt` states the judgment for `weight` without forbidding the
/// other reasonable choice.
#[must_use]
pub fn prompt_guides(prompt: &str, weight: TaskWeight) -> bool {
    let forces_scout = prompt.contains("Do not do that research inline")
        || prompt.contains("must dispatch")
        || prompt.contains("only `scout`");
    if forces_scout {
        return false;
    }
    match weight {
        TaskWeight::Direct => {
            prompt.contains("run_code")
                && (prompt.contains("tools.edit") || prompt.contains("tools.write"))
        }
        TaskWeight::Lookup => prompt.contains("tools.grep") || prompt.contains("tools.find"),
        TaskWeight::GroupInline => {
            prompt.contains("run_code")
                && (prompt.contains("asyncio.gather") || prompt.contains("tools.edit"))
        }
        TaskWeight::Broad => {
            prompt.contains("scout")
                && (prompt.contains("judgment")
                    || prompt.contains("your judgment")
                    || prompt.contains("You decide"))
                && prompt.contains("run_code")
        }
    }
}

/// The `<tools>` block: the PTC rule plus the SDK for this registry.
#[must_use]
pub fn grouped_execution_block(prompt: &str) -> Option<&str> {
    let start = prompt.find("<tools>")?;
    let end = prompt[start..].find("</tools>")? + "</tools>".len();
    Some(&prompt[start..start + end])
}

/// Builds the compact default prompt from the currently registered tools.
pub fn build_system_prompt(tools: &ToolRegistry) -> String {
    let mut prompt = String::from(IDENTITY);
    prompt.push_str("\n\nAvailable tools:");

    let entries = tools.prompt_entries();
    if tools.get("run_code").is_some() {
        prompt.push_str("\n- run_code: the only directly callable tool.");
        if let Some(section) = tools_section(tools) {
            prompt.push_str("\n\n");
            prompt.push_str(&section);
        }
    } else if entries.is_empty() {
        prompt.push_str("\n(none)");
    } else {
        for (name, snippet) in entries {
            write!(prompt, "\n- {name}").expect("writing to a String cannot fail");
            let snippet = snippet
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty());
            if let Some(detail) = snippet {
                let detail = detail
                    .strip_prefix(&name)
                    .and_then(|text| text.strip_prefix(':'))
                    .map(str::trim)
                    .unwrap_or(detail);
                write!(prompt, ": {detail}").expect("writing to a String cannot fail");
            }
        }
    }

    if let Some(section) = web_section(tools) {
        prompt.push_str("\n\n");
        prompt.push_str(section);
    }
    if tools.get("run_code").is_none()
        && let Some(shell_tool) = shell_tool_name(tools)
    {
        prompt.push_str("\n\n");
        prompt.push_str(&tool_calling(&shell_tool, false));
    }
    prompt
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use mycode_tools::{
        DetectedShell, ShellKind, ShellTool, Tool, ToolRegistry, register_builtins,
    };

    use super::build_system_prompt;

    fn registry_with(shell: ShellKind, program: &str) -> ToolRegistry {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(ShellTool::forcing(DetectedShell {
            kind: shell,
            program: PathBuf::from(program),
        })));
        registry
    }

    #[test]
    fn prompt_names_the_active_shell_tool_and_not_exec_or_task() {
        let registry = ToolRegistry::new();
        register_builtins(&registry);
        let prompt = build_system_prompt(&registry);
        let shell = mycode_tools::resolved_shell().kind.tool_name();
        assert!(prompt.contains(&format!("`{shell}`")), "{prompt}");
        assert!(prompt.contains("`program`"));
        assert!(prompt.contains("`script`"));
        assert!(!prompt.contains("`exec`"));
        assert!(!prompt.contains("`task`"));
        assert!(!prompt.contains("translated"));
    }

    #[test]
    fn powershell_51_prompt_forbids_bash_chaining() {
        let registry = registry_with(ShellKind::WindowsPowerShell, "powershell.exe");
        let prompt = build_system_prompt(&registry);
        assert!(prompt.contains("`powershell`"), "{prompt}");
        assert!(!prompt.contains("\n- bash:"), "{prompt}");
        assert!(prompt.contains("here-string"), "{prompt}");
        assert!(!prompt.contains("quoted heredoc"), "{prompt}");
        assert!(!prompt.contains("translated"), "{prompt}");
        assert!(
            !prompt.contains("`shell`"),
            "the model must not be told to call `shell`:\n{prompt}"
        );
    }

    #[test]
    fn bash_prompt_teaches_a_quoted_heredoc() {
        let registry = registry_with(ShellKind::Bash, "/bin/bash");
        let prompt = build_system_prompt(&registry);
        assert!(prompt.contains("`bash`"), "{prompt}");
        assert!(prompt.contains("quoted heredoc"), "{prompt}");
        assert!(
            prompt.contains("`write`, `edit`, and `bash`"),
            "write and edit stay alongside bash, got:\n{prompt}"
        );
        assert!(
            prompt.contains("`mode` `script`"),
            "file edits belong on script mode, got:\n{prompt}"
        );
        assert!(
            !prompt.contains("only for pipelines"),
            "shell must not be limited to pipelines:\n{prompt}"
        );
        assert!(
            prompt.contains("does not restore or delete workspace files"),
            "chat rewind must be documented as chat-only:\n{prompt}"
        );
        assert!(!prompt.contains("here-string"), "{prompt}");
        assert!(!prompt.contains("translated"), "{prompt}");
        let shell = ShellTool::forcing(DetectedShell {
            kind: ShellKind::Bash,
            program: PathBuf::from("/bin/bash"),
        });
        let description = shell.description();
        assert!(
            description.contains("quoted heredoc"),
            "bash description must allow Python heredocs, got:\n{description}"
        );
        assert!(
            description.contains("`mode` `program`"),
            "program mode stays on the shell tool, got:\n{description}"
        );
        assert!(
            !description.contains("`exec`"),
            "shell description must not name a separate exec tool, got:\n{description}"
        );
    }

    #[test]
    fn powershell_7_prompt_allows_native_chaining() {
        let registry = registry_with(ShellKind::Pwsh, "pwsh.exe");
        let prompt = build_system_prompt(&registry);
        assert!(prompt.contains("powershell:"), "{prompt}");
        assert!(prompt.contains("here-string"), "{prompt}");
        let shell = ShellTool::forcing(DetectedShell {
            kind: ShellKind::Pwsh,
            program: PathBuf::from("pwsh.exe"),
        });
        assert!(shell.description().contains("PowerShell 7"));
        assert!(!shell.description().contains("syntax errors"));
    }

    #[test]
    fn ptc_block_lists_only_the_registry_and_forbids_direct_calls() {
        let parent = ToolRegistry::new();
        register_builtins(&parent);
        let child = ToolRegistry::new();
        child.register(Arc::new(mycode_tools::builtin::ReadTool));
        child.register(Arc::new(mycode_tools::builtin::GrepTool));
        child.register(Arc::new(mycode_tools::builtin::RunCodeTool));
        let parent_prompt = build_system_prompt(&parent);
        let child_prompt = build_system_prompt(&child);
        let parent_block = super::grouped_execution_block(&parent_prompt).expect("parent block");
        let child_block = super::grouped_execution_block(&child_prompt).expect("child block");
        assert!(
            parent_block.contains("only tool you can call directly"),
            "{parent_block}"
        );
        assert!(child_block.contains("only tool you can call directly"));
        assert!(parent_block.contains("async def read("));
        assert!(parent_block.contains("async def shell("), "{parent_block}");
        assert!(child_block.contains("async def grep("));
        assert!(!child_block.contains("async def shell("), "{child_block}");
        assert!(!child_block.contains("async def write("));
        assert!(parent_block.contains("tools.shell"));
        assert!(parent_block.contains("Never call `subprocess`"));
        assert!(!parent_block.contains("does not need to be installed"));
        assert!(!parent_block.contains("one obvious call stays direct"));
        assert!(!parent_prompt.contains("console.log"));
        let again = build_system_prompt(&parent);
        assert_eq!(
            parent_prompt, again,
            "the tools section must stay byte-stable"
        );
    }

    #[test]
    fn choice_prompts_guide_difficulty_without_forcing_scout() {
        let registry = ToolRegistry::new();
        register_builtins(&registry);
        let prompt = build_system_prompt(&registry);
        assert!(
            super::prompt_guides(&prompt, super::TaskWeight::Direct),
            "{prompt}"
        );
        assert!(
            super::prompt_guides(&prompt, super::TaskWeight::GroupInline),
            "{prompt}"
        );
        for scenario in super::choice_scenarios() {
            if scenario.weight == super::TaskWeight::Broad {
                assert!(
                    !super::prompt_guides(&prompt, scenario.weight),
                    "without agent, broad research is not aimed at scout:\n{prompt}"
                );
            }
        }
        assert!(!super::accept_choice(
            super::TaskWeight::Lookup,
            "grep",
            &serde_json::json!({"pattern": "dispatch_tool"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::Lookup,
            "run_code",
            &serde_json::json!({"description": "Find dispatch", "code": "hits = await tools.grep(pattern='dispatch_tool')\nreturn hits"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Lookup,
            "bash",
            &serde_json::json!({"command": "rg dispatch"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::GroupInline,
            "run_code",
            &serde_json::json!({"description": "Find dispatch", "code": "return 1"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::GroupInline,
            "agent",
            &serde_json::json!({"agent": "scout"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Direct,
            "edit",
            &serde_json::json!({"path": "src/a.rs"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::Direct,
            "run_code",
            &serde_json::json!({"description": "One edit", "code": "await tools.edit(path='src/a.rs')"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Direct,
            "run_code",
            &serde_json::json!({"description": "One edit", "code": "return 1"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::Broad,
            "run_code",
            &serde_json::json!({"description": "Map auth", "code": "return await tools.agent(agent='scout', prompt='map auth')"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::Broad,
            "run_code",
            &serde_json::json!({"description": "Map auth inline", "code": "return 1"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Broad,
            "web_search",
            &serde_json::json!({"query": "auth"})
        ));
    }
}
