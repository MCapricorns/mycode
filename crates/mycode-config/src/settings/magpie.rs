//! magpie gateway settings.
//!
//! magpie (`https://usemagpie.ai`) runs a local OpenAI-compatible gateway and
//! writes this section through its agent plugin. The section is the only
//! place the app reads the gateway address, its key, and the model magpie
//! picked for MCode; model metadata comes from the gateway's `/v1/models`.

use serde::{Deserialize, Serialize};

use super::{
    AppSettings, MAX_FIELD_BYTES, MAX_HIDDEN_MODELS_PER_PROVIDER, bounded_text, is_https_url,
    is_local_http_url, is_portable_model_id,
};
use crate::ConfigError;

/// Provider id the gateway is exposed under in the model picker.
pub const MAGPIE_PROVIDER_ID: &str = "magpie";

/// The local magpie gateway binding.
///
/// `enabled`, `baseUrl`, `apiKey`, and `model` are written by magpie's
/// `mycode.agent.js`; the app never edits them from the settings UI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct MagpieSettings {
    /// Whether the gateway is offered as a provider.
    pub enabled: bool,
    /// OpenAI-compatible base URL, ending in `/v1`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub base_url: String,
    /// Bearer key magpie issued to the app (`magpie-mycode`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    /// Model magpie picked, as the gateway names it (`provider/model`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// Models kept out of the picker while the gateway still lists them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_models: Vec<String>,
}

impl MagpieSettings {
    /// Whether the section is absent from `settings.json` as written.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    /// The base URL with any trailing slash removed.
    #[must_use]
    pub fn endpoint_base(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    /// Whether a usable binding was written.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.enabled && !self.endpoint_base().is_empty() && !self.model.is_empty()
    }
}

impl AppSettings {
    /// Validates the magpie binding: address grammar, key shape, and the
    /// selected model id.
    pub(super) fn validate_magpie(&self) -> Result<(), ConfigError> {
        let invalid =
            |detail: &str| ConfigError::authority_rejection().with_detail(detail.to_owned());
        let magpie = &self.magpie;
        if magpie.base_url.is_empty() {
            if magpie.enabled {
                return Err(invalid(
                    "magpie.baseUrl: required when the gateway is enabled",
                ));
            }
            return Ok(());
        }
        if !is_local_http_url(&magpie.base_url) && !is_https_url(&magpie.base_url) {
            return Err(invalid(
                "magpie.baseUrl: must be an https:// URL or a loopback http:// gateway",
            ));
        }
        bounded_text(&magpie.api_key, MAX_FIELD_BYTES)
            .map_err(|_| invalid("magpie.apiKey: too long or contains line breaks or NUL"))?;
        if magpie.enabled && magpie.api_key.is_empty() {
            return Err(invalid(
                "magpie.apiKey: required when the gateway is enabled",
            ));
        }
        if !magpie.model.is_empty() && !is_portable_model_id(&magpie.model) {
            return Err(invalid(
                "magpie.model: must be a provider/model id without line breaks or NUL",
            ));
        }
        if magpie.enabled && magpie.model.is_empty() {
            return Err(invalid(
                "magpie.model: required when the gateway is enabled",
            ));
        }
        if magpie.hidden_models.len() > MAX_HIDDEN_MODELS_PER_PROVIDER {
            return Err(invalid(&format!(
                "magpie.hiddenModels: at most {MAX_HIDDEN_MODELS_PER_PROVIDER} entries"
            )));
        }
        for model in &magpie.hidden_models {
            bounded_text(model, MAX_FIELD_BYTES).map_err(|_| {
                invalid("magpie.hiddenModels: an id is too long or contains line breaks or NUL")
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{AppSettings, MagpieSettings, is_local_http_url};

    fn settings(magpie: MagpieSettings) -> AppSettings {
        AppSettings {
            magpie,
            ..AppSettings::default()
        }
    }

    fn active() -> MagpieSettings {
        MagpieSettings {
            enabled: true,
            base_url: "http://127.0.0.1:3425/v1".to_owned(),
            api_key: "magpie-mycode".to_owned(),
            model: "deepseek/deepseek-chat".to_owned(),
            hidden_models: Vec::new(),
        }
    }

    #[test]
    fn loopback_http_gateways_are_accepted_and_public_http_is_not() {
        assert!(is_local_http_url("http://127.0.0.1:3425/v1"));
        assert!(is_local_http_url("http://localhost:3425"));
        assert!(is_local_http_url("http://[::1]:3425/v1"));
        assert!(!is_local_http_url("http://192.168.1.5:3425"));
        assert!(!is_local_http_url("https://127.0.0.1:3425"));
        assert!(!is_local_http_url("http://127.0.0.1:not-a-port"));
        assert!(settings(active()).validate_magpie().is_ok());
    }

    #[test]
    fn an_enabled_binding_needs_an_address_key_and_model() {
        let mut magpie = active();
        magpie.api_key.clear();
        assert!(
            settings(magpie)
                .validate_magpie()
                .expect_err("empty key is rejected")
                .summary()
                .contains("apiKey")
        );
        let mut magpie = active();
        magpie.model.clear();
        assert!(
            settings(magpie)
                .validate_magpie()
                .expect_err("empty model is rejected")
                .summary()
                .contains("model")
        );
    }

    #[test]
    fn a_disabled_binding_with_no_fields_is_valid() {
        assert!(
            settings(MagpieSettings::default())
                .validate_magpie()
                .is_ok()
        );
    }
}
