//! Parser for the raw models.dev `api.json` cloud document.
//!
//! The cloud document is normalized into the same compact schema as the
//! vendored snapshot: only providers MYCode can serve (anthropic-messages or
//! OpenAI-compatible endpoints) are kept, first-party labs without a
//! published `api` field are filled from a pinned endpoint table, and cloud
//! SDK consoles (bedrock/vertex/azure/google) are excluded. OAuth device-code
//! vendors such as `github-copilot` are kept with an `auth` marker.
use serde::Deserialize;

use super::{
    AUTH_DEVICE_CODE, CatalogDocument, CatalogModel, CatalogProvider, KIND_ANTHROPIC_MESSAGES,
    KIND_OPENAI_COMPLETIONS, KIND_OPENAI_RESPONSES, clean_text, normalize, valid_provider_id,
};

/// Cloud source of the provider catalog.
pub const MODELS_DEV_API_URL: &str = "https://models.dev/api.json";

/// Known base URLs for first-party labs that models.dev lists without an
/// `api` field. Pinned from the vendors' published endpoints.
fn endpoint_fix(provider_id: &str) -> Option<(&'static str, &'static str)> {
    match provider_id {
        "anthropic" => Some((KIND_ANTHROPIC_MESSAGES, "https://api.anthropic.com")),
        "openai" => Some((KIND_OPENAI_COMPLETIONS, "https://api.openai.com/v1")),
        "groq" => Some((KIND_OPENAI_COMPLETIONS, "https://api.groq.com/openai/v1")),
        "mistral" => Some((KIND_OPENAI_COMPLETIONS, "https://api.mistral.ai/v1")),
        "xai" => Some((KIND_OPENAI_COMPLETIONS, "https://api.x.ai/v1")),
        "cerebras" => Some((KIND_OPENAI_COMPLETIONS, "https://api.cerebras.ai/v1")),
        "perplexity" => Some((KIND_OPENAI_COMPLETIONS, "https://api.perplexity.ai")),
        "github-copilot" => Some((KIND_OPENAI_COMPLETIONS, "https://api.githubcopilot.com")),
        "openai-codex" => Some((
            KIND_OPENAI_RESPONSES,
            "https://chatgpt.com/backend-api/codex",
        )),
        _ => None,
    }
}

/// Providers that authenticate with an OAuth device flow instead of a pasted
/// API key; the settings UI renders a sign-in button for these.
fn device_code_auth(provider_id: &str) -> &'static str {
    match provider_id {
        "github-copilot" | "openai-codex" => AUTH_DEVICE_CODE,
        "xai" => super::AUTH_OAUTH,
        _ => "",
    }
}

/// Providers excluded from presets: their credentials are cloud SDK consoles
/// rather than a portable API key or a supported OAuth flow.
fn excluded(npm: &str) -> bool {
    ["bedrock", "vertex", "azure", "google"]
        .iter()
        .any(|token| npm.contains(token))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ModelsDevModel {
    name: String,
    status: String,
    reasoning: bool,
    tool_call: bool,
    attachment: bool,
    reasoning_options: Vec<ModelsDevReasoningOption>,
    limit: ModelsDevLimit,
    cost: ModelsDevCost,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ModelsDevReasoningOption {
    #[serde(rename = "type")]
    kind: String,
    values: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ModelsDevLimit {
    context: u64,
    output: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ModelsDevCost {
    input: serde_json::Value,
    output: serde_json::Value,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ModelsDevProvider {
    npm: String,
    name: String,
    api: String,
    doc: String,
    models: std::collections::BTreeMap<String, ModelsDevModel>,
}

fn number(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|cost| *cost >= 0.0 && cost.is_finite())
}

/// Parses and normalizes a raw models.dev `api.json` payload.
#[must_use]
pub fn parse_models_dev(bytes: &[u8]) -> CatalogDocument {
    let Ok(raw) =
        serde_json::from_slice::<std::collections::BTreeMap<String, ModelsDevProvider>>(bytes)
    else {
        return CatalogDocument::default();
    };
    let mut providers = Vec::new();
    for (provider_id, entry) in raw {
        if !valid_provider_id(&provider_id) || excluded(&entry.npm) {
            continue;
        }
        let auth = device_code_auth(&provider_id).to_owned();
        // A device-code vendor always uses the pinned chat endpoint, never
        // whatever console URL the cloud document happens to carry.
        let (kind, base_url) = if !auth.is_empty() {
            match endpoint_fix(&provider_id) {
                Some((kind, base)) => (kind.to_owned(), base.to_owned()),
                None => continue,
            }
        } else {
            let (kind, api) = if entry.npm.contains("anthropic") {
                (KIND_ANTHROPIC_MESSAGES, entry.api.clone())
            } else {
                (KIND_OPENAI_COMPLETIONS, entry.api.clone())
            };
            match clean_text(&api) {
                Some(base) if base.starts_with("https://") => (kind.to_owned(), base),
                _ => match endpoint_fix(&provider_id) {
                    Some((kind, base)) => (kind.to_owned(), base.to_owned()),
                    None => continue,
                },
            }
        };
        let mut models = Vec::new();
        for (model_id, model) in entry.models {
            if models.len() >= super::MAX_MODELS_PER_PROVIDER {
                break;
            }
            if !valid_model_id(&model_id) || clean_text(&model.status).is_some() {
                continue;
            }
            let (reasoning_toggle, reasoning_efforts) = reasoning_options(&model);
            models.push(CatalogModel {
                id: model_id.clone(),
                name: clean_text(&model.name).unwrap_or(model_id),
                reasoning: model.reasoning,
                reasoning_toggle,
                reasoning_efforts,
                tool_call: model.tool_call,
                attachment: model.attachment,
                context: model.limit.context,
                output: model.limit.output,
                cost_in: number(&model.cost.input),
                cost_out: number(&model.cost.output),
            });
        }
        if models.is_empty() {
            continue;
        }
        providers.push(CatalogProvider {
            id: provider_id,
            name: clean_text(&entry.name).unwrap_or_else(|| "provider".to_owned()),
            kind,
            base_url,
            doc: clean_text(&entry.doc).filter(|doc| doc.starts_with("https://")),
            auth,
            models,
        });
    }
    super::attach_subscription_presets(normalize(providers))
}

fn reasoning_options(model: &ModelsDevModel) -> (bool, Vec<String>) {
    const MAX_EFFORTS: usize = 8;
    let mut toggle = false;
    let mut efforts = Vec::new();
    for option in &model.reasoning_options {
        match option.kind.as_str() {
            "toggle" => toggle = true,
            "effort" => {
                for value in &option.values {
                    let Some(token) = clean_text(value) else {
                        continue;
                    };
                    let token = token.to_ascii_lowercase();
                    if mycode_core::ReasoningLevel::parse(&token).is_none() && token != "default" {
                        continue;
                    }
                    if !efforts.iter().any(|existing| existing == &token)
                        && efforts.len() < MAX_EFFORTS
                    {
                        efforts.push(token);
                    }
                }
            }
            _ => {}
        }
    }
    (toggle, efforts)
}

/// Model ids keep the provider's own spelling: any nonempty printable id
/// without whitespace or control characters is accepted.
fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= super::MAX_STRING_BYTES
        && id.chars().all(|c| !c.is_whitespace() && !c.is_control())
}
