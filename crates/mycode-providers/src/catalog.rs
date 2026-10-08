//! Cloud-synced provider and model catalog.
//!
//! The catalog mirrors pi's model-data strategy: a normalized snapshot
//! generated from models.dev is vendored into the binary as the offline
//! baseline, a cached copy lives in the owned home, and a background
//! refresh re-downloads the cloud document with conditional requests.
//! Provider presets and model discovery in the desktop UI read this
//! catalog, so new vendors and models appear without an app update.
use serde::{Deserialize, Serialize};

pub mod modelsdev;
pub mod store;

pub use store::{
    CachedCatalog, DEFAULT_MAX_AGE_SECS, RefreshOutcome, bundled, current, current_with_repair,
    http_client, load_cache, refresh,
};

/// Wire protocol: Anthropic Messages.
pub const KIND_ANTHROPIC_MESSAGES: &str = "anthropic-messages";
/// Wire protocol: OpenAI Chat Completions.
pub const KIND_OPENAI_COMPLETIONS: &str = "openai-completions";
/// Wire protocol: OpenAI Responses.
pub const KIND_OPENAI_RESPONSES: &str = "openai-responses";

/// Auth: an OAuth device-code sign-in (GitHub Copilot / Codex).
pub const AUTH_DEVICE_CODE: &str = "device-code";
/// Auth: subscription OAuth plus an optional pasted API key (xAI).
pub const AUTH_OAUTH: &str = "oauth";

/// Whether the settings preset should offer a device-flow sign-in button.
#[must_use]
pub fn uses_oauth_login(auth: &str) -> bool {
    auth == AUTH_DEVICE_CODE || auth == AUTH_OAUTH
}

/// Upper bound for provider entries in one catalog.
pub const MAX_PROVIDERS: usize = 1024;
/// Upper bound for model entries in one provider.
pub const MAX_MODELS_PER_PROVIDER: usize = 512;
/// Upper bound for one catalog string field.
pub const MAX_STRING_BYTES: usize = 8 * 1024;

/// One model preset in the catalog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CatalogModel {
    /// Model id as sent to the provider.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Supports reasoning output.
    pub reasoning: bool,
    /// models.dev `reasoning_options` includes a toggle (on/off).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reasoning_toggle: bool,
    /// models.dev effort values, already lowercased.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<String>,
    /// Supports tool calling.
    pub tool_call: bool,
    /// Supports image attachments.
    pub attachment: bool,
    /// Advertised context window in tokens; 0 when unknown.
    pub context: u64,
    /// Advertised output limit in tokens; 0 when unknown.
    pub output: u64,
    /// Input cost per million tokens, when published.
    pub cost_in: Option<f64>,
    /// Output cost per million tokens, when published.
    pub cost_out: Option<f64>,
}

impl CatalogModel {
    /// Thinking choices advertised for this model.
    ///
    /// Only levels models.dev published are offered. A toggle adds Off and On.
    /// An effort list is kept as published (`none` is shown as Off). A model
    /// that merely sets `reasoning: true` does not grow a guessed ladder;
    /// the composer offers Default and leaves the provider's own setting.
    #[must_use]
    pub fn reasoning_levels(&self) -> Vec<String> {
        if !self.reasoning && self.reasoning_efforts.is_empty() && !self.reasoning_toggle {
            return Vec::new();
        }
        let mut levels = vec!["default".to_owned()];
        if self.reasoning_toggle {
            levels.push("off".to_owned());
            levels.push("on".to_owned());
        }
        for effort in &self.reasoning_efforts {
            let key = match effort.as_str() {
                "none" => "off",
                "default" => continue,
                other => other,
            };
            if !levels.iter().any(|level| level == key) {
                levels.push(key.to_owned());
            }
        }
        levels
    }
}

/// One provider preset: endpoint data plus its model list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CatalogProvider {
    /// Stable provider id (models.dev spelling).
    pub id: String,
    /// Display name.
    pub name: String,
    /// MYCode wire protocol for this endpoint.
    pub kind: String,
    /// API base URL.
    pub base_url: String,
    /// Documentation URL, when published.
    pub doc: Option<String>,
    /// Credential mode: `""` (default) pastes an API key; `device-code`
    /// signs in with an OAuth device flow.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub auth: String,
    /// Model presets, sorted by id.
    pub models: Vec<CatalogModel>,
}

impl Default for CatalogProvider {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            kind: KIND_OPENAI_COMPLETIONS.to_owned(),
            base_url: String::new(),
            doc: None,
            auth: String::new(),
            models: Vec::new(),
        }
    }
}

/// The complete provider catalog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogDocument {
    /// Providers, sorted by id.
    #[serde(default)]
    pub providers: Vec<CatalogProvider>,
}

impl CatalogDocument {
    /// Looks one provider up by id.
    #[must_use]
    pub fn provider(&self, id: &str) -> Option<&CatalogProvider> {
        self.providers.iter().find(|provider| provider.id == id)
    }

    /// Looks one model up by provider and model id.
    #[must_use]
    pub fn model(&self, provider_id: &str, model_id: &str) -> Option<&CatalogModel> {
        self.provider(provider_id)?
            .models
            .iter()
            .find(|model| model.id == model_id)
    }

    /// Resolves a model for a configured endpoint.
    ///
    /// The catalog provider id wins, then an exact base URL. When neither
    /// names a catalog provider, the model id is matched across the whole
    /// catalog. Providers are sorted by id. A row that publishes a reasoning
    /// toggle or effort list wins over an earlier row that only repeats the
    /// id, so a custom GLM gateway still picks up Off/On and the output cap.
    /// A catalog provider that simply does not list the model does not borrow
    /// another vendor's row.
    #[must_use]
    pub fn model_for_endpoint(
        &self,
        provider_id: &str,
        base_url: Option<&str>,
        model_id: &str,
    ) -> Option<&CatalogModel> {
        if let Some(provider) = self.provider(provider_id).or_else(|| {
            let base_url = base_url.filter(|url| !url.is_empty())?;
            self.providers.iter().find(|item| item.base_url == base_url)
        }) {
            return provider.models.iter().find(|model| model.id == model_id);
        }
        self.model_by_id(model_id)
    }

    fn model_by_id(&self, model_id: &str) -> Option<&CatalogModel> {
        let mut with_output = None;
        let mut any = None;
        for provider in &self.providers {
            let Some(model) = provider.models.iter().find(|model| model.id == model_id) else {
                continue;
            };
            if model.reasoning_toggle || !model.reasoning_efforts.is_empty() {
                return Some(model);
            }
            if with_output.is_none() && model.output > 0 {
                with_output = Some(model);
            }
            if any.is_none() {
                any = Some(model);
            }
        }
        with_output.or(any)
    }

    /// Returns a display name for a provider id, even when absent.
    #[must_use]
    pub fn display_name(&self, id: &str) -> String {
        self.provider(id)
            .map(|provider| provider.name.clone())
            .unwrap_or_else(|| id.to_owned())
    }
}

/// Validates one catalog string field: bounded UTF-8 without control noise.
pub(crate) fn clean_text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_STRING_BYTES
        || trimmed.chars().any(char::is_control)
    {
        return None;
    }
    Some(trimmed.to_owned())
}

/// Accepts a provider id when it is a lowercase portable spelling.
pub(crate) fn valid_provider_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

/// Normalizes a parsed catalog: drops invalid entries, bounds model lists,
/// and sorts providers and models by id.
pub(crate) fn normalize(mut providers: Vec<CatalogProvider>) -> CatalogDocument {
    providers.retain(|provider| {
        valid_provider_id(&provider.id)
            && clean_text(&provider.name).is_some()
            && clean_text(&provider.base_url).is_some()
            && matches!(
                provider.kind.as_str(),
                KIND_ANTHROPIC_MESSAGES | KIND_OPENAI_COMPLETIONS | KIND_OPENAI_RESPONSES
            )
            && !provider.models.is_empty()
    });
    for provider in &mut providers {
        if provider.models.len() > MAX_MODELS_PER_PROVIDER {
            provider.models.truncate(MAX_MODELS_PER_PROVIDER);
        }
        for model in &mut provider.models {
            if clean_text(&model.name).is_none() {
                model.name = model.id.clone();
            }
        }
        provider.models.sort_by(|a, b| a.id.cmp(&b.id));
        provider.models.dedup_by(|a, b| a.id == b.id);
    }
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    providers.dedup_by(|a, b| a.id == b.id);
    CatalogDocument { providers }
}

/// Parses the vendored normalized snapshot into a catalog document.
///
/// The snapshot is generated by `scripts/generate_catalog.py` from models.dev
/// and compiled into the binary; a decode failure yields an empty catalog
/// rather than a panic, and the unit tests keep that failure impossible.
pub fn parse_snapshot(bytes: &[u8]) -> CatalogDocument {
    serde_json::from_slice::<CatalogDocument>(bytes)
        .map(|document| attach_subscription_presets(normalize(document.providers)))
        .unwrap_or_default()
}

pub(crate) fn attach_subscription_presets(mut document: CatalogDocument) -> CatalogDocument {
    if let Some(xai) = document
        .providers
        .iter_mut()
        .find(|provider| provider.id == "xai")
    {
        xai.auth = AUTH_OAUTH.to_owned();
    }
    if document.provider("openai-codex").is_none() {
        document.providers.push(openai_codex_preset());
        document.providers.sort_by(|a, b| a.id.cmp(&b.id));
    }
    document
}

fn openai_codex_preset() -> CatalogProvider {
    CatalogProvider {
        id: "openai-codex".to_owned(),
        name: "OpenAI Codex".to_owned(),
        kind: KIND_OPENAI_RESPONSES.to_owned(),
        base_url: "https://chatgpt.com/backend-api/codex".to_owned(),
        doc: Some("https://developers.openai.com/codex".to_owned()),
        auth: AUTH_DEVICE_CODE.to_owned(),
        models: [
            ("o3-pro", "o3-pro"),
            ("o3", "o3"),
            ("gpt-5.3-codex-spark", "GPT-5.3 Codex Spark"),
            ("gpt-5.5", "GPT-5.5"),
            ("gpt-5.6-luna", "GPT-5.6 Luna"),
            ("gpt-5.6-sol", "GPT-5.6 Sol"),
            ("gpt-5.6-terra", "GPT-5.6 Terra"),
            ("gpt-6-astra", "GPT-6 Astra"),
        ]
        .into_iter()
        .map(|(id, name)| CatalogModel {
            id: id.to_owned(),
            name: name.to_owned(),
            reasoning: true,
            tool_call: true,
            ..CatalogModel::default()
        })
        .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::CatalogModel;

    #[test]
    fn published_efforts_are_not_padded_with_a_guessed_ladder() {
        let model = CatalogModel {
            id: "glm-5.3-flash".to_owned(),
            reasoning: true,
            reasoning_efforts: vec!["low".to_owned(), "high".to_owned(), "max".to_owned()],
            ..CatalogModel::default()
        };
        let levels = model.reasoning_levels();
        assert_eq!(
            levels,
            vec![
                "default".to_owned(),
                "low".to_owned(),
                "high".to_owned(),
                "max".to_owned()
            ]
        );
    }

    #[test]
    fn a_toggle_adds_off_and_on_without_inventing_efforts() {
        let model = CatalogModel {
            id: "claude".to_owned(),
            reasoning: true,
            reasoning_toggle: true,
            ..CatalogModel::default()
        };
        assert_eq!(
            model.reasoning_levels(),
            vec!["default".to_owned(), "off".to_owned(), "on".to_owned()]
        );
    }

    #[test]
    fn a_custom_glm_provider_borrows_limits_and_reasoning_levels() {
        let document = super::parse_snapshot(include_bytes!("catalog/snapshot.json"));
        let model = document
            .model_for_endpoint("my-glm", Some("https://glm.example/v1"), "glm-4.7")
            .expect("catalog row");
        assert!(model.reasoning_toggle);
        assert!(model.output > 0);
        assert!(model.context > 0);
        let levels = model.reasoning_levels();
        assert!(
            levels.iter().any(|level| level == "off"),
            "glm-4.7 toggle should offer Off, got {levels:?}"
        );
        assert!(levels.iter().any(|level| level == "on"), "{levels:?}");
        assert!(levels.iter().any(|level| level == "default"), "{levels:?}");
        assert!(
            document
                .model_for_endpoint("302ai", None, "not-a-real-model")
                .is_none(),
            "a known provider does not borrow another vendor's model"
        );
    }

    #[test]
    fn a_non_glm_effort_list_stays_as_published() {
        let model = CatalogModel {
            id: "gpt-5".to_owned(),
            reasoning: true,
            reasoning_efforts: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
            ..CatalogModel::default()
        };
        let levels = model.reasoning_levels();
        assert!(!levels.iter().any(|level| level == "off"));
        assert!(!levels.iter().any(|level| level == "on"));
        assert!(!levels.iter().any(|level| level == "max"));
        assert!(levels.iter().any(|level| level == "high"));
    }
}
