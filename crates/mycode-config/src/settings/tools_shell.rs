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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolsSettings {
    /// Legacy shell override. Ignored when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ShellSettings>,
    /// OS helper for `shell` and `run_code`. Default on.
    ///
    /// Linux uses bubblewrap and macOS uses seatbelt when this is on. The
    /// helper is best-effort write confinement, not a security boundary. A
    /// missing helper fails the call instead of running unsandboxed.
    /// Windows has no helper. [`Self::effective_os_sandbox`] leaves the
    /// product path off there so shell and `run_code` still run, and the
    /// settings page says the sandbox is not supported yet. A runtime flag
    /// that forces the helper on Windows still fails closed.
    #[serde(
        default = "default_os_sandbox",
        skip_serializing_if = "os_sandbox_is_default_on"
    )]
    pub os_sandbox: bool,
}

fn default_os_sandbox() -> bool {
    true
}

fn os_sandbox_is_default_on(value: &bool) -> bool {
    *value
}

impl Default for ToolsSettings {
    fn default() -> Self {
        Self {
            shell: None,
            os_sandbox: default_os_sandbox(),
        }
    }
}

/// Bubblewrap and seatbelt exist on these OS names (`std::env::consts::OS`).
fn platform_supports_os_sandbox(os: &str) -> bool {
    matches!(os, "linux" | "macos")
}

impl ToolsSettings {
    /// Whether this build has a bubblewrap or seatbelt helper.
    #[must_use]
    pub fn os_sandbox_platform_supported() -> bool {
        platform_supports_os_sandbox(std::env::consts::OS)
    }

    /// Sandbox flag the product path should pass into tool execution.
    ///
    /// Linux and macOS follow [`Self::os_sandbox`] and fail closed when the
    /// helper is missing. Windows has no helper. The default-on setting stays
    /// stored as on, but this returns false so shell and `run_code` still
    /// run. The settings UI says the sandbox is not supported yet instead of
    /// showing a switch that looks on. Callers that set the runtime flag
    /// anyway still fail closed inside the helper planner.
    #[must_use]
    pub fn effective_os_sandbox(&self) -> bool {
        self.effective_os_sandbox_on(std::env::consts::OS)
    }

    fn effective_os_sandbox_on(&self, os: &str) -> bool {
        self.os_sandbox && platform_supports_os_sandbox(os)
    }
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
        assert!(
            parsed.tools.os_sandbox,
            "a legacy shell object that omits osSandbox keeps the default on"
        );
    }

    #[test]
    fn os_sandbox_defaults_on_and_round_trips() {
        let defaults = AppSettings::default();
        assert!(defaults.tools.os_sandbox);
        let encoded = serde_json::to_string(&defaults).expect("encode");
        assert!(
            !encoded.contains("osSandbox"),
            "default-on settings omit the flag: {encoded}"
        );
        let omitted: AppSettings = serde_json::from_str("{}").expect("empty document");
        assert!(omitted.tools.os_sandbox);
        let empty_tools: AppSettings =
            serde_json::from_str(r#"{"tools":{}}"#).expect("empty tools");
        assert!(empty_tools.tools.os_sandbox);
        let enabled: AppSettings =
            serde_json::from_str(r#"{"tools":{"osSandbox":true}}"#).expect("parse");
        assert!(enabled.tools.os_sandbox);
        assert!(enabled.validate().is_ok());
        let disabled: AppSettings =
            serde_json::from_str(r#"{"tools":{"osSandbox":false}}"#).expect("opt out");
        assert!(!disabled.tools.os_sandbox);
        assert!(disabled.validate().is_ok());
        let disabled_json = serde_json::to_string(&disabled).expect("encode opt out");
        assert!(
            disabled_json.contains("\"osSandbox\":false"),
            "{disabled_json}"
        );
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

    #[test]
    fn windows_product_path_leaves_the_default_sandbox_off() {
        let defaults = ToolsSettings::default();
        assert!(defaults.os_sandbox);
        assert!(defaults.effective_os_sandbox_on("linux"));
        assert!(defaults.effective_os_sandbox_on("macos"));
        assert!(
            !defaults.effective_os_sandbox_on("windows"),
            "default-on must not enable a helper Windows does not have"
        );
        let forced = ToolsSettings {
            os_sandbox: true,
            ..ToolsSettings::default()
        };
        assert!(
            !forced.effective_os_sandbox_on("windows"),
            "a stored true is still the product path on Windows, not a forced helper"
        );
        let opted_out = ToolsSettings {
            os_sandbox: false,
            ..ToolsSettings::default()
        };
        assert!(!opted_out.effective_os_sandbox_on("linux"));
        assert!(!opted_out.effective_os_sandbox_on("windows"));
        assert_eq!(
            ToolsSettings::os_sandbox_platform_supported(),
            cfg!(any(target_os = "linux", target_os = "macos"))
        );
    }
}
