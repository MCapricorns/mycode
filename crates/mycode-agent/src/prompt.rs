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

fn tool_calling(shell_tool: &str) -> String {
    let hint = edit_hint(shell_tool);
    format!(
        "<tool_calling>\n\
- Independent calls in one response run together. Do not wait between them.\n\
- Read existing content before changing it. Prefer `read`, `grep`, and `find` to inspect files.\n\
- `write`, `edit`, and `{shell_tool}` can all change files. {hint}\n\
- Use `{shell_tool}` with `mode` `program` for one kernel-loadable program and an explicit argument vector. That path does not start a shell.\n\
- Never use `{shell_tool}` to talk to the user.\n\
- Editing or recalling a chat message does not restore or delete workspace files.\n\
</tool_calling>"
    )
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

    if let Some(shell_tool) = shell_tool_name(tools) {
        prompt.push_str("\n\n");
        prompt.push_str(&tool_calling(&shell_tool));
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
}
