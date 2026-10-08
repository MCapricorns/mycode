//! Provider-endpoint settings family.

use serde::{Deserialize, Serialize};

use super::{AppSettings, MAX_FIELD_BYTES, bounded_text, is_https_url, is_portable_id};
use crate::ConfigError;

/// Maximum provider entries.
pub const MAX_PROVIDERS: usize = 64;
/// Maximum models listed by one provider entry.
pub const MAX_MODELS_PER_PROVIDER: usize = 128;

/// One configured first-party provider endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderSettings {
    /// Unique provider identity (lowercase portable).
    pub id: String,
    /// Wire protocol: `anthropic-messages`, `openai-completions`, or
    /// `openai-responses`.
    pub kind: String,
    /// Base URL for API calls (`https://` only).
    pub base_url: String,
    /// Models exposed by this provider; the first is the default.
    pub models: Vec<String>,
    /// Enabled in the model picker.
    pub enabled: bool,
    /// Context window override in tokens; absent keeps the catalog value or
    /// the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_limit: Option<u64>,
    /// Max output tokens override; absent keeps the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
}

/// Wire protocols accepted by [`ProviderSettings::kind`].
///
/// Vendor differences (DeepSeek, Kimi, Z.AI GLM, custom gateways, …) are data:
/// a base URL plus credentials over one of these protocols. Adding a vendor
/// never adds an adapter family.
pub const VALID_PROVIDER_KINDS: [&str; 3] = [
    "anthropic-messages",
    "openai-completions",
    "openai-responses",
];

impl AppSettings {
    /// Validates the provider family: entry bounds, id grammar, kind
    /// vocabulary, https base URLs, model lists, and duplicate ids.
    pub(super) fn validate_providers(&self) -> Result<(), ConfigError> {
        let invalid =
            |detail: &str| ConfigError::authority_rejection().with_detail(detail.to_owned());
        if self.providers.len() > MAX_PROVIDERS {
            return Err(invalid("providers: too many entries"));
        }
        for (index, provider) in self.providers.iter().enumerate() {
            let field = format!("providers[{index}]");
            if !is_portable_id(&provider.id) {
                return Err(invalid(&format!(
                    "{field}.id: must be letters, digits, dash, dot, or underscore"
                )));
            }
            if !VALID_PROVIDER_KINDS.contains(&provider.kind.as_str()) {
                return Err(invalid(&format!(
                    "{field}.kind: must be one of anthropic-messages, openai-completions, openai-responses"
                )));
            }
            if !is_https_url(&provider.base_url) {
                return Err(invalid(&format!(
                    "{field}.baseUrl: must be an https:// URL"
                )));
            }
            if provider.models.is_empty() || provider.models.len() > MAX_MODELS_PER_PROVIDER {
                return Err(invalid(&format!(
                    "{field}.models: list at least one model id (at most {MAX_MODELS_PER_PROVIDER})"
                )));
            }
            if self.providers[..index].iter().any(|p| p.id == provider.id) {
                return Err(invalid(&format!(
                    "{field}.id: duplicates an earlier provider id"
                )));
            }
            for (model_index, model) in provider.models.iter().enumerate() {
                bounded_text(model, MAX_FIELD_BYTES).map_err(|_| {
                    invalid(&format!(
                        "{field}.models[{model_index}]: too long or contains control characters"
                    ))
                })?;
            }
        }
        Ok(())
    }
}
