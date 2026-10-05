//! Builds the default system prompt from the active tool registry.

use std::fmt::Write;

use mycode_tools::ToolRegistry;

const IDENTITY: &str =
    "You are MYCode Agent. Complete the task with the tools listed below. Do not invent tools.";

/// Tool-use contract. Only names tools this process can actually call.
const TOOL_CALLING: &str = "\
<tool_calling>
- Independent calls in one response run together. Do not wait between them.
- Read existing content before changing it.
- Prefer `read`, `write`, `edit`, `find`, and `grep` for files and search.
- Use `shell` with `mode` `program` for one kernel-loadable program and an explicit argument vector. That path does not start a shell.
- Use `shell` with `mode` `script` only for pipelines, redirection, expansion, or a compound script. Never use it to talk to the user.
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
    use mycode_tools::{ToolRegistry, register_builtins};

    #[test]
    fn prompt_names_shell_modes_and_not_exec_or_task() {
        let registry = ToolRegistry::new();
        register_builtins(&registry);
        let prompt = super::build_system_prompt(&registry);
        assert!(prompt.contains("`shell`"));
        assert!(prompt.contains("`program`"));
        assert!(prompt.contains("`script`"));
        assert!(!prompt.contains("`exec`"));
        assert!(!prompt.contains("`task`"));
    }
}
