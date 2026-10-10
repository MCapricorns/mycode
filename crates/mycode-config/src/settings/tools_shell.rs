//! Tool-runtime settings family: the platform shell behind the `shell` tool.

use serde::{Deserialize, Serialize};

use super::{AppSettings, MAX_FIELD_BYTES, bounded_text};
use crate::ConfigError;

/// Accepted `tools.shell.kind` values.
pub const VALID_SHELL_KINDS: [&str; 3] = ["pwsh", "powershell", "bash"];

/// One resolved platform shell used by the `shell` tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellSettings {
    /// Interpreter family: `pwsh`, `powershell` (Windows PowerShell 5.1), or `bash`.
    pub kind: String,
    /// Absolute path of the shell executable.
    pub program: String,
    /// `auto` after first-run detection; `user` after an explicit pick.
    #[serde(default, skip_serializing_if = "shell_source_is_auto")]
    pub source: String,
}

fn shell_source_is_auto(source: &str) -> bool {
    source.is_empty() || source == "auto"
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            kind: String::new(),
            program: String::new(),
            source: "auto".to_owned(),
        }
    }
}

/// Tool-runtime preferences persisted in settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolsSettings {
    /// Platform shell used by the `shell` tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ShellSettings>,
}

pub(super) fn tools_are_default(tools: &ToolsSettings) -> bool {
    *tools == ToolsSettings::default()
}

impl AppSettings {
    /// Validates the shell override: kind vocabulary, a nonempty executable
    /// path, and the source marker grammar.
    pub(super) fn validate_tools_shell(&self) -> Result<(), ConfigError> {
        let invalid =
            |detail: &str| ConfigError::authority_rejection().with_detail(detail.to_owned());
        if let Some(shell) = self.tools.shell.as_ref() {
            if !VALID_SHELL_KINDS.contains(&shell.kind.as_str()) {
                return Err(invalid(
                    "tools.shell.kind: must be pwsh, powershell, or bash",
                ));
            }
            let program = shell.program.trim();
            if program.is_empty() {
                return Err(invalid("tools.shell.program: set an executable path"));
            }
            bounded_text(program, MAX_FIELD_BYTES).map_err(|_| {
                invalid("tools.shell.program: too long or contains line breaks or NUL")
            })?;
            if !shell.source.is_empty() && shell.source != "auto" && shell.source != "user" {
                return Err(invalid("tools.shell.source: must be auto or user"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{AppSettings, ShellSettings, ToolsSettings};

    #[test]
    fn powershell_kind_is_stored_and_cmd_is_rejected() {
        let mut settings = AppSettings::default();
        settings.tools = ToolsSettings {
            shell: Some(ShellSettings {
                kind: "powershell".into(),
                program: r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".into(),
                source: "auto".into(),
            }),
        };
        assert!(settings.validate().is_ok());
        settings.tools.shell.as_mut().unwrap().kind = "pwsh".into();
        assert!(settings.validate().is_ok());
        settings.tools.shell.as_mut().unwrap().kind = "bash".into();
        assert!(settings.validate().is_ok());
        settings.tools.shell.as_mut().unwrap().kind = "cmd".into();
        let error = settings.validate().expect_err("cmd is not a settings kind");
        assert!(
            error.summary().contains("pwsh, powershell, or bash"),
            "{}",
            error.summary()
        );
    }
}
