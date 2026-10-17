//! The OpenAI Codex subscription backend (`https://chatgpt.com/backend-api/codex`).
//!
//! models.dev does not carry the Codex rows: the backend serves its own
//! catalog at `GET {base}/models?client_version=…`, the same endpoint Codex
//! CLI and other agents enumerate, gated on a plausible client version (a
//! stale one answers an empty `{"models": []}` without an error). The
//! provider is built in two layers:
//!
//! - **Preset**: [`super::openai_codex_preset`] — the hand-maintained list
//!   shown before sign-in and kept when no live listing was ever stored.
//! - **Live**: the fetched listing in `codex-models.json`, merged over the
//!   preset on every catalog load. Live membership wins; the backend's own
//!   reasoning ladder wins over a preset row it also names, while context
//!   and output limits fall back to the preset values the wire answer lacks.
use mycode_config::{HomeLayout, locked_update_owned_file, read_owned_file};
use serde::{Deserialize, Serialize};

use super::{CatalogDocument, CatalogModel, MAX_MODELS_PER_PROVIDER};

/// Provider id used in settings and the catalog.
pub const PROVIDER_ID: &str = "openai-codex";
/// Live listing cache below the owned home.
pub const STORE_PATH: &str = "codex-models.json";

const STORE_KIND: &str = "mycode-codex-models";
const STORE_FORMAT_VERSION: u32 = 1;
const MAX_STORE_BYTES: usize = 256 * 1024;
/// Largest `/models` body accepted.
const MAX_LISTING_BYTES: usize = 1024 * 1024;
/// The client version sent to `/models`. The backend gates on it like the
/// Codex CLI's own version; a too-low or foreign value returns an empty
/// list, so this mirrors a current `codex_cli_rs` release.
const CODEX_CLIENT_VERSION: &str = "1.0.0";

/// One model the Codex backend serves to this account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LiveModel {
    /// `slug`: the id sent as `model` on `/responses`.
    pub id: String,
    /// `display_name`.
    pub name: String,
    /// `supported_reasoning_levels[].effort` spellings, in served order.
    pub efforts: Vec<String>,
    /// `context_window`, when the listing publishes one.
    pub context_window: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreDocument {
    format_version: u32,
    kind: String,
    fetched_at: u64,
    models: Vec<LiveModel>,
}

/// Parses a Codex `/models` body into served rows.
///
/// `visibility: "hide"` rows (the auto-review slot) and invalid or
/// duplicated slugs are dropped.
///
/// # Errors
///
/// Returns a message when the body is not the Codex list shape.
pub fn parse_listing(bytes: &[u8]) -> Result<Vec<LiveModel>, String> {
    #[derive(Deserialize)]
    struct Listing {
        #[serde(default)]
        models: Vec<serde_json::Value>,
    }
    let document: Listing = serde_json::from_slice(bytes)
        .map_err(|error| format!("Codex /models was not a model list: {error}"))?;
    let mut models: Vec<LiveModel> = Vec::new();
    for entry in &document.models {
        if models.len() >= MAX_MODELS_PER_PROVIDER {
            break;
        }
        if entry.get("visibility").and_then(serde_json::Value::as_str) == Some("hide") {
            continue;
        }
        let Some(id) = entry.get("slug").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !super::valid_model_id(id) || models.iter().any(|model| model.id == id) {
            continue;
        }
        let efforts = entry
            .get("supported_reasoning_levels")
            .and_then(serde_json::Value::as_array)
            .map(|levels| {
                levels
                    .iter()
                    .filter_map(|level| {
                        level
                            .get("effort")
                            .and_then(serde_json::Value::as_str)
                            .or_else(|| level.as_str())
                    })
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        models.push(LiveModel {
            id: id.to_owned(),
            name: entry
                .get("display_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(id)
                .to_owned(),
            efforts,
            context_window: entry
                .get("context_window")
                .and_then(serde_json::Value::as_u64)
                .filter(|tokens| *tokens > 0),
        });
    }
    Ok(models)
}

/// Builds the catalog rows for one live listing: live membership wins, a
/// preset row the backend still serves keeps its curated limit fields, and
/// any other slug is synthesized from what the listing publishes.
#[must_use]
pub fn merge(live: &[LiveModel], preset: &[CatalogModel]) -> Vec<CatalogModel> {
    let mut models: Vec<CatalogModel> = Vec::new();
    for listing in live {
        if models.len() >= MAX_MODELS_PER_PROVIDER {
            break;
        }
        if models.iter().any(|model| model.id == listing.id) {
            continue;
        }
        let curated = preset.iter().find(|model| model.id == listing.id);
        models.push(CatalogModel {
            id: listing.id.clone(),
            name: if listing.name.is_empty() {
                curated.map_or_else(|| listing.id.clone(), |model| model.name.clone())
            } else {
                listing.name.clone()
            },
            reasoning: true,
            reasoning_toggle: false,
            reasoning_efforts: if listing.efforts.is_empty() {
                curated
                    .map(|model| model.reasoning_efforts.clone())
                    .unwrap_or_default()
            } else {
                listing.efforts.clone()
            },
            tool_call: true,
            attachment: curated.is_some_and(|model| model.attachment),
            context: listing
                .context_window
                .or_else(|| curated.map(|model| model.context).filter(|c| *c > 0))
                .unwrap_or(0),
            output: curated.map(|model| model.output).unwrap_or(0),
            cost_in: None,
            cost_out: None,
        });
    }
    models
}

/// Replaces or inserts the Codex row of `document`: the live listing merged
/// over the preset when one is given, the preset list otherwise.
pub fn attach(document: &mut CatalogDocument, live: Option<&[LiveModel]>) {
    let mut row = super::openai_codex_preset();
    if let Some(live) = live
        && !live.is_empty()
    {
        row.models = merge(live, &row.models);
    }
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

/// Reads the stored live listing. A missing, damaged, or empty store is
/// `None`: the preset stands in and the next sign-in rewrites the file.
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
        return Err("Codex model list exceeded its size bound".to_owned());
    }
    locked_update_owned_file(home, STORE_PATH, MAX_STORE_BYTES, |_| Ok(bytes.clone()))
        .map_err(|error| error.to_string())
}

/// Fetches the model catalog the Codex backend serves to this account.
///
/// # Errors
///
/// Returns a message naming the HTTP status, a transport failure, or a body
/// that is not a model list. The token never appears in the message.
pub async fn fetch_live_models(
    call: &crate::PinnedCall,
    access: &str,
    account_id: Option<&str>,
) -> Result<Vec<LiveModel>, String> {
    let url = format!(
        "https://chatgpt.com/backend-api/codex/models?client_version={CODEX_CLIENT_VERSION}"
    );
    let mut headers = vec![
        ("accept".to_owned(), "application/json".to_owned()),
        ("authorization".to_owned(), format!("Bearer {access}")),
        ("originator".to_owned(), "pi".to_owned()),
        (
            "openai-beta".to_owned(),
            "responses=experimental".to_owned(),
        ),
    ];
    if let Some(account) = account_id {
        headers.push(("chatgpt-account-id".to_owned(), account.to_owned()));
    }
    let response = crate::send_pinned(crate::PinnedRequest {
        method: reqwest::Method::GET,
        url,
        headers,
        body: None,
        mode: crate::PinMode::CheckRedirect,
        timeout: Some(call.timeout),
        read_timeout: None,
        user_agent: (!call.user_agent.is_empty()).then(|| call.user_agent.clone()),
        // The bearer token must not follow a redirect to another host.
        follow_redirects: false,
        cancel: tokio_util::sync::CancellationToken::new(),
    })
    .await
    .map_err(|error| format!("Codex /models failed: {error}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(format!(
            "Codex rejected the sign-in token (HTTP {})",
            status.as_u16()
        ));
    }
    if !status.is_success() {
        return Err(format!("Codex /models returned HTTP {}", status.as_u16()));
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Codex /models read failed: {error}"))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_LISTING_BYTES {
            return Err("Codex /models answer was oversized".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    let models = parse_listing(&bytes)?;
    if models.is_empty() {
        return Err("Codex listed no models for this account".to_owned());
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::{LiveModel, attach, merge, parse_listing};
    use crate::catalog::{CatalogDocument, CatalogProvider};

    const LISTING: &[u8] = br#"{"models":[
        {"slug":"gpt-6.1-sol","display_name":"GPT-6.1 Sol","visibility":"list",
         "supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}],
         "context_window":1050000},
        {"slug":"codex-auto-review","display_name":"Codex Auto Review","visibility":"hide"},
        {"slug":"gpt-5.6-sol","display_name":"GPT-5.6 Sol","visibility":"list",
         "supported_reasoning_levels":[{"effort":"none"},{"effort":"low"},{"effort":"max"}]},
        {"slug":"gpt-6.1-sol","display_name":"GPT-6.1 Sol","visibility":"list"}
    ]}"#;

    #[test]
    fn listing_keeps_visible_rows_and_drops_hidden_and_duplicates() {
        let models = parse_listing(LISTING).expect("listing parses");
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, ["gpt-6.1-sol", "gpt-5.6-sol"]);
        assert_eq!(models[0].name, "GPT-6.1 Sol");
        assert_eq!(models[0].efforts, ["low", "high"]);
        assert_eq!(models[0].context_window, Some(1_050_000));
    }

    #[test]
    fn merge_prefers_the_backend_ladder_and_preset_limits() {
        let live = vec![LiveModel {
            id: "gpt-5.6-sol".to_owned(),
            name: "GPT-5.6 Sol".to_owned(),
            efforts: vec!["low".to_owned(), "high".to_owned()],
            context_window: Some(999_000),
        }];
        let merged = merge(&live, &crate::catalog::openai_codex_preset().models);
        let row = merged
            .iter()
            .find(|model| model.id == "gpt-5.6-sol")
            .expect("merged row");
        assert_eq!(row.reasoning_efforts, ["low", "high"]);
        assert_eq!(row.context, 999_000);
    }

    #[test]
    fn attach_replaces_the_row_with_merged_models() {
        let mut document = CatalogDocument {
            providers: vec![CatalogProvider {
                id: "openai-codex".to_owned(),
                ..CatalogProvider::default()
            }],
        };
        let live = vec![LiveModel {
            id: "gpt-6.1-sol".to_owned(),
            ..LiveModel::default()
        }];
        attach(&mut document, Some(&live));
        let row = document.provider("openai-codex").expect("codex row");
        assert_eq!(row.models.len(), 1);
        assert_eq!(row.models[0].id, "gpt-6.1-sol");
        // Provider constants survive the replace.
        assert_eq!(row.kind, crate::catalog::KIND_OPENAI_RESPONSES);
    }
}
