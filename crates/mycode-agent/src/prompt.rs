//! Builds the default system prompt from the active tool registry.

use std::fmt::Write;

use mycode_tools::ToolRegistry;

/// Tool-list header. Deliberately not an identity sentence: every caller
/// (session, subagent, compaction) opens its prompt with its own identity,
/// so a second one here would read as a duplication.
const IDENTITY: &str = "Complete the task with the tools listed below. Do not invent tools.";

/// Tool-use contract. Only names tools this process can actually call.
const TOOL_CALLING: &str = "\
<tool_calling>
- Independent calls in one response run together. Do not wait between them.
- Read existing content before changing it. Prefer `read`, `grep`, and `find` to inspect files.
- `write`, `edit`, and `shell` can all change files. For a shell edit, use `mode` `script` and run Python (`python3` or `python`) with a quoted heredoc or a short script on a POSIX shell, or pipe a PowerShell here-string to `python`.
- Use `shell` with `mode` `program` for one kernel-loadable program and an explicit argument vector. That path does not start a shell.
- Never use `shell` to talk to the user.
- Editing or recalling a chat message does not restore or delete workspace files.
</tool_calling>";

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

    prompt.push_str("\n\n");
    prompt.push_str(TOOL_CALLING);
    prompt
}

#[cfg(test)]
mod tests {
    use mycode_tools::{ShellTool, Tool, ToolRegistry, register_builtins};

    use super::build_system_prompt;

    #[test]
    fn prompt_names_shell_modes_and_not_exec_or_task() {
        let registry = ToolRegistry::new();
        register_builtins(&registry);
        let prompt = build_system_prompt(&registry);
        assert!(prompt.contains("`shell`"));
        assert!(prompt.contains("`program`"));
        assert!(prompt.contains("`script`"));
        assert!(!prompt.contains("`exec`"));
        assert!(!prompt.contains("`task`"));
    }

    #[test]
    fn shell_python_edits_are_a_supported_path() {
        let registry = ToolRegistry::new();
        register_builtins(&registry);
        let prompt = build_system_prompt(&registry);
        assert!(
            prompt.contains("Python") && prompt.contains("heredoc"),
            "system prompt must teach shell Python edits, got:\n{prompt}"
        );
        assert!(
            prompt.contains("`write`, `edit`, and `shell`"),
            "write and edit stay alongside shell, got:\n{prompt}"
        );
        assert!(
            prompt.contains("`mode` `script`"),
            "file edits belong on shell script mode, got:\n{prompt}"
        );
        assert!(
            !prompt.contains("only for pipelines"),
            "shell must not be limited to pipelines:\n{prompt}"
        );
        assert!(
            prompt.contains("does not restore or delete workspace files"),
            "chat rewind must be documented as chat-only:\n{prompt}"
        );
        let shell = ShellTool::new();
        let description = shell.description();
        assert!(
            description.contains("quoted heredoc"),
            "shell description must allow Python heredocs, got:\n{description}"
        );
        assert!(
            description.contains("`mode` `program`"),
            "program mode stays on the shell tool, got:\n{description}"
        );
        assert!(
            !description.contains("do not use this tool to read, write, edit"),
            "shell description must not forbid file edits, got:\n{description}"
        );
        assert!(
            !description.contains("`exec`"),
            "shell description must not name a separate exec tool, got:\n{description}"
        );
    }
}
