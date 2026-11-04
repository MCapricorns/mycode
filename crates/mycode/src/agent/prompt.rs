//! Builds the default system prompt from the active tool registry.

use std::fmt::Write;

use crate::tools::ToolRegistry;

const HEADER: &str = "\
Complete the task with the tools listed below. Do not invent tools. \
Support claims with inspected sources or tool results. Distinguish source inspection, \
checks actually run, and unresolved gaps; do not claim a command ran without its result.";

/// Names the one shell tool in `tools`, if the process registered one.
pub fn shell_tool_name(tools: &ToolRegistry) -> Option<String> {
    tools
        .names()
        .into_iter()
        .find(|name| crate::tools::ShellKind::is_tool_name(name))
}

/// Calling rules follow the exposed registry.
fn tool_calling(tools: &ToolRegistry, code_mode: bool) -> String {
    let mut rules = String::from("<tool_calling>\n");
    if code_mode {
        rules.push_str(
            "- Only names under Available tools are directly callable. Tools under \
Tools inside run_code are called from JavaScript as `call(\"name\", { argument: value })`; \
use their argument schemas below. `script` accepts JavaScript only (embedded QuickJS with \
top-level await and Promises; no Node.js, require, fs, fetch, or imports).\n\
- Batch independent reads/searches with `await Promise.all([...callAsync(name, args)])`. \
Parallel-safe calls run together; writes, shell commands, and other exclusive calls run in submission order. \
Use `await callAsync(name, args)` or blocking `call(name, args)` for dependent steps. \
Calls return `{ content, is_error }` (`result.content[0].Text`). Await required work before \
`complete(value)`: it ends the script and cancels outstanding calls. Use `log(value)` for \
progress and `budget()` for remaining limits. Discover remote tool schemas before calling them; \
a `run_code` script cannot call `run_code` or `wait_code`.\n",
        );
        let _ = writeln!(
            rules,
            "- Each cell permits at most {} `call`/`callAsync` invocations combined.",
            crate::tools::code_mode::MAX_HOST_CALLS
        );
    }
    if code_mode && tools.is_exposed("wait_code") && tools.get("wait_code").is_some() {
        rules.push_str(
            "- When `run_code` yields a `cell_id`, collect that same cell with `wait_code` until it terminates; do not resubmit the script.\n",
        );
    }
    if ["find", "grep", "read"]
        .iter()
        .all(|name| tools.get(name).is_some())
    {
        rules.push_str(
            "- Use dedicated tools first: `find` locates files/directories (pattern `*` with `max_depth` 1 lists one directory), `grep` searches contents, `read` opens files.\n",
        );
    }
    if tools.get("write").is_some() && tools.get("edit").is_some() {
        let names = if code_mode {
            "`call(\"write\", …)` and `call(\"edit\", …)`"
        } else {
            "the registered `write` and `edit` tools"
        };
        let _ = writeln!(
            rules,
            "- Read existing content before changing it. Change files by {names}. Use revision tokens from the latest read when required."
        );
    }
    if let Some(shell_tool) = shell_tool_name(tools) {
        let _ = writeln!(
            rules,
            "- Use `{shell_tool}` only for builds, tests, git, package managers, and running programs; never for listing, reading, or searching when dedicated tools are available.\n\
- Commands must exit on their own (pass non-interactive flags; no dev servers or watchers). On Windows, keep console work in the background with captured output.\n\
- `mode` `script` runs a shell command; `mode` `program` runs an executable with explicit `args` and no shell parsing."
        );
    }
    rules.push_str(
        "- Editing or recalling a chat message does not restore or delete workspace files.\n</tool_calling>",
    );
    rules
}

fn push_code_tools(prompt: &mut String, tools: &ToolRegistry) {
    let hidden: Vec<_> = tools
        .names()
        .into_iter()
        .filter(|name| !tools.is_exposed(name))
        .filter(|name| !matches!(name.as_str(), "run_code" | "wait_code"))
        .collect();
    if hidden.is_empty() {
        return;
    }
    prompt.push_str("\n\nTools inside run_code (use call(name, args)):");
    for name in hidden {
        let Some(tool) = tools.get(&name) else {
            continue;
        };
        if tool.schema_on_demand() {
            continue;
        }
        let spec = tool.spec();
        let _ = write!(
            prompt,
            "\n- {name}: {description}\n  Arguments JSON Schema: {schema}",
            description = spec.description,
            schema = spec.params_schema,
        );
    }
}

/// Builds the compact default prompt from the currently registered tools.
pub fn build_system_prompt(tools: &ToolRegistry) -> String {
    let mut prompt = String::from(HEADER);
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

    let code_mode = tools.get("run_code").is_some() && tools.is_exposed("run_code");
    if !tools.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&tool_calling(tools, code_mode));
    }
    if code_mode {
        push_code_tools(&mut prompt, tools);
    }
    prompt
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::tools::{
        DetectedShell, ShellKind, ShellTool, Tool, ToolRegistry, register_builtins,
    };

    use super::build_system_prompt;

    fn registry_with(shell: ShellKind, program: &str) -> Arc<ToolRegistry> {
        let registry = Arc::new(ToolRegistry::new());
        let builtins = ToolRegistry::new();
        register_builtins(&builtins);
        for name in builtins.names() {
            if !ShellKind::is_tool_name(&name) {
                registry.register(builtins.get(&name).unwrap());
            }
        }
        registry.register(Arc::new(ShellTool::forcing(DetectedShell {
            kind: shell,
            program: PathBuf::from(program),
        })));
        expose_code_mode(&registry, &[shell.tool_name(), "run_code", "wait_code"]);
        registry
    }

    fn expose_code_mode(registry: &Arc<ToolRegistry>, exposed: &[&str]) {
        let cells = Arc::new(crate::tools::CodeCells::new());
        registry.register(Arc::new(crate::tools::RunCodeTool::new(
            Arc::clone(registry),
            Arc::clone(&cells),
            "prompt-test".to_owned(),
        )));
        registry.register(Arc::new(crate::tools::WaitCodeTool::new(
            cells,
            "prompt-test".to_owned(),
        )));
        registry.expose_only(exposed);
    }

    #[test]
    fn code_mode_carries_hidden_tool_schemas_without_exposing_them_to_the_provider() {
        let registry = registry_with(ShellKind::Bash, "/bin/bash");
        let prompt = build_system_prompt(&registry);
        let (direct, hidden) = prompt
            .split_once("\n\nTools inside run_code")
            .expect("hidden catalog");
        assert!(direct.contains("\n- run_code:"));
        assert!(!direct.contains("\n- read:"));
        assert!(!registry.specs().iter().any(|spec| spec.name == "read"));
        for name in ["read", "write", "edit", "find", "grep"] {
            let spec = registry.get(name).unwrap().spec();
            assert!(hidden.contains(&format!("\n- {name}:")));
            assert!(hidden.contains(&format!("Arguments JSON Schema: {}", spec.params_schema)));
        }
    }

    #[test]
    fn powershell_prompt_names_the_powershell_tool_and_hides_bash() {
        for (kind, program, label) in [
            (
                ShellKind::WindowsPowerShell,
                "powershell.exe",
                "Windows PowerShell 5.1",
            ),
            (ShellKind::Pwsh, "pwsh.exe", "PowerShell 7"),
        ] {
            let registry = registry_with(kind, program);
            let prompt = build_system_prompt(&registry);
            assert!(prompt.contains("`powershell`"), "{prompt}");
            assert!(!prompt.contains("\n- bash:"), "{prompt}");
            let shell = ShellTool::forcing(DetectedShell {
                kind,
                program: PathBuf::from(program),
            });
            assert!(shell.description().contains(label));
        }
    }
}
