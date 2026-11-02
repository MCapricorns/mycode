//! The model catalog the picker and the turn planner read.
//!
//! magpie's gateway is the only model source: its `/v1/models` answer is
//! parsed into one provider row (`magpie`), and the gateway translates
//! between the OpenAI, Anthropic, and Gemini APIs on its side.

pub mod magpie;

use serde::{Deserialize, Serialize};

/// Wire protocol: OpenAI Chat Completions.
pub const KIND_OPENAI_COMPLETIONS: &str = "openai-completions";

/// Upper bound for model entries in one provider.
pub const MAX_MODELS_PER_PROVIDER: usize = 512;
/// Upper bound for one catalog string field.
pub const MAX_STRING_BYTES: usize = 8 * 1024;

/// One model the gateway offers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CatalogModel {
    /// Model id as sent to the provider.
    pub id: String,
    /// Other model ids that resolve to this route.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Upstream source published by the gateway, distinct from the gateway itself.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// Display name.
    pub name: String,
    /// Supports reasoning output.
    pub reasoning: bool,
    /// A published on/off toggle in `reasoning_levels`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reasoning_toggle: bool,
    /// Effort values the gateway publishes, lowercased.
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
    /// Only levels the gateway published are offered. A toggle adds Off and
    /// On. An effort list is kept as published (`none` is shown as Off). A
    /// model that merely sets `reasoning: true` does not grow a guessed
    /// ladder: it offers Default alone, which leaves the provider's own
    /// setting.
    #[must_use]
    pub fn reasoning_levels(&self) -> Vec<String> {
        if !self.reasoning && self.reasoning_efforts.is_empty() && !self.reasoning_toggle {
            return Vec::new();
        }
        let mut levels = Vec::new();
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
        if levels.is_empty() {
            levels.push("default".to_owned());
        }
        levels
    }
}

/// One provider row: endpoint data plus its model list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CatalogProvider {
    /// Stable provider id (`magpie`).
    pub id: String,
    /// Display name.
    pub name: String,
    /// MYCode wire protocol for this endpoint.
    pub kind: String,
    /// API base URL.
    pub base_url: String,
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
            models: Vec::new(),
        }
    }
}

impl CatalogProvider {
    /// Looks one model up by exact id first, then by alias.
    #[must_use]
    pub fn model(&self, id: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|model| model.id == id).or_else(|| {
            self.models
                .iter()
                .find(|model| model.aliases.iter().any(|alias| alias == id))
        })
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

    /// Looks one model up by provider and model id or alias.
    #[must_use]
    pub fn model(&self, provider_id: &str, model_id: &str) -> Option<&CatalogModel> {
        self.provider(provider_id)?.model(model_id)
    }

    /// Resolves a model for a configured endpoint.
    ///
    /// The catalog provider id wins, then an exact base URL. When neither
    /// names a catalog provider, the model id is matched across the whole
    /// catalog.
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
            return provider.model(model_id);
        }
        self.model_by_id(model_id)
    }

    fn model_by_id(&self, model_id: &str) -> Option<&CatalogModel> {
        for provider in &self.providers {
            if let Some(model) = provider.model(model_id) {
                return Some(model);
            }
        }
        None
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

/// Model ids keep the provider's own spelling: any nonempty printable id
/// without whitespace or control characters is accepted.
pub(crate) fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_STRING_BYTES
        && id.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

/// Normalizes one parsed provider: drops invalid entries, bounds the model
/// list, and sorts models by id.
pub(crate) fn normalize_provider(mut provider: CatalogProvider) -> Option<CatalogProvider> {
    if !valid_provider_id(&provider.id) {
        return None;
    }
    provider.name = clean_text(&provider.name).unwrap_or_else(|| provider.id.clone());
    provider.base_url = clean_text(&provider.base_url)?;
    provider.models.retain(|model| valid_model_id(&model.id));
    if provider.models.len() > MAX_MODELS_PER_PROVIDER {
        provider.models.truncate(MAX_MODELS_PER_PROVIDER);
    }
    for model in &mut provider.models {
        model
            .aliases
            .retain(|alias| valid_model_id(alias) && alias != &model.id);
        model.aliases.truncate(MAX_MODELS_PER_PROVIDER);
        model.aliases.sort();
        model.aliases.dedup();
        model.source = clean_text(&model.source).unwrap_or_default();
        model.name = clean_text(&model.name).unwrap_or_else(|| model.id.clone());
        model.cost_in = model
            .cost_in
            .filter(|cost| *cost >= 0.0 && cost.is_finite());
        model.cost_out = model
            .cost_out
            .filter(|cost| *cost >= 0.0 && cost.is_finite());
    }
    provider.models.sort_by(|a, b| a.id.cmp(&b.id));
    provider.models.dedup_by(|a, b| a.id == b.id);
    (!provider.models.is_empty()).then_some(provider)
}

#[cfg(test)]
mod tests {
    use super::{
        CatalogDocument, CatalogModel, CatalogProvider, MAX_MODELS_PER_PROVIDER, MAX_STRING_BYTES,
        normalize_provider,
    };

    #[test]
    fn published_efforts_are_not_padded_with_a_guessed_ladder() {
        let model = CatalogModel {
            id: "glm-5.3-flash".to_owned(),
            reasoning: true,
            reasoning_efforts: vec!["low".to_owned(), "high".to_owned(), "max".to_owned()],
            ..CatalogModel::default()
        };
        assert_eq!(
            model.reasoning_levels(),
            vec!["low".to_owned(), "high".to_owned(), "max".to_owned()]
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
            vec!["off".to_owned(), "on".to_owned()]
        );
        let bare = CatalogModel {
            id: "bare".to_owned(),
            reasoning: true,
            ..CatalogModel::default()
        };
        assert_eq!(bare.reasoning_levels(), vec!["default".to_owned()]);
    }

    #[test]
    fn exact_model_ids_win_over_aliases() {
        let provider = CatalogProvider {
            models: vec![
                CatalogModel {
                    id: "copilot/model".to_owned(),
                    aliases: vec!["model".to_owned()],
                    ..CatalogModel::default()
                },
                CatalogModel {
                    id: "model".to_owned(),
                    ..CatalogModel::default()
                },
            ],
            ..CatalogProvider::default()
        };
        assert_eq!(provider.model("model"), Some(&provider.models[1]));
        assert_eq!(provider.model("copilot/model"), Some(&provider.models[0]));
        assert!(provider.model("missing").is_none());
    }

    #[test]
    fn aliases_default_to_empty_and_serialize_only_when_present() {
        let mut model: CatalogModel = serde_json::from_str(r#"{"id":"copilot/model"}"#)
            .expect("legacy model without aliases");
        assert!(model.aliases.is_empty());
        assert!(
            serde_json::to_value(&model)
                .expect("model")
                .get("aliases")
                .is_none()
        );
        model.aliases.push("model".to_owned());
        let encoded = serde_json::to_value(&model).expect("model with alias");
        assert_eq!(encoded["aliases"], serde_json::json!(["model"]));
        assert_eq!(
            serde_json::from_value::<CatalogModel>(encoded).expect("round trip"),
            model
        );
    }

    #[test]
    fn normalization_validates_aliases_like_ids_and_bounds_their_count() {
        let provider = CatalogProvider {
            id: "magpie".to_owned(),
            base_url: "http://localhost:1/v1".to_owned(),
            models: vec![
                CatalogModel {
                    id: "copilot/model".to_owned(),
                    aliases: vec![
                        "z".to_owned(),
                        "alias".to_owned(),
                        "alias".to_owned(),
                        "copilot/model".to_owned(),
                        String::new(),
                        "has space".to_owned(),
                        "has\ncontrol".to_owned(),
                        "x".repeat(MAX_STRING_BYTES + 1),
                        "模型".to_owned(),
                    ],
                    ..CatalogModel::default()
                },
                CatalogModel {
                    id: "bounded".to_owned(),
                    aliases: (0..MAX_MODELS_PER_PROVIDER + 1)
                        .map(|index| format!("alias-{index:04}"))
                        .collect(),
                    ..CatalogModel::default()
                },
                CatalogModel {
                    id: "invalid id".to_owned(),
                    ..CatalogModel::default()
                },
            ],
            ..CatalogProvider::default()
        };
        let provider = normalize_provider(provider).expect("normalized provider");
        assert_eq!(provider.models.len(), 2);
        let model = provider.model("copilot/model").expect("route");
        assert_eq!(model.aliases, ["alias", "z", "模型"]);
        assert_eq!(provider.model("alias"), Some(model));
        let bounded = provider.model("bounded").expect("bounded aliases");
        assert_eq!(bounded.aliases.len(), MAX_MODELS_PER_PROVIDER);
        assert!(bounded.aliases.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn known_providers_keep_alias_resolution_within_their_own_models() {
        let document = CatalogDocument {
            providers: vec![
                CatalogProvider {
                    id: "magpie".to_owned(),
                    models: vec![CatalogModel {
                        id: "copilot/model".to_owned(),
                        aliases: vec!["model".to_owned()],
                        ..CatalogModel::default()
                    }],
                    ..CatalogProvider::default()
                },
                CatalogProvider {
                    id: "other".to_owned(),
                    base_url: "http://other/v1".to_owned(),
                    ..CatalogProvider::default()
                },
            ],
        };
        assert!(document.model("magpie", "model").is_some());
        assert!(document.model("other", "model").is_none());
        assert!(
            document
                .model_for_endpoint("other", None, "model")
                .is_none()
        );
        assert!(
            document
                .model_for_endpoint("unknown", Some("http://other/v1"), "model")
                .is_none()
        );
    }

    #[test]
    fn a_known_provider_does_not_borrow_another_providers_model() {
        let document = CatalogDocument {
            providers: vec![CatalogProvider {
                id: "magpie".to_owned(),
                models: vec![CatalogModel {
                    id: "glm-4.7".to_owned(),
                    ..CatalogModel::default()
                }],
                ..CatalogProvider::default()
            }],
        };
        assert!(document.model("magpie", "glm-4.7").is_some());
        assert!(document.model("other", "glm-4.7").is_none());
        assert!(
            document
                .model_for_endpoint("other", None, "glm-4.7")
                .is_some(),
            "an unknown provider falls back to the model id"
        );
    }
}
