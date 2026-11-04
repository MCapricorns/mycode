//! Builds the default system prompt from the active tool registry.

use std::fmt::Write;

use crate::tools::ToolRegistry;

const HEADER: &str = "\
Use only the tools listed below. Support claims with inspected sources or tool results; \
never claim a command ran without its result.";

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
            "- Call the direct tools by name. Call the tools under Tools inside run_code only from \
a `run_code` script, with the schemas listed there; a script cannot call `run_code` or \
`wait_code`.\n\
- In a script, batch independent reads and searches with `await Promise.all([...])` over \
`callAsync(name, args)` and await each dependent step. Writes, shell commands, and other \
exclusive calls run alone in submission order. Await required work before `complete(value)`, \
which ends the script and cancels outstanding calls.\n\
- When `run_code` returns a `cell_id`, collect that cell with `wait_code` until it \
finishes; never resubmit the script.\n",
        );
    }
    let shell = shell_tool_name(tools);
    if ["find", "grep", "read"]
        .iter()
        .all(|name| tools.get(name).is_some())
    {
        rules.push_str(
            "- Use `find` to locate files and directories (pattern `*` with `max_depth` 1 lists \
one directory), `grep` to search contents, and `read` to open files",
        );
        match &shell {
            Some(shell) => {
                let _ = writeln!(
                    rules,
                    "; use `{shell}` only for builds, tests, git, package managers, and running programs."
                );
            }
            None => rules.push_str(".\n"),
        }
    }
    if tools.get("write").is_some() && tools.get("edit").is_some() {
        rules.push_str("- Read a file before changing it with `edit` or `write`.\n");
    }
    rules.push_str("</tool_calling>");
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
    prompt.push_str("\n\nTools inside run_code:");
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
            "\n- {name}: {description}\n  Schema: {schema}",
            description = spec.description,
            schema = spec.params_schema,
        );
    }
}

/// Builds the compact default prompt from the currently registered tools.
pub fn build_system_prompt(tools: &ToolRegistry) -> String {
    let mut prompt = String::from(HEADER);
    let direct: Vec<String> = tools
        .specs()
        .iter()
        .map(|spec| format!("`{}`", spec.name))
        .collect();
    prompt.push_str("\n\nDirect tools: ");
    if direct.is_empty() {
        prompt.push_str("none.");
        return prompt;
    }
    prompt.push_str(&direct.join(", "));
    prompt.push('.');
    let code_mode = tools.get("run_code").is_some() && tools.is_exposed("run_code");
    prompt.push_str("\n\n");
    prompt.push_str(&tool_calling(tools, code_mode));
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
        crate::tools::enable_code_mode(
            &registry,
            &Arc::new(crate::tools::CodeCells::new()),
            "prompt-test",
        );
        registry
    }

    #[test]
    fn code_mode_carries_hidden_tool_schemas_without_exposing_them_to_the_provider() {
        let registry = registry_with(ShellKind::Bash, "/bin/bash");
        let prompt = build_system_prompt(&registry);
        let (direct, hidden) = prompt
            .split_once("\n\nTools inside run_code")
            .expect("hidden catalog");
        assert!(direct.contains("Direct tools: `bash`, `run_code`, `wait_code`."));
        assert!(!direct.contains("\n- read:"));
        assert!(!registry.specs().iter().any(|spec| spec.name == "read"));
        for name in ["read", "write", "edit", "find", "grep"] {
            let spec = registry.get(name).unwrap().spec();
            assert!(hidden.contains(&format!("\n- {name}:")));
            assert!(hidden.contains(&format!("Schema: {}", spec.params_schema)));
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
