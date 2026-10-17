//! Web-search backend settings family.

use serde::{Deserialize, Serialize};

use super::{AppSettings, is_https_url, is_portable_id};
use crate::ConfigError;

/// Maximum web search backends.
pub const MAX_WEB_BACKENDS: usize = 16;

/// Wire families accepted by [`WebBackendSettings::kind`]. `custom` stays
/// valid so a settings file written before the vendor-only settings page
/// still loads; the UI no longer adds or edits one.
const VALID_WEB_KINDS: [&str; 4] = ["querit", "anysearch", "exa", "custom"];

/// One configured search backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebBackendSettings {
    /// Unique backend identity (lowercase portable).
    pub id: String,
    /// Backend family: `querit`, `anysearch`, `exa`, or `custom`.
    pub kind: String,
    /// HTTPS API endpoint.
    pub endpoint: String,
    /// Enabled.
    pub enabled: bool,
}

/// The environment variable that can hold a builtin backend's API key.
///
/// `None` for a `custom` row, which only takes a vault key. Callers check
/// the variable before the vault so a machine already exporting it (the pi
/// agent's querit plugin reads `QUERIT_API_KEY` the same way) keeps working.
#[must_use]
pub fn web_key_env(kind: &str) -> Option<&'static str> {
    match kind {
        "querit" => Some("QUERIT_API_KEY"),
        "anysearch" => Some("ANYSEARCH_API_KEY"),
        "exa" => Some("EXA_API_KEY"),
        _ => None,
    }
}

/// The vendor search backends the settings page lists.
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
        WebBackendSettings {
            id: "exa".to_owned(),
            kind: "exa".to_owned(),
            endpoint: "https://api.exa.ai".to_owned(),
            enabled: false,
        },
    ]
}

/// The vendor rows overlaid with configured backends: a stored row replaces
/// its builtin slot by id, and an id the vendor list no longer offers (a
/// `custom` row from an older file) is kept so nothing is silently dropped.
#[must_use]
pub fn merged_web_backends(configured: &[WebBackendSettings]) -> Vec<WebBackendSettings> {
    let mut backends = builtin_web_backends();
    for backend in configured {
        if let Some(slot) = backends.iter_mut().find(|item| item.id == backend.id) {
            *slot = backend.clone();
        } else {
            backends.push(backend.clone());
        }
    }
    backends
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
                    "{field}.id: must be 1-128 lowercase letters, digits, dash, dot, or underscore"
                )));
            }
            if !VALID_WEB_KINDS.contains(&backend.kind.as_str()) {
                return Err(invalid(&format!(
                    "{field}.kind: must be querit, anysearch, exa, or custom"
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

#[cfg(test)]
mod tests {
    use super::{AppSettings, WebBackendSettings, builtin_web_backends, web_key_env};

    fn backend(id: &str, kind: &str) -> WebBackendSettings {
        WebBackendSettings {
            id: id.to_owned(),
            kind: kind.to_owned(),
            endpoint: "https://api.example.com".to_owned(),
            enabled: false,
        }
    }

    fn settings(backends: Vec<WebBackendSettings>) -> AppSettings {
        AppSettings {
            web: super::WebSettings { backends },
            ..AppSettings::default()
        }
    }

    #[test]
    fn builtin_list_is_the_three_vendors() {
        let builtins = builtin_web_backends();
        let ids: Vec<_> = builtins.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["querit", "anysearch", "exa"]);
        for backend in &builtins {
            assert_eq!(backend.id, backend.kind);
            assert!(!backend.enabled);
            assert!(web_key_env(&backend.kind).is_some());
        }
    }

    #[test]
    fn key_env_maps_each_vendor_and_no_custom() {
        assert_eq!(web_key_env("querit"), Some("QUERIT_API_KEY"));
        assert_eq!(web_key_env("anysearch"), Some("ANYSEARCH_API_KEY"));
        assert_eq!(web_key_env("exa"), Some("EXA_API_KEY"));
        assert_eq!(web_key_env("custom"), None);
    }

    #[test]
    fn validation_accepts_exa_and_legacy_custom_but_not_new_kinds() {
        assert!(settings(vec![backend("exa", "exa")]).validate_web().is_ok());
        // A file written before the vendor-only page still loads.
        assert!(
            settings(vec![backend("mine", "custom")])
                .validate_web()
                .is_ok()
        );
        let error = settings(vec![backend("mine", "tavily")])
            .validate_web()
            .expect_err("an unknown kind is rejected");
        assert!(error.summary().contains("kind"), "{}", error.summary());
    }

    #[test]
    fn merged_list_overlays_configured_and_keeps_leftovers() {
        let mut stored_exa = backend("exa", "exa");
        stored_exa.enabled = true;
        let merged = super::merged_web_backends(&[stored_exa, backend("mine", "custom")]);
        let ids: Vec<_> = merged.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["querit", "anysearch", "exa", "mine"]);
        // The stored row replaced the builtin slot instead of appending.
        assert!(merged[2].enabled);
        assert_eq!(merged[3].kind, "custom");
    }

    #[test]
    fn validation_keeps_the_single_enabled_rule() {
        let mut a = backend("querit", "querit");
        a.enabled = true;
        let mut b = backend("exa", "exa");
        b.enabled = true;
        let error = settings(vec![a, b])
            .validate_web()
            .expect_err("two enabled backends are rejected");
        assert!(error.summary().contains("at most one"));
    }
}
