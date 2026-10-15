//! The OpenDesign gateway (`https://amr-link.open-design.ai/v1`).
//!
//! The model data follows `@lokeraar/pi-opendesign-bridge`. The gateway's
//! `/models` answer carries ids, input modalities, and
//! `metadata.context_limit` / `context_budget`, but no reasoning levels and no
//! real output ceiling, so the catalog is built in two layers:
//!
//! - **Curated**: ids measured against this gateway. Reasoning levels the
//!   gateway rejects are left out, `off` is offered only where
//!   `reasoning_effort: "none"` both passes and stops the thinking, and the
//!   output cap is the lower of the gateway budget and the vendor maximum.
//! - **Live**: `GET {base}/models` with the user's key decides membership:
//!   new ids appear and retired ones drop. A curated id keeps its curated
//!   values. Any other id takes reasoning, output, and image support from the
//!   models.dev catalog by bare id (OpenRouter first), and its context window
//!   from the gateway, which states what it actually serves. No price is
//!   borrowed: a donor's price belongs to another reseller.
//!
//! The live listing is stored in `opendesign-models.json` and merged again on
//! every catalog load, so a later curated correction still wins. The bridge's
//! per-id probe (about 15 paid requests per new model) is not run.
//!
//! The provider is attached to the catalog document under its own id so model
//! limits and thinking levels resolve like any other row. The settings page
//! keeps it out of the models.dev provider list and opens it from its own
//! button.
use mycode_config::{HomeLayout, locked_update_owned_file, read_owned_file};
use serde::{Deserialize, Serialize};

use super::{
    CatalogDocument, CatalogModel, CatalogProvider, KIND_OPENAI_COMPLETIONS,
    MAX_MODELS_PER_PROVIDER, valid_model_id,
};

/// Provider id used in settings and the catalog.
pub const PROVIDER_ID: &str = "opendesign";
/// Display name.
pub const PROVIDER_NAME: &str = "OpenDesign";
/// OpenAI-compatible base URL of the gateway.
pub const BASE_URL: &str = "https://amr-link.open-design.ai/v1";
/// Live listing cache below the owned home.
pub const STORE_PATH: &str = "opendesign-models.json";

const STORE_KIND: &str = "mycode-opendesign-models";
const STORE_FORMAT_VERSION: u32 = 1;
const MAX_STORE_BYTES: usize = 1024 * 1024;
/// Largest `/models` body accepted.
const MAX_LISTING_BYTES: usize = 4 * 1024 * 1024;
/// Tokens kept free under the window. The gateway rejects a ceiling equal to
/// the window: it counts messages + tools + `max_tokens` against it.
const PROMPT_RESERVE_TOKENS: u64 = 2_048;
/// Output cap for an id neither the curated table nor a donor describes.
const FALLBACK_OUTPUT_TOKENS: u64 = 16_384;

/// One curated row: limits, image input, and the accepted effort tokens.
/// `none` in `efforts` is the measured Off switch.
struct Curated {
    id: &'static str,
    context: u64,
    output: u64,
    image: bool,
    efforts: &'static [&'static str],
}

const OFF_AND_ALL: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
/// GLM-5.3 accepts only `low` / `high` / `max` and rejects `none`.
const GLM_53: &[&str] = &["low", "high", "max"];
/// GPT-6 rejects `minimal`; Luna accepts `none`, Sol does not.
const GPT_6_LUNA: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];
const GPT_61_SOL: &[&str] = &["low", "medium", "high", "xhigh", "max"];
/// K2.7 Code always thinks and rejects `max`.
const KIMI_K27_CODE: &[&str] = &["minimal", "low", "medium", "high", "xhigh"];

/// Measured on 2026-10-01 against the gateway (pi-opendesign-bridge 0.2.9).
/// `context` is the gateway's `context_limit`; `output` is the lower of its
/// `context_budget` and the vendor's published maximum.
const CURATED: &[Curated] = &[
    Curated {
        id: "deepseek-v4-flash",
        context: 1_048_000,
        output: 232_000,
        image: false,
        efforts: OFF_AND_ALL,
    },
    Curated {
        id: "deepseek-v4-flash-vision-exp",
        context: 1_048_000,
        output: 232_000,
        image: true,
        efforts: OFF_AND_ALL,
    },
    Curated {
        id: "deepseek-v4-pro",
        context: 1_048_000,
        output: 232_000,
        image: false,
        efforts: OFF_AND_ALL,
    },
    Curated {
        id: "deepseek-v4.1-flash",
        context: 1_048_000,
        output: 232_000,
        image: true,
        efforts: OFF_AND_ALL,
    },
    Curated {
        id: "glm-5.3-flash",
        context: 1_048_000,
        output: 128_000,
        image: true,
        efforts: GLM_53,
    },
    Curated {
        id: "glm-5.3-flashx",
        context: 1_000_000,
        output: 128_000,
        image: true,
        efforts: GLM_53,
    },
    Curated {
        id: "gpt-6-luna",
        context: 1_050_000,
        output: 128_000,
        image: true,
        efforts: GPT_6_LUNA,
    },
    Curated {
        id: "gpt-6.1-sol",
        context: 1_050_000,
        output: 128_000,
        image: true,
        efforts: GPT_61_SOL,
    },
    Curated {
        id: "kimi-k2.7-code",
        context: 262_000,
        output: 262_000,
        image: true,
        efforts: KIMI_K27_CODE,
    },
    Curated {
        id: "mimo-v2.6-flash",
        context: 1_048_000,
        output: 131_072,
        image: true,
        efforts: OFF_AND_ALL,
    },
    Curated {
        id: "mimo-v2.6-pro",
        context: 1_048_000,
        output: 131_072,
        image: true,
        efforts: OFF_AND_ALL,
    },
];

/// One chat model the gateway lists for a key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LiveModel {
    /// Model id as sent to the gateway.
    pub id: String,
    /// Whether `architecture.input_modalities` names `image`; `None` when
    /// the gateway published no modalities.
    pub image: Option<bool>,
    /// `metadata.context_limit`: the window the gateway serves.
    pub context_limit: Option<u64>,
    /// `metadata.context_budget`: the largest `max_tokens` it accepts.
    pub context_budget: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreDocument {
    format_version: u32,
    kind: String,
    fetched_at: u64,
    models: Vec<LiveModel>,
}

/// An output cap the window can hold: a cap at or above the window always
/// fails, so `PROMPT_RESERVE_TOKENS` stay free (never below 1024).
fn clamp_output(output: u64, context: u64) -> u64 {
    if context == 0 {
        return output;
    }
    output.min(context.saturating_sub(PROMPT_RESERVE_TOKENS).max(1_024))
}

fn curated_model(row: &Curated) -> CatalogModel {
    CatalogModel {
        id: row.id.to_owned(),
        name: row.id.to_owned(),
        reasoning: true,
        reasoning_toggle: false,
        reasoning_efforts: row
            .efforts
            .iter()
            .map(|effort| (*effort).to_owned())
            .collect(),
        tool_call: true,
        attachment: row.image,
        context: row.context,
        output: clamp_output(row.output, row.context),
        cost_in: None,
        cost_out: None,
    }
}

/// The curated models, used until a live listing is stored.
#[must_use]
pub fn curated_models() -> Vec<CatalogModel> {
    CURATED.iter().map(curated_model).collect()
}

/// Wraps a model list in the OpenDesign provider row.
#[must_use]
pub fn provider(mut models: Vec<CatalogModel>) -> CatalogProvider {
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    CatalogProvider {
        id: PROVIDER_ID.to_owned(),
        name: PROVIDER_NAME.to_owned(),
        kind: KIND_OPENAI_COMPLETIONS.to_owned(),
        base_url: BASE_URL.to_owned(),
        doc: None,
        auth: String::new(),
        models,
    }
}

/// Replaces the OpenDesign row of `document`: the live listing merged over
/// the curated table when one is given, the curated table otherwise.
pub fn attach(document: &mut CatalogDocument, live: Option<&[LiveModel]>) {
    let models = match live {
        Some(live) if !live.is_empty() => merge(live, document),
        _ => curated_models(),
    };
    let row = provider(models);
    match document
        .providers
        .iter_mut()
        .find(|existing| existing.id == PROVIDER_ID)
    {
        Some(existing) => *existing = row,
        None => {
            document.providers.push(row);
            document.providers.sort_by(|a, b| a.id.cmp(&b.id));
        }
    }
}

/// Attaches the stored live listing, if any, to `document`.
pub fn attach_stored(home: &HomeLayout, document: &mut CatalogDocument) {
    let stored = read_store(home);
    attach(document, stored.as_deref());
}

/// Builds the catalog rows for one live listing. See the module docs for
/// which layer decides each value.
#[must_use]
pub fn merge(live: &[LiveModel], donors: &CatalogDocument) -> Vec<CatalogModel> {
    let mut models: Vec<CatalogModel> = Vec::new();
    for listing in live {
        if models.len() >= MAX_MODELS_PER_PROVIDER {
            break;
        }
        if !valid_model_id(&listing.id) || models.iter().any(|model| model.id == listing.id) {
            continue;
        }
        if let Some(row) = CURATED.iter().find(|row| row.id == listing.id) {
            models.push(curated_model(row));
            continue;
        }
        models.push(uncurated_model(listing, donor_for(donors, &listing.id)));
    }
    models
}

fn uncurated_model(listing: &LiveModel, donor: Option<&CatalogModel>) -> CatalogModel {
    let context = listing
        .context_limit
        .or_else(|| {
            donor
                .map(|model| model.context)
                .filter(|context| *context > 0)
        })
        .unwrap_or(0);
    // The lower known ceiling wins: the donor's vendor maximum, or the
    // gateway's own budget. Neither known: a conservative cap.
    let donor_output = donor.map(|model| model.output).filter(|output| *output > 0);
    let output = match (donor_output, listing.context_budget) {
        (Some(donor), Some(budget)) => donor.min(budget),
        (Some(cap), None) | (None, Some(cap)) => cap,
        (None, None) => FALLBACK_OUTPUT_TOKENS,
    };
    CatalogModel {
        id: listing.id.clone(),
        name: donor.map_or_else(|| listing.id.clone(), |model| model.name.clone()),
        reasoning: donor.is_none_or(|model| model.reasoning),
        reasoning_toggle: donor.is_some_and(|model| model.reasoning_toggle),
        reasoning_efforts: donor
            .map(|model| model.reasoning_efforts.clone())
            .unwrap_or_default(),
        tool_call: true,
        attachment: listing
            .image
            .unwrap_or_else(|| donor.is_some_and(|model| model.attachment)),
        context,
        output: clamp_output(output, context),
        cost_in: None,
        cost_out: None,
    }
}

/// The models.dev row that describes `id`, matched on the bare id (the part
/// after the last `/`, case-insensitive) in every provider but this one.
///
/// OpenRouter wins: it routes these models for a living and keeps the
/// numbers current. Otherwise the first row that publishes reasoning options
/// wins, then one with an output limit, then any row.
fn donor_for<'a>(catalog: &'a CatalogDocument, id: &str) -> Option<&'a CatalogModel> {
    let bare = bare_id(id);
    let mut with_options = None;
    let mut with_output = None;
    let mut any = None;
    for provider in &catalog.providers {
        if provider.id == PROVIDER_ID {
            continue;
        }
        let Some(model) = provider
            .models
            .iter()
            .find(|model| bare_id(&model.id).eq_ignore_ascii_case(bare))
        else {
            continue;
        };
        if provider.id == "openrouter" {
            return Some(model);
        }
        if with_options.is_none() && (model.reasoning_toggle || !model.reasoning_efforts.is_empty())
        {
            with_options = Some(model);
        }
        if with_output.is_none() && model.output > 0 {
            with_output = Some(model);
        }
        if any.is_none() {
            any = Some(model);
        }
    }
    with_options.or(with_output).or(any)
}

fn bare_id(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

/// Embedding, speech, image, moderation, and rerank ids are not chat models.
fn is_non_chat(id: &str) -> bool {
    const WORDS: &[&str] = &[
        "embed",
        "embedding",
        "embeddings",
        "tts",
        "whisper",
        "dalle",
        "clip",
        "moderation",
        "rerank",
        "reranker",
    ];
    let lower = id.to_ascii_lowercase();
    lower.starts_with("image-")
        || lower.starts_with("image_")
        || lower.contains("-embed")
        || lower.contains("_embed")
        || lower.contains("dall-e")
        || lower
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .any(|word| WORDS.contains(&word))
}

#[derive(Deserialize)]
struct ListingDocument {
    #[serde(default)]
    data: Vec<serde_json::Value>,
}

/// Parses a gateway `/models` body into chat models.
///
/// Disabled entries and non-chat ids are dropped; so are invalid ids.
///
/// # Errors
///
/// Returns a message when the body is not the OpenAI list shape.
pub fn parse_listing(bytes: &[u8]) -> Result<Vec<LiveModel>, String> {
    let document: ListingDocument = serde_json::from_slice(bytes)
        .map_err(|error| format!("OpenDesign /models was not a model list: {error}"))?;
    let positive = |value: Option<&serde_json::Value>| {
        value
            .and_then(serde_json::Value::as_u64)
            .filter(|tokens| *tokens > 0)
    };
    let mut models: Vec<LiveModel> = Vec::new();
    for entry in &document.data {
        if models.len() >= MAX_MODELS_PER_PROVIDER {
            break;
        }
        let Some(id) = entry.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if entry.get("enabled").and_then(serde_json::Value::as_bool) == Some(false)
            || !valid_model_id(id)
            || is_non_chat(id)
            || models.iter().any(|model| model.id == id)
        {
            continue;
        }
        let image = entry
            .pointer("/architecture/input_modalities")
            .and_then(serde_json::Value::as_array)
            .filter(|modalities| !modalities.is_empty())
            .map(|modalities| {
                modalities
                    .iter()
                    .any(|modality| modality.as_str() == Some("image"))
            });
        models.push(LiveModel {
            id: id.to_owned(),
            image,
            context_limit: positive(entry.pointer("/metadata/context_limit")),
            context_budget: positive(entry.pointer("/metadata/context_budget")),
        });
    }
    Ok(models)
}

/// Reads the stored live listing. A missing, damaged, or empty store is
/// `None`: the curated table stands in and the next fetch rewrites the file.
#[must_use]
pub fn read_store(home: &HomeLayout) -> Option<Vec<LiveModel>> {
    let bytes = read_owned_file(home, STORE_PATH, MAX_STORE_BYTES).ok()??;
    let document: StoreDocument = serde_json::from_slice(&bytes).ok()?;
    (document.kind == STORE_KIND
        && document.format_version == STORE_FORMAT_VERSION
        && !document.models.is_empty())
    .then_some(document.models)
}

/// Replaces the stored live listing.
///
/// # Errors
///
/// Returns a message when the file cannot be written.
pub fn write_store(home: &HomeLayout, models: &[LiveModel]) -> Result<(), String> {
    let document = StoreDocument {
        format_version: STORE_FORMAT_VERSION,
        kind: STORE_KIND.to_owned(),
        fetched_at: super::store::unix_now(),
        models: models.to_vec(),
    };
    let mut bytes = serde_json::to_vec(&document).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if bytes.len() > MAX_STORE_BYTES {
        return Err("OpenDesign model list exceeded its size bound".to_owned());
    }
    locked_update_owned_file(home, STORE_PATH, MAX_STORE_BYTES, |_| Ok(bytes.clone()))
        .map_err(|error| error.to_string())
}

/// Fetches the chat models one key can use from `{base_url}/models`.
///
/// # Errors
///
/// Returns a message naming the HTTP status, a transport failure, or a body
/// that is not a model list. The key never appears in the message.
pub async fn fetch_live_models(
    base_url: &str,
    api_key: &str,
    user_agent: &str,
) -> Result<Vec<LiveModel>, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let response = crate::send_pinned(crate::PinnedRequest {
        method: reqwest::Method::GET,
        url,
        headers: vec![
            ("accept".to_owned(), "application/json".to_owned()),
            ("authorization".to_owned(), format!("Bearer {api_key}")),
        ],
        body: None,
        mode: crate::PinMode::CheckRedirect,
        timeout: Some(std::time::Duration::from_secs(20)),
        read_timeout: None,
        user_agent: (!user_agent.is_empty()).then(|| user_agent.to_owned()),
        // The bearer key must not follow a redirect to another host.
        follow_redirects: false,
        cancel: tokio_util::sync::CancellationToken::new(),
    })
    .await
    .map_err(|error| format!("OpenDesign /models failed: {error}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(format!(
            "OpenDesign rejected the API key (HTTP {})",
            status.as_u16()
        ));
    }
    if !status.is_success() {
        return Err(format!(
            "OpenDesign /models returned HTTP {}",
            status.as_u16()
        ));
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("OpenDesign /models read failed: {error}"))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_LISTING_BYTES {
            return Err("OpenDesign /models answer was oversized".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    let models = parse_listing(&bytes)?;
    if models.is_empty() {
        return Err("OpenDesign listed no chat models for this key".to_owned());
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::{BASE_URL, LiveModel, PROVIDER_ID, attach, curated_models, merge, parse_listing};
    use crate::catalog::{CatalogDocument, CatalogModel, CatalogProvider};

    #[test]
    fn curated_levels_follow_the_measured_gateway_behaviour() {
        let models = curated_models();
        let levels = |id: &str| {
            models
                .iter()
                .find(|model| model.id == id)
                .expect("curated id")
                .reasoning_levels()
        };
        // GLM-5.3 accepts low / high / max only and has no Off switch.
        assert_eq!(levels("glm-5.3-flash"), ["low", "high", "max"]);
        // `none` passes and disables thinking on DeepSeek: Off is offered.
        let deepseek = levels("deepseek-v4-flash");
        assert_eq!(deepseek.first().map(String::as_str), Some("off"));
        assert!(deepseek.iter().any(|level| level == "xhigh"));
        // K2.7 Code always thinks and rejects max.
        let kimi = levels("kimi-k2.7-code");
        assert!(!kimi.iter().any(|level| level == "off" || level == "max"));
        // GPT-6 rejects minimal.
        assert!(!levels("gpt-6.1-sol").iter().any(|level| level == "minimal"));
    }

    #[test]
    fn a_ceiling_equal_to_the_window_keeps_room_for_the_prompt() {
        let kimi = curated_models()
            .into_iter()
            .find(|model| model.id == "kimi-k2.7-code")
            .expect("kimi");
        assert_eq!(kimi.context, 262_000);
        assert_eq!(kimi.output, 262_000 - 2_048);
    }

    #[test]
    fn listing_drops_disabled_and_non_chat_ids_and_reads_metadata() {
        let body = serde_json::json!({
            "object": "list",
            "data": [
                {
                    "id": "glm-5.3-flash",
                    "architecture": { "input_modalities": ["text", "image"] },
                    "metadata": { "context_limit": 1048000, "context_budget": 232000 }
                },
                { "id": "text-embedding-3-large" },
                { "id": "image-gen-2" },
                { "id": "whisper-1" },
                { "id": "old-model", "enabled": false },
                { "id": "brand-new", "architecture": { "input_modalities": ["text"] } },
                { "id": "brand-new" },
                { "id": "no modalities" },
                { "id": 7 }
            ]
        });
        let models = parse_listing(body.to_string().as_bytes()).expect("listing");
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, ["glm-5.3-flash", "brand-new"]);
        assert_eq!(models[0].image, Some(true));
        assert_eq!(models[0].context_limit, Some(1_048_000));
        assert_eq!(models[0].context_budget, Some(232_000));
        assert_eq!(models[1].image, Some(false));
        assert!(parse_listing(b"not json").is_err());
    }

    fn donors() -> CatalogDocument {
        let row = |efforts: &[&str], output: u64, attachment: bool| CatalogModel {
            id: "vendor/brand-new".to_owned(),
            name: "Brand New".to_owned(),
            reasoning: true,
            reasoning_efforts: efforts.iter().map(|effort| (*effort).to_owned()).collect(),
            output,
            attachment,
            context: 400_000,
            cost_in: Some(3.0),
            ..CatalogModel::default()
        };
        CatalogDocument {
            providers: vec![
                CatalogProvider {
                    id: "aaa-reseller".to_owned(),
                    models: vec![row(&["low"], 50_000, true)],
                    ..CatalogProvider::default()
                },
                CatalogProvider {
                    id: "openrouter".to_owned(),
                    models: vec![row(&["low", "high"], 64_000, true)],
                    ..CatalogProvider::default()
                },
            ],
        }
    }

    #[test]
    fn live_membership_with_curated_values_and_openrouter_donor() {
        let live = [
            LiveModel {
                id: "glm-5.3-flash".to_owned(),
                image: Some(false),
                context_limit: Some(10),
                context_budget: Some(10),
            },
            LiveModel {
                id: "brand-new".to_owned(),
                image: None,
                context_limit: Some(300_000),
                context_budget: Some(100_000),
            },
            LiveModel {
                id: "unknown-anywhere".to_owned(),
                image: Some(true),
                context_limit: None,
                context_budget: None,
            },
        ];
        let models = merge(&live, &donors());
        assert_eq!(models.len(), 3, "retired curated ids drop out");
        // Curated values are never overwritten by gateway metadata.
        let glm = &models[0];
        assert_eq!(glm.context, 1_048_000);
        assert!(glm.attachment);
        // OpenRouter donates reasoning and output; the gateway keeps the window.
        let fresh = &models[1];
        assert_eq!(fresh.name, "Brand New");
        assert_eq!(fresh.reasoning_efforts, ["low", "high"]);
        assert_eq!(fresh.context, 300_000);
        assert_eq!(fresh.output, 64_000);
        assert!(fresh.attachment, "no gateway modalities: the donor decides");
        assert_eq!(fresh.cost_in, None, "a donor's price is another reseller's");
        // Nobody describes it: Default only, a conservative cap.
        let unknown = &models[2];
        assert!(unknown.reasoning_efforts.is_empty());
        assert_eq!(unknown.output, 16_384);
        assert!(unknown.attachment);
    }

    #[test]
    fn attach_replaces_the_row_and_keeps_providers_sorted() {
        let mut document = donors();
        attach(&mut document, None);
        let ids: Vec<&str> = document.providers.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["aaa-reseller", PROVIDER_ID, "openrouter"]);
        let row = document.provider(PROVIDER_ID).expect("row");
        assert_eq!(row.base_url, BASE_URL);
        assert_eq!(row.models.len(), curated_models().len());
        attach(
            &mut document,
            Some(&[LiveModel {
                id: "brand-new".to_owned(),
                ..LiveModel::default()
            }]),
        );
        let row = document.provider(PROVIDER_ID).expect("row");
        assert_eq!(row.models.len(), 1);
        assert_eq!(document.providers.len(), 3);
        // The live row resolves through the normal endpoint lookup.
        assert!(
            document
                .model_for_endpoint("opendesign-2", Some(BASE_URL), "brand-new")
                .is_some()
        );
    }
}
