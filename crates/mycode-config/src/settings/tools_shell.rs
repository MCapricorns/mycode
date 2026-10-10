//! Legacy `tools.shell` document shape.
//!
//! The shell is chosen automatically. This module only keeps old settings
//! files readable.

use serde::{Deserialize, Serialize};

use super::AppSettings;
use crate::ConfigError;

/// Legacy `tools.shell` object.
///
/// Startup ignores this value. It stays deserializable so an older
/// `settings.json` still loads, then the load path clears it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellSettings {
    /// Previously `pwsh`, `powershell`, or `bash`. Not consulted.
    #[serde(default)]
    pub kind: String,
    /// Previously an executable path. Not consulted.
    #[serde(default)]
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
///
/// `shell` is accepted and ignored. Selection is automatic.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolsSettings {
    /// Legacy shell override. Ignored when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ShellSettings>,
}

pub(super) fn tools_are_default(tools: &ToolsSettings) -> bool {
    *tools == ToolsSettings::default()
}

impl AppSettings {
    /// `tools.shell` is legacy and ignored, including kinds that used to be
    /// rejected. A present value must not fail the rest of the document.
    pub(super) fn validate_tools_shell(&self) -> Result<(), ConfigError> {
        let _ = self.tools.shell;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{AppSettings, ShellSettings, ToolsSettings};

    #[test]
    fn stored_shell_of_any_kind_still_validates() {
        let mut settings = AppSettings::default();
        settings.tools = ToolsSettings {
            shell: Some(ShellSettings {
                kind: "cmd".into(),
                program: r"C:\Windows\System32\cmd.exe".into(),
                source: "user".into(),
            }),
        };
        assert!(settings.validate().is_ok());
        settings.tools.shell.as_mut().unwrap().kind = "bash".into();
        settings.tools.shell.as_mut().unwrap().program.clear();
        assert!(settings.validate().is_ok());
        let parsed: AppSettings =
            serde_json::from_str(r#"{"tools":{"shell":{"kind":"pwsh","program":"C:\\pwsh.exe"}}}"#)
                .expect("legacy shell object");
        assert_eq!(
            parsed.tools.shell.as_ref().map(|shell| shell.kind.as_str()),
            Some("pwsh")
        );
    }
}
