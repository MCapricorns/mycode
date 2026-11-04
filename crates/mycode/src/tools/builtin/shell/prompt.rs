//! Model-facing copy for the one shell tool the active interpreter selects.

use std::path::Path;

use serde_json::{Value, json};

use super::detect::{DetectedShell, ShellKind};

#[must_use]
pub(crate) fn tool_description(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Pwsh => POWERSHELL7_DESCRIPTION,
        ShellKind::WindowsPowerShell => POWERSHELL51_DESCRIPTION,
        ShellKind::Bash => BASH_DESCRIPTION,
        ShellKind::Zsh => ZSH_DESCRIPTION,
        ShellKind::Sh => SH_DESCRIPTION,
        ShellKind::Cmd => CMD_DESCRIPTION,
    }
}

#[must_use]
pub(crate) fn prompt_snippet(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => POWERSHELL_SNIPPET,
        ShellKind::Bash => BASH_SNIPPET,
        ShellKind::Zsh => ZSH_SNIPPET,
        ShellKind::Sh => SH_SNIPPET,
        ShellKind::Cmd => CMD_SNIPPET,
    }
}

#[must_use]
pub(crate) fn shell_label(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Pwsh => "PowerShell 7",
        ShellKind::WindowsPowerShell => "Windows PowerShell 5.1",
        ShellKind::Bash => "bash",
        ShellKind::Zsh => "zsh",
        ShellKind::Sh => "sh",
        ShellKind::Cmd => "cmd.exe",
    }
}

#[must_use]
pub(crate) fn guidance(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Pwsh => POWERSHELL7_GUIDANCE,
        ShellKind::WindowsPowerShell => POWERSHELL51_GUIDANCE,
        ShellKind::Bash => BASH_GUIDANCE,
        ShellKind::Zsh => ZSH_GUIDANCE,
        ShellKind::Sh => SH_GUIDANCE,
        ShellKind::Cmd => CMD_GUIDANCE,
    }
}

#[must_use]
pub(crate) fn command_parameter_doc(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Pwsh => {
            "PowerShell 7 command (required for `script`, omit for `program`). `&&` and `||` work; do not send bash or cmd."
        }
        ShellKind::WindowsPowerShell => {
            "Windows PowerShell 5.1 command (required for `script`, omit for `program`). Chain with `;` and `$LASTEXITCODE`; `&&` and `||` are syntax errors."
        }
        ShellKind::Bash => {
            "Bash command passed to `bash -c` (required for `script`, omit for `program`)."
        }
        ShellKind::Zsh => {
            "Zsh command passed to `zsh -c` (required for `script`, omit for `program`)."
        }
        ShellKind::Sh => {
            "POSIX sh command passed to `sh -c` (required for `script`, omit for `program`)."
        }
        ShellKind::Cmd => {
            "cmd.exe command passed to `/d /s /c` (required for `script`, omit for `program`)."
        }
    }
}

macro_rules! mode_doc {
    ($interpreter:literal, $starts:literal) => {
        concat!(
            "`script` (default) runs `command` in ",
            $interpreter,
            ". `program` runs an executable or direct shebang wrapper with `args` and does not start ",
            $starts,
            "."
        )
    };
}

#[must_use]
pub(crate) fn mode_parameter_doc(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Pwsh => mode_doc!("PowerShell 7", "PowerShell"),
        ShellKind::WindowsPowerShell => mode_doc!("Windows PowerShell 5.1", "PowerShell"),
        ShellKind::Bash => mode_doc!("bash", "a shell"),
        ShellKind::Zsh => mode_doc!("zsh", "a shell"),
        ShellKind::Sh => mode_doc!("POSIX sh", "a shell"),
        ShellKind::Cmd => mode_doc!("cmd.exe", "a shell"),
    }
}

pub(crate) fn apply_parameter_docs(schema: &mut Value, kind: ShellKind) {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    set_description(properties.get_mut("command"), command_parameter_doc(kind));
    set_description(properties.get_mut("mode"), mode_parameter_doc(kind));
}

fn set_description(value: Option<&mut Value>, description: &str) {
    let Some(object) = value.and_then(Value::as_object_mut) else {
        return;
    };
    object.insert("description".to_owned(), json!(description));
}

#[must_use]
pub fn render_environment_block(os: &str, arch: &str, cwd: &Path, shell: &DetectedShell) -> String {
    format!(
        "<environment>\n\
os: {os} ({arch})\n\
cwd: {cwd}\n\
shell_tool: {tool}\n\
interpreter: {label} ({program})\n\
</environment>\n\n\
<shell>\n\
{guidance}\n\
</shell>",
        cwd = cwd.display(),
        tool = shell.kind.tool_name(),
        label = shell_label(shell.kind),
        program = shell.program.display(),
        guidance = guidance(shell.kind),
    )
}

macro_rules! shared_description {
    () => {
        " Each call starts a new process in the session cwd (`cd` and env assignments do not persist). \
Use for builds, tests, git, package managers, and running programs; prefer dedicated `read`, `grep`, `find`, `edit`, and `write` tools when available. \
Commands must exit on their own (no servers, watchers, or interactive prompts). `mode` `program` spawns `program` with `args` and no shell. \
Output beyond 50 KiB keeps its start and end; default timeout is 120 s (`timeout_secs` overrides)."
    };
}

const POWERSHELL7_DESCRIPTION: &str = concat!(
    "Run a PowerShell 7 (`pwsh`) command. Use PowerShell syntax (`$env:NAME`, `2>$null`, `&&`/`||`, `@'...'@`), not bash or cmd.",
    shared_description!()
);

const POWERSHELL51_DESCRIPTION: &str = concat!(
    "Run a Windows PowerShell 5.1 command. `&&` and `||` are syntax errors; chain with `;` and check `$LASTEXITCODE`. Use `$env:NAME` and `2>$null`, not bash.",
    shared_description!()
);

const BASH_DESCRIPTION: &str = concat!(
    "Run a bash command (`bash -c`) and return stdout/stderr.",
    shared_description!()
);

const ZSH_DESCRIPTION: &str = concat!(
    "Run a zsh command (`zsh -c`) and return stdout/stderr.",
    shared_description!()
);

const SH_DESCRIPTION: &str = concat!(
    "Run a POSIX sh command (`sh -c`) and return stdout/stderr.",
    shared_description!()
);

const CMD_DESCRIPTION: &str = concat!(
    "Run a cmd.exe command (`cmd.exe /d /s /c`) and return stdout/stderr.",
    shared_description!()
);

macro_rules! snippet {
    ($tool:literal, $interpreter:literal) => {
        concat!(
            $tool,
            ": mode script (default) runs one ",
            $interpreter,
            " command (`command`); mode program runs one program with `args` and no shell."
        )
    };
}

const POWERSHELL_SNIPPET: &str = snippet!("powershell", "PowerShell");
const BASH_SNIPPET: &str = snippet!("bash", "bash");
const ZSH_SNIPPET: &str = snippet!("zsh", "zsh");
const SH_SNIPPET: &str = snippet!("sh", "POSIX sh");
const CMD_SNIPPET: &str = snippet!("cmd", "cmd.exe");

const POWERSHELL7_GUIDANCE: &str = "\
The `powershell` tool runs PowerShell 7 (`pwsh`). Write PowerShell, not bash or cmd.
- Each call starts a new process in the session cwd; `cd` and `$env:` do not persist.
- `&&` and `||` are allowed. Use `$env:NAME`, `2>$null`, `Get-Command`, and `@'...'@` instead of bash syntax.";

const POWERSHELL51_GUIDANCE: &str = "\
The `powershell` tool runs Windows PowerShell 5.1 (`powershell.exe`).
- Do not use `&&` or `||`; chain with `;` and test `$LASTEXITCODE`.
- Use `$env:NAME`, `2>$null`, `Get-Command`, and `@'...'@` instead of bash syntax.";

const BASH_GUIDANCE: &str = "\
The `bash` tool runs `bash -c`. Each call starts a new process in the session cwd.";

const ZSH_GUIDANCE: &str = "\
The `zsh` tool runs `zsh -c`. Each call starts a new process in the session cwd.";

const SH_GUIDANCE: &str = "\
The `sh` tool runs `sh -c`. Each call starts a new process in the session cwd.";

const CMD_GUIDANCE: &str = "\
The `cmd` tool runs `cmd.exe /d /s /c`. Each call starts a new process in the session cwd.";

#[cfg(test)]
mod tests {
    use super::{guidance, render_environment_block};
    use crate::tools::builtin::shell::{DetectedShell, ShellKind};
    use std::path::{Path, PathBuf};

    #[test]
    fn environment_block_matches_the_interpreter() {
        let cwd = Path::new("/work");
        for (kind, program, label) in [
            (ShellKind::Pwsh, "pwsh.exe", "PowerShell 7"),
            (
                ShellKind::WindowsPowerShell,
                "powershell.exe",
                "Windows PowerShell 5.1",
            ),
            (ShellKind::Bash, "/bin/bash", "bash"),
        ] {
            let shell = DetectedShell {
                kind,
                program: PathBuf::from(program),
            };
            let block = render_environment_block("windows", "x86_64", cwd, &shell);
            assert!(block.contains(&format!("shell_tool: {}", kind.tool_name())));
            assert!(block.contains(&format!("interpreter: {label} ({program})")));
            assert!(block.contains(guidance(kind)));
        }
    }
}
