//! Web-search backend settings family.

use serde::{Deserialize, Serialize};

use super::{AppSettings, is_https_url, is_portable_id};
use crate::ConfigError;

/// Maximum web search backends.
pub const MAX_WEB_BACKENDS: usize = 16;

/// Wire families accepted by [`WebBackendSettings::kind`].
pub const VALID_WEB_KINDS: [&str; 3] = ["querit", "anysearch", "custom"];

/// One configured search backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebBackendSettings {
    /// Unique backend identity (lowercase portable).
    pub id: String,
    /// Backend family: `querit`, `anysearch`, or `custom`.
    pub kind: String,
    /// HTTPS API endpoint.
    pub endpoint: String,
    /// Enabled.
    pub enabled: bool,
}

/// Built-in search backends the settings page can add in one click.
///
/// Keys stay in the environment or the vault (`web-<id>`), never here.
#[must_use]
pub fn builtin_web_backends() -> Vec<WebBackendSettings> {
    vec![
        WebBackendSettings {
            id: "querit".to_owned(),
            kind: "querit".to_owned(),
            endpoint: "https://api.querit.ai".to_owned(),
            enabled: false,
        },
        WebBackendSettings {
            id: "anysearch".to_owned(),
            kind: "anysearch".to_owned(),
            endpoint: "https://api.anysearch.com".to_owned(),
            enabled: false,
        },
    ]
}

/// Web search settings: many vendor backends, at most one enabled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSettings {
    /// Configured backends.
    pub backends: Vec<WebBackendSettings>,
}

impl Default for WebSettings {
    fn default() -> Self {
        Self {
            backends: builtin_web_backends(),
        }
    }
}

impl AppSettings {
    /// Validates the web-search family: entry bounds, id grammar, kind
    /// vocabulary, https endpoints, duplicate ids, and the single-active rule.
    pub(super) fn validate_web(&self) -> Result<(), ConfigError> {
        let invalid =
            |detail: &str| ConfigError::authority_rejection().with_detail(detail.to_owned());
        if self.web.backends.len() > MAX_WEB_BACKENDS {
            return Err(invalid("web.backends: too many entries"));
        }
        for (index, backend) in self.web.backends.iter().enumerate() {
            let field = format!("web.backends[{index}]");
            if !is_portable_id(&backend.id) {
                return Err(invalid(&format!(
                    "{field}.id: must be letters, digits, dash, dot, or underscore"
                )));
            }
            if !VALID_WEB_KINDS.contains(&backend.kind.as_str()) {
                return Err(invalid(&format!(
                    "{field}.kind: must be querit, anysearch, or custom"
                )));
            }
            if !is_https_url(&backend.endpoint) {
                return Err(invalid(&format!(
                    "{field}.endpoint: must be an https:// URL"
                )));
            }
            if self.web.backends[..index]
                .iter()
                .any(|b| b.id == backend.id)
            {
                return Err(invalid(&format!(
                    "{field}.id: duplicates an earlier backend id"
                )));
            }
        }
        if self.web.backends.iter().filter(|b| b.enabled).count() > 1 {
            return Err(invalid("web.backends: at most one backend may be enabled"));
        }
        Ok(())
    }
}
