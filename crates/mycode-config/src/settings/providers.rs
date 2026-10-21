//! Runtime endpoint row handed to the provider client.
//!
//! MCode no longer stores provider rows in `settings.json`: magpie's gateway
//! is the only model source, and the app builds one [`ProviderSettings`] from
//! `AppSettings::magpie` plus the gateway's published model list.

use serde::{Deserialize, Serialize};

use super::MAGPIE_PROVIDER_ID;

/// Maximum models listed by one provider entry.
pub const MAX_MODELS_PER_PROVIDER: usize = 128;
/// Maximum hidden model ids on one provider entry.
pub const MAX_HIDDEN_MODELS_PER_PROVIDER: usize = 2048;

/// One resolved endpoint row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderSettings {
    /// Unique provider identity (lowercase portable).
    pub id: String,
    /// Wire protocol: `anthropic-messages`, `openai-completions`, or
    /// `openai-responses`.
    pub kind: String,
    /// Base URL for API calls (`https://`, or loopback `http://` for magpie).
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
    /// Model ids kept out of the chat model picker. The settings page still
    /// lists them so they can be shown again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_models: Vec<String>,
}

impl ProviderSettings {
    /// Whether the chat model picker offers `model`.
    #[must_use]
    pub fn shows_model(&self, model: &str) -> bool {
        !self.hidden_models.iter().any(|hidden| hidden == model)
    }

    /// The one row every turn runs on: magpie's gateway.
    ///
    /// `models` is the gateway's published list, in the order it sent them.
    #[must_use]
    pub fn magpie(base_url: &str, models: Vec<String>, hidden: Vec<String>) -> Self {
        Self {
            id: MAGPIE_PROVIDER_ID.to_owned(),
            kind: "openai-completions".to_owned(),
            base_url: base_url.to_owned(),
            models,
            enabled: true,
            context_limit: None,
            max_output: None,
            hidden_models: hidden,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderSettings;

    #[test]
    fn the_gateway_row_is_the_only_provider() {
        let row = ProviderSettings::magpie(
            "http://127.0.0.1:3425/v1",
            vec!["deepseek/deepseek-chat".to_owned()],
            Vec::new(),
        );
        assert_eq!(row.id, "magpie");
        assert_eq!(row.kind, "openai-completions");
        assert!(row.enabled);
        assert!(row.shows_model("deepseek/deepseek-chat"));
    }
}
