//! Builds the default system prompt from the active tool registry.

use std::fmt::Write;

use mycode_tools::ToolRegistry;

/// Tool-list header. Deliberately not an identity sentence: every caller
/// (session, subagent, compaction) opens its prompt with its own identity,
/// so a second one here would read as a duplication.
const HEADER: &str = "\
Complete the task with the tools listed below. Do not invent tools. \
Tool results are the source of truth; do not claim a command ran unless its result is in the conversation.";

/// Names the one shell tool in `tools`, if the process registered one.
pub fn shell_tool_name(tools: &ToolRegistry) -> Option<String> {
    tools
        .names()
        .into_iter()
        .find(|name| mycode_tools::ShellKind::is_tool_name(name))
}

fn edit_hint(shell_tool: &str) -> &'static str {
    match shell_tool {
        "powershell" => {
            "For a change they cannot express (a bulk or generated rewrite), use `mode` `script` and pipe a literal here-string to `python` or `python3` (`@'...'@ | python -`)."
        }
        "bash" | "zsh" | "sh" => {
            "For a change they cannot express (a bulk or generated rewrite), use `mode` `script` and run Python (`python3` or `python`) with a quoted heredoc or a short script."
        }
        "cmd" => "`mode` `script` is cmd.exe, not bash or PowerShell; do not edit files with it.",
        _ => "For a change they cannot express, use `mode` `script`.",
    }
}

fn tool_calling(shell_tool: &str) -> String {
    let hint = edit_hint(shell_tool);
    format!(
        "<tool_calling>\n\
- Put independent reads in one response: `read`, `grep`, `find`, and MCP lookups emitted together run at the same time. Edits and `{shell_tool}` calls run one at a time, in the order you emit them.\n\
- Use the dedicated tools first: `find` lists and locates files and directories (pattern `*` with `max_depth` 1 lists one directory), `grep` searches contents, `read` opens files.\n\
- Use `{shell_tool}` only for what the dedicated tools cannot do: builds, tests, git, package managers, and running programs. Never use it to list directories, read files, or search text.\n\
- A `{shell_tool}` call waits for its command to exit. Do not start one that waits for input or never exits (a dev server, a watcher, an interactive prompt): it holds the turn until the timeout. Pass non-interactive flags instead.\n\
- Read existing content before changing it. Change files with `write` and `edit`. {hint}\n\
- `{shell_tool}` `mode` `program` runs one executable with an explicit `args` vector and no shell parsing. Prefer it when a command string would need fragile quoting.\n\
- Never use `{shell_tool}` to talk to the user.\n\
- Editing or recalling a chat message does not restore or delete workspace files.\n\
</tool_calling>"
    )
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
            prompt.contains("Change files with `write` and `edit`"),
            "write and edit are the file-change tools, got:\n{prompt}"
        );
        assert!(
            prompt.contains("Use `bash` only for what the dedicated tools cannot do"),
            "the shell is the fallback, got:\n{prompt}"
        );
        assert!(
            prompt.contains("Never use it to list directories, read files, or search text"),
            "{prompt}"
        );
        assert!(
            prompt.contains("`max_depth` 1"),
            "directory listing belongs to find, got:\n{prompt}"
        );
        assert!(
            prompt.contains("`mode` `script`"),
            "bulk edits stay on script mode, got:\n{prompt}"
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
    fn agent_snippet_defers_role_names_to_the_dispatch_section() {
        struct Host;

        impl mycode_tools::builtin::AgentHost for Host {
            fn run_subagent<'a>(
                &'a self,
                _request: mycode_tools::builtin::SubagentRequest,
                _progress: &'a mycode_tools::ToolStream,
                _cancel: &'a tokio_util::sync::CancellationToken,
                _call_id: &'a str,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = Result<String, mycode_tools::ToolError>>
                        + Send
                        + 'a,
                >,
            > {
                Box::pin(async { Err(mycode_tools::ToolError::Execution("unused".to_owned())) })
            }
        }

        let registry = ToolRegistry::new();
        register_builtins(&registry);
        registry.register(Arc::new(mycode_tools::builtin::AgentTool::new(Arc::new(
            Host,
        ))));
        let prompt = build_system_prompt(&registry);
        assert!(
            prompt.contains("agent: hand one self-contained brief to a role from <dispatch>"),
            "the agent snippet must point at the dispatch section:\n{prompt}"
        );
        assert!(
            !prompt.contains("scout"),
            "role names come from the dispatch section only:\n{prompt}"
        );
    }
}
