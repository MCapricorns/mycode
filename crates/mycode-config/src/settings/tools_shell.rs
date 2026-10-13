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
/// `shell` and `osSandbox` are accepted and ignored. Selection is automatic
/// and there is no OS sandbox.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolsSettings {
    /// Legacy shell override. Ignored when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ShellSettings>,
    /// Legacy OS-sandbox flag. The feature is gone; the value is accepted so
    /// an older `settings.json` still loads, then the load path clears it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub os_sandbox: bool,
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
        let mut settings = AppSettings {
            tools: ToolsSettings {
                shell: Some(ShellSettings {
                    kind: "cmd".into(),
                    program: r"C:\Windows\System32\cmd.exe".into(),
                    source: "user".into(),
                }),
                os_sandbox: false,
            },
            ..AppSettings::default()
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

    #[test]
    fn legacy_os_sandbox_flag_is_accepted_and_never_written() {
        let defaults = AppSettings::default();
        assert!(!defaults.tools.os_sandbox);
        let encoded = serde_json::to_string(&defaults).expect("encode");
        assert!(
            !encoded.contains("osSandbox"),
            "the removed flag is never written: {encoded}"
        );
        let enabled: AppSettings =
            serde_json::from_str(r#"{"tools":{"osSandbox":true}}"#).expect("parse");
        assert!(enabled.tools.os_sandbox);
        assert!(enabled.validate().is_ok());
        let disabled: AppSettings =
            serde_json::from_str(r#"{"tools":{"osSandbox":false}}"#).expect("parse");
        assert!(!disabled.tools.os_sandbox);
        assert!(disabled.validate().is_ok());
        let both: AppSettings = serde_json::from_str(
            r#"{"tools":{"osSandbox":true,"shell":{"kind":"bash","program":"/bin/bash"}}}"#,
        )
        .expect("legacy shell plus sandbox");
        assert!(both.tools.os_sandbox);
        assert_eq!(
            both.tools.shell.as_ref().map(|shell| shell.kind.as_str()),
            Some("bash")
        );
        assert!(both.validate().is_ok());
    }
}
