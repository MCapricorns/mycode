//! Builds the default system prompt from the active tool registry.

use std::fmt::Write;

use mycode_tools::ToolRegistry;
use mycode_tools::builtin::{RUN_CODE_EXAMPLE_CODE, RUN_CODE_EXAMPLE_DESCRIPTION};

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

/// System-prompt contract for `run_code`, shaped like dsh's PTC instructions:
/// both required arguments are named, then one complete call the model can copy.
fn grouped_section(tools: &ToolRegistry) -> Option<String> {
    let _registered = tools.get("run_code")?;
    let description = serde_json::to_string(RUN_CODE_EXAMPLE_DESCRIPTION)
        .expect("example description is a string");
    let code = serde_json::to_string(RUN_CODE_EXAMPLE_CODE).expect("example code is a string");
    Some(format!(
        "<grouped_execution>\n\
## Writing code for run_code\n\
\n\
`run_code` takes two required arguments: `description`, a short summary of what the program does, and `code` — the body of an async Python function. Top-level `await` and `return` work. The interpreter is embedded in mycode: Node.js is not used, and Python does not need to be installed. One obvious call stays direct. When a step needs several reads, greps, finds, edits, or page fetches, call `run_code` once:\n\
\n\
`run_code({{ description: {description}, code: {code} }})`\n\
\n\
Inside the program:\n\
- Call tools as `await tools.name(arg=value)` with the same argument names as the direct tool.\n\
- A failed tool raises. `except Exception as e` sees `e.toolName` and `e.message`.\n\
- Independent read-only calls MAY overlap under `await gather(...)` (`read`, `grep`, `find`, `web_search`, and `fetch_content` run concurrently, up to 8 at a time; `write`, `edit`, and the shell run alone, in submission order). Sequence dependent work with `await`.\n\
- Emit results with `return` and/or `print(...)`. Only what you print or return is program output, capped at 8000 bytes. Every other intermediate result stays out of the conversation, so extract just what you need.\n\
- Do not call `run_code` from inside the program. The program stops after 48 tool calls, 20000 steps, or 120 seconds.\n\
- A rename or the same edit across several files is one program: read and `edit` inside it, then return the paths. Do not emit one `edit` per file.\n\
- Subset: assignment (`a, b = ...`, `obj[key] = value`, `+=`), if/else, `a if c else b`, for (including `for i, item in enumerate(...)`), while, try/except, lists, dicts (`items`/`get`/`in`), f-strings, list comprehensions, comparisons, + - * / (including `\"S\" * n`), `len`, `range`, `str`, `int`, `enumerate`, `zip`, `sorted`, `min`, `max`, `sum`, slices `value[start:end]`, string split/strip/startswith/endswith/lower/upper/join/replace, list append. No import, classes, lambda, or match.\n\
</grouped_execution>"
    ))
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
            weight: TaskWeight::GroupInline,
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

/// Whether a model's first tool call matches the judgment for `weight`.
///
/// A narrow multi-step task must be `run_code`. One edit may start with
/// `read` (the prompt says to read before changing a file) or go straight to
/// `edit` or `write`. Broad research may be `scout` or inline `run_code`.
#[must_use]
pub fn accept_choice(weight: TaskWeight, tool: &str, arguments: &serde_json::Value) -> bool {
    match weight {
        TaskWeight::Direct => matches!(tool, "read" | "edit" | "write"),
        TaskWeight::GroupInline => tool == "run_code",
        TaskWeight::Broad => {
            tool == "run_code"
                || (tool == "agent"
                    && arguments.get("agent").and_then(serde_json::Value::as_str) == Some("scout"))
        }
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
            prompt.contains("one obvious call stays direct")
                || prompt.contains("One obvious call stays direct")
        }
        TaskWeight::GroupInline => {
            prompt.contains("run_code")
                && (prompt.contains("several reads")
                    || prompt.contains("several files")
                    || prompt.contains("Simple lookups stay inline"))
        }
        TaskWeight::Broad => {
            prompt.contains("`scout`")
                && (prompt.contains("You choose") || prompt.contains("You decide"))
                && prompt.contains("run_code")
        }
    }
}

/// The `<grouped_execution>` block. Identical for every registry that has
/// `run_code`, parent or child.
#[must_use]
pub fn grouped_execution_block(prompt: &str) -> Option<&str> {
    let start = prompt.find("<grouped_execution>")?;
    let end = prompt[start..].find("</grouped_execution>")? + "</grouped_execution>".len();
    Some(&prompt[start..start + end])
}

/// Builds the compact default prompt from the currently registered tools.
pub fn build_system_prompt(tools: &ToolRegistry) -> String {
    let mut prompt = String::from(IDENTITY);
    prompt.push_str("\n\nAvailable tools:");

    let entries = tools.prompt_entries();
    if entries.is_empty() {
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

    if let Some(section) = grouped_section(tools) {
        prompt.push_str("\n\n");
        prompt.push_str(&section);
    }
    if let Some(section) = web_section(tools) {
        prompt.push_str("\n\n");
        prompt.push_str(section);
    }
    if let Some(shell_tool) = shell_tool_name(tools) {
        prompt.push_str("\n\n");
        prompt.push_str(&tool_calling(&shell_tool, tools.get("run_code").is_some()));
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
    fn grouping_block_is_identical_with_or_without_the_agent_tool() {
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
        assert_eq!(parent_block, child_block);
        assert!(parent_block.contains("two required arguments"));
        assert!(parent_block.contains("description"));
        assert!(parent_block.contains("code"));
        assert!(parent_block.contains("run_code({ description:"));
        assert!(parent_block.contains("gather"));
        assert!(parent_block.contains("does not need to be installed"));
        assert!(parent_block.contains("Node.js is not used"));
        assert!(!parent_block.contains("JavaScript"));
        assert!(!parent_prompt.contains("console.log"));
        assert!(parent_prompt.contains("only print and return"));
        assert!(!parent_block.contains("Do not do that research inline"));
        assert!(!parent_block.contains("`scout`"));
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
        assert!(super::accept_choice(
            super::TaskWeight::Direct,
            "edit",
            &serde_json::json!({"path": "src/a.rs"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::Direct,
            "read",
            &serde_json::json!({"path": "src/a.rs"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Direct,
            "run_code",
            &serde_json::json!({"description": "One edit", "code": "return 1"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::Broad,
            "agent",
            &serde_json::json!({"agent": "scout", "prompt": "map auth"})
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
