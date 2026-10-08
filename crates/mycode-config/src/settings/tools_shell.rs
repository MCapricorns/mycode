//! Tool-runtime settings family: the platform shell behind the `shell` tool.

use serde::{Deserialize, Serialize};

use super::{AppSettings, MAX_FIELD_BYTES, bounded_text};
use crate::ConfigError;

/// Accepted `tools.shell.kind` values.
pub const VALID_SHELL_KINDS: [&str; 2] = ["pwsh", "bash"];

/// Kinds older builds accepted. A stored entry with one of them is dropped on
/// load so first-run detection re-runs with this build's candidates.
pub const RETIRED_SHELL_KINDS: [&str; 2] = ["powershell", "cmd"];

/// One resolved platform shell used by the `shell` tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellSettings {
    /// Interpreter family: `pwsh` or `bash`.
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

/// Drops a stored shell whose kind this build retired (`powershell`, `cmd`).
///
/// Returns whether the document changed, so the caller can persist the
/// migration and detection can refill the entry.
pub(super) fn retire_unsupported_shell(settings: &mut AppSettings) -> bool {
    let retired = settings
        .tools
        .shell
        .as_ref()
        .is_some_and(|shell| RETIRED_SHELL_KINDS.contains(&shell.kind.as_str()));
    if retired {
        settings.tools.shell = None;
    }
    retired
}

impl AppSettings {
    /// Validates the shell override: kind vocabulary, a nonempty executable
    /// path, and the source marker grammar.
    pub(super) fn validate_tools_shell(&self) -> Result<(), ConfigError> {
        let invalid =
            |detail: &str| ConfigError::authority_rejection().with_detail(detail.to_owned());
        if let Some(shell) = self.tools.shell.as_ref() {
            if !VALID_SHELL_KINDS.contains(&shell.kind.as_str()) {
                return Err(invalid("tools.shell.kind: must be pwsh or bash"));
            }
            let program = shell.program.trim();
            if program.is_empty() {
                return Err(invalid("tools.shell.program: set an executable path"));
            }
            bounded_text(program, MAX_FIELD_BYTES).map_err(|_| {
                invalid("tools.shell.program: too long or contains control characters")
            })?;
            if !shell.source.is_empty() && shell.source != "auto" && shell.source != "user" {
                return Err(invalid("tools.shell.source: must be auto or user"));
            }
        }
        Ok(())
    }
}
