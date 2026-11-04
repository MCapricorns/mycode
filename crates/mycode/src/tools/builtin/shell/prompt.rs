//! Model-facing copy for the one shell tool the active interpreter selects.

use std::fmt::Write as _;
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

/// The `<environment>` block: OS, cwd, and the shell tool when the run has one.
#[must_use]
pub fn render_environment_block(
    os: &str,
    arch: &str,
    cwd: &Path,
    shell: Option<&DetectedShell>,
) -> String {
    let mut block = format!("<environment>\nos: {os} ({arch})\ncwd: {}\n", cwd.display());
    if let Some(shell) = shell {
        let _ = writeln!(
            block,
            "shell_tool: {}\ninterpreter: {} ({})",
            shell.kind.tool_name(),
            shell_label(shell.kind),
            shell.program.display()
        );
    }
    block.push_str("</environment>");
    block
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

#[cfg(test)]
mod tests {
    use super::render_environment_block;
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
            let block = render_environment_block("windows", "x86_64", cwd, Some(&shell));
            assert!(block.contains(&format!("shell_tool: {}", kind.tool_name())));
            assert!(block.contains(&format!("interpreter: {label} ({program})")));
        }
        let block = render_environment_block("windows", "x86_64", cwd, None);
        assert!(!block.contains("shell_tool"), "{block}");
    }
}
