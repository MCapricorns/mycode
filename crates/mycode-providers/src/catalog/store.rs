//! Catalog persistence and cloud refresh.
//!
//! Resolution order: a young cached document wins without network traffic;
//! otherwise one conditional GET revalidates the cloud document and rewrites
//! the cache; on any failure the cached or vendored snapshot keeps the
//! product fully functional offline.
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use mycode_config::{HomeLayout, locked_update_owned_file, read_owned_file};
use serde::{Deserialize, Serialize};

use super::modelsdev::{MODELS_DEV_API_URL, parse_models_dev};
use super::{CatalogDocument, parse_snapshot};

/// Cache path below the owned home.
pub const CATALOG_CACHE_PATH: &str = "catalog-cache.json";
/// Maximum encoded cache size.
pub const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;
/// Cache format version. Bumped to 3 when the snapshot gained models.dev
/// `reasoning_options` (toggle / effort lists), so stale caches fall back
/// to the bundled baseline instead of inventing low/medium/high.
pub const CACHE_FORMAT_VERSION: u32 = 3;
/// Cache kind tag.
pub const CACHE_KIND: &str = "mycode-providers-cache";
/// Maximum cloud document size accepted.
pub const MAX_CLOUD_BYTES: usize = 32 * 1024 * 1024;
/// Default auto-refresh age: the cache is revalidated at most this often.
pub const DEFAULT_MAX_AGE_SECS: u64 = 6 * 60 * 60;

/// A catalog document together with its cache metadata.
#[derive(Clone, Debug)]
pub struct CachedCatalog {
    /// The catalog document.
    pub document: CatalogDocument,
    /// Unix seconds when the cloud copy was last fetched.
    pub fetched_at: u64,
    /// Opaque ETag validator from the cloud copy.
    pub etag: Option<String>,
}

/// The result of one refresh attempt.
#[derive(Clone, Debug)]
pub enum RefreshOutcome {
    /// The cache was young enough; no network traffic happened.
    Fresh(CachedCatalog),
    /// The cloud document was unchanged (HTTP 304).
    NotModified(CachedCatalog),
    /// A new cloud document replaced the cache.
    Updated(CachedCatalog),
    /// The refresh failed; the caller keeps the previous catalog.
    Unavailable(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheHeader {
    format_version: u32,
    kind: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheDocument {
    format_version: u32,
    kind: String,
    fetched_at: u64,
    etag: Option<String>,
    document: CatalogDocument,
}

/// The vendored offline baseline, parsed once.
pub fn bundled() -> &'static CatalogDocument {
    static SNAPSHOT: OnceLock<CatalogDocument> = OnceLock::new();
    SNAPSHOT.get_or_init(|| parse_snapshot(include_bytes!("snapshot.json")))
}

/// Live or cached catalog installed by the process. Empty until startup
/// loads the cache or a refresh replaces it.
static ACTIVE: RwLock<Option<Arc<CatalogDocument>>> = RwLock::new(None);

/// Publishes the catalog turns should consult before [`bundled`].
pub fn install_active(document: Arc<CatalogDocument>) {
    if let Ok(mut slot) = ACTIVE.write() {
        *slot = Some(document);
    }
}

/// The installed live or cached catalog, when one has been published.
#[must_use]
pub fn active() -> Option<Arc<CatalogDocument>> {
    ACTIVE.read().ok().and_then(|slot| slot.clone())
}

/// Drops the installed catalog so lookups use [`bundled`] again.
#[cfg(test)]
pub(crate) fn clear_active() {
    if let Ok(mut slot) = ACTIVE.write() {
        *slot = None;
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// Reads the catalog cache and reports a quarantine when the file was damaged.
///
/// A missing file is `(None, None)`. A readable but unusable file is copied
/// aside, replaced with the bundled snapshot, and returned with that snapshot
/// so the next launch does not quarantine it again. I/O failures leave the
/// file alone and report no repair.
#[must_use]
fn load_cache_with_repair(
    home: &HomeLayout,
) -> (Option<CachedCatalog>, Option<mycode_config::DocumentRepair>) {
    let bytes = match read_owned_file(home, CATALOG_CACHE_PATH, MAX_CACHE_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) | Err(_) => return (None, None),
    };
    match parse_cache(bytes.as_slice()) {
        CacheParse::Ready(cache) => (Some(cache), None),
        CacheParse::Migrate => {
            let migrated = bundled_cache();
            let _ = write_cache(home, &migrated);
            (Some(migrated), None)
        }
        CacheParse::Damaged => {
            let repair =
                mycode_config::quarantine_owned_bytes(home, CATALOG_CACHE_PATH, bytes.as_slice())
                    .ok();
            if repair.is_some() {
                let _ = write_cache(home, &bundled_cache());
            }
            (repair.as_ref().map(|_| bundled_cache()), repair)
        }
    }
}

enum CacheParse {
    Ready(CachedCatalog),
    Migrate,
    Damaged,
}

fn bundled_cache() -> CachedCatalog {
    CachedCatalog {
        document: bundled().clone(),
        fetched_at: 0,
        etag: None,
    }
}

fn parse_cache(bytes: &[u8]) -> CacheParse {
    let Ok(header) = serde_json::from_slice::<CacheHeader>(bytes) else {
        return CacheParse::Damaged;
    };
    if header.kind != CACHE_KIND {
        return CacheParse::Damaged;
    }
    if header.format_version > 0 && header.format_version < CACHE_FORMAT_VERSION {
        return CacheParse::Migrate;
    }
    let Ok(document) = serde_json::from_slice::<CacheDocument>(bytes) else {
        return CacheParse::Damaged;
    };
    if document.format_version != CACHE_FORMAT_VERSION {
        return CacheParse::Damaged;
    }
    if document.etag.as_deref().is_some_and(|etag| {
        etag.is_empty() || etag.len() > 256 || etag.chars().any(char::is_control)
    }) {
        return CacheParse::Damaged;
    }
    let normalized = super::normalize(document.document.providers);
    if normalized.providers.is_empty() {
        return CacheParse::Damaged;
    }
    CacheParse::Ready(CachedCatalog {
        document: super::attach_subscription_presets(normalized),
        fetched_at: document.fetched_at,
        etag: document.etag,
    })
}

/// Resolves the current catalog: cache when valid, else the vendored snapshot.
#[must_use]
pub fn current(home: &HomeLayout) -> CachedCatalog {
    current_with_repair(home).0
}

/// Resolves the catalog and reports when a damaged cache was reset.
///
/// The in-memory catalog is always usable. Session data is not touched.
#[must_use]
pub fn current_with_repair(
    home: &HomeLayout,
) -> (CachedCatalog, Option<mycode_config::DocumentRepair>) {
    let (mut cache, repair) = match load_cache_with_repair(home) {
        (Some(cache), repair) => (cache, repair),
        (None, repair) => (bundled_cache(), repair),
    };
    super::opendesign::attach_stored(home, &mut cache.document);
    (cache, repair)
}

fn write_cache(home: &HomeLayout, cache: &CachedCatalog) -> Result<(), String> {
    let document = CacheDocument {
        format_version: CACHE_FORMAT_VERSION,
        kind: CACHE_KIND.to_owned(),
        fetched_at: cache.fetched_at,
        etag: cache.etag.clone(),
        document: cache.document.clone(),
    };
    let mut bytes = serde_json::to_vec(&document).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if bytes.len() > MAX_CACHE_BYTES {
        return Err("catalog cache exceeded its size bound".to_owned());
    }
    locked_update_owned_file(home, CATALOG_CACHE_PATH, MAX_CACHE_BYTES, |_| {
        Ok(bytes.clone())
    })
    .map_err(|error| error.to_string())
}

/// Runs a blocking catalog step off the async runtime: cache IO and the
/// multi-megabyte document parse/serialize would otherwise freeze every
/// queued command on the single-threaded core runtime.
async fn blocking<T: Send + 'static>(
    step: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(step)
        .await
        .map_err(|error| format!("catalog task failed: {error}"))?
}

async fn load_current(home: &HomeLayout) -> CachedCatalog {
    let home = home.clone();
    blocking(move || Ok(current(&home)))
        .await
        .unwrap_or_else(|_| bundled_cache())
}

async fn store_cache(home: &HomeLayout, cache: &CachedCatalog) -> Result<(), String> {
    let home = home.clone();
    let cache = cache.clone();
    blocking(move || write_cache(&home, &cache)).await
}

/// Whether a parsed cloud document carries any provider besides the presets
/// this build injects (`openai-codex`, OpenDesign).
fn has_cloud_providers(document: &CatalogDocument) -> bool {
    document.providers.iter().any(|provider| {
        provider.id != "openai-codex" && provider.id != super::opendesign::PROVIDER_ID
    })
}

/// Runs one refresh against the cloud catalog.
///
/// `force` bypasses the freshness window. Network failures and parse
/// failures surface as [`RefreshOutcome::Unavailable`]; the caller keeps
/// whatever catalog it already had.
pub async fn refresh(
    home: &HomeLayout,
    user_agent: &str,
    force: bool,
    max_age_secs: u64,
) -> RefreshOutcome {
    let existing = load_current(home).await;
    let age = unix_now().saturating_sub(existing.fetched_at);
    if existing.fetched_at > 0 && !force && age < max_age_secs {
        return RefreshOutcome::Fresh(existing);
    }

    let mut headers = vec![("accept".to_owned(), "application/json".to_owned())];
    if let Some(etag) = existing.etag.clone() {
        headers.push(("if-none-match".to_owned(), etag));
    }
    let response = match crate::send_pinned(crate::PinnedRequest {
        method: reqwest::Method::GET,
        url: MODELS_DEV_API_URL.to_owned(),
        headers,
        body: None,
        mode: crate::PinMode::CheckRedirect,
        timeout: Some(std::time::Duration::from_secs(30)),
        read_timeout: None,
        user_agent: Some(user_agent.to_owned()),
        follow_redirects: true,
        cancel: tokio_util::sync::CancellationToken::new(),
    })
    .await
    {
        Ok(response) => response,
        Err(error) => return RefreshOutcome::Unavailable(format!("catalog fetch failed: {error}")),
    };
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        let etag = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .or_else(|| existing.etag.clone());
        let refreshed = CachedCatalog {
            fetched_at: unix_now(),
            etag,
            ..existing
        };
        if let Err(message) = store_cache(home, &refreshed).await {
            return RefreshOutcome::Unavailable(message);
        }
        return RefreshOutcome::NotModified(refreshed);
    }
    if !response.status().is_success() {
        return RefreshOutcome::Unavailable(format!(
            "catalog endpoint returned {}",
            response.status()
        ));
    }
    let headers = response.headers().clone();
    let mut bytes = Vec::new();
    let mut response = response;
    loop {
        let chunk = match tokio::time::timeout(std::time::Duration::from_secs(30), response.chunk())
            .await
        {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(error)) => {
                return RefreshOutcome::Unavailable(format!("catalog read failed: {error}"));
            }
            Err(_) => {
                return RefreshOutcome::Unavailable("catalog read failed: timed out".to_owned());
            }
        };
        if bytes.len().saturating_add(chunk.len()) > MAX_CLOUD_BYTES {
            return RefreshOutcome::Unavailable("catalog document oversized".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    let parse_home = home.clone();
    let document = match blocking(move || {
        let mut document = parse_models_dev(&bytes);
        // Judge the cloud copy before this build's own rows join it: an
        // unparsable body (a captive portal's HTML page, say) must not
        // replace the cache with nothing but the injected presets.
        if !has_cloud_providers(&document) {
            return Err("catalog document had no usable providers".to_owned());
        }
        super::opendesign::attach_stored(&parse_home, &mut document);
        Ok(document)
    })
    .await
    {
        Ok(document) => document,
        Err(message) => return RefreshOutcome::Unavailable(message),
    };
    let etag = headers
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let refreshed = CachedCatalog {
        document,
        fetched_at: unix_now(),
        etag,
    };
    if let Err(message) = store_cache(home, &refreshed).await {
        return RefreshOutcome::Unavailable(message);
    }
    RefreshOutcome::Updated(refreshed)
}

#[cfg(test)]
mod tests {
    use super::{CACHE_FORMAT_VERSION, CACHE_KIND, CacheParse, has_cloud_providers, parse_cache};
    use serde_json::json;

    #[test]
    fn a_cloud_body_without_providers_is_not_a_catalog() {
        let html = super::parse_models_dev(b"<html>captive portal</html>");
        assert!(!has_cloud_providers(&html));
        let empty = super::parse_models_dev(b"{}");
        assert!(
            !has_cloud_providers(&empty),
            "the injected presets alone are not a catalog"
        );
        let snapshot = super::bundled();
        assert!(has_cloud_providers(snapshot));
    }

    #[test]
    fn parse_cache_normalizes_and_rejects_empty_or_invalid_provider_sets() {
        let empty = json!({
            "formatVersion": CACHE_FORMAT_VERSION,
            "kind": CACHE_KIND,
            "fetchedAt": 123,
            "etag": "\"v1\"",
            "document": { "providers": [] }
        });
        assert!(matches!(
            parse_cache(empty.to_string().as_bytes()),
            CacheParse::Damaged
        ));

        let invalid_only = json!({
            "formatVersion": CACHE_FORMAT_VERSION,
            "kind": CACHE_KIND,
            "fetchedAt": 123,
            "etag": "\"v1\"",
            "document": {
                "providers": [{
                    "id": "BAD ID",
                    "name": "Bad",
                    "kind": "openai-completions",
                    "baseUrl": "https://example.com/v1",
                    "models": [{ "id": "m1", "name": "m1" }]
                }]
            }
        });
        assert!(matches!(
            parse_cache(invalid_only.to_string().as_bytes()),
            CacheParse::Damaged
        ));

        let valid = json!({
            "formatVersion": CACHE_FORMAT_VERSION,
            "kind": CACHE_KIND,
            "fetchedAt": 123,
            "etag": "\"v1\"",
            "document": {
                "providers": [{
                    "id": "acme",
                    "name": "Acme",
                    "kind": "openai-completions",
                    "baseUrl": "https://api.acme.example/v1",
                    "models": [
                        { "id": "b-model", "name": "B", "reasoningEfforts": ["DEFAULT", "High", "bad token!"] },
                        { "id": "a-model", "name": "" },
                        { "id": "bad id", "name": " dropped" }
                    ]
                }]
            }
        });
        let CacheParse::Ready(cached) = parse_cache(valid.to_string().as_bytes()) else {
            panic!("expected valid cache");
        };
        let acme = cached.document.provider("acme").expect("acme");
        let ids: Vec<_> = acme.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a-model", "b-model"]);
        assert_eq!(acme.models[0].name, "a-model");
        assert_eq!(acme.models[1].reasoning_efforts, ["high"]);
        assert!(cached.document.provider("openai-codex").is_some());
    }
}
