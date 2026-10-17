//! Catalog persistence and cloud refresh.
//!
//! Resolution order: a young cached document wins without network traffic;
//! otherwise one conditional GET revalidates the cloud document and rewrites
//! the cache; on any failure the cached document or the hand-maintained
//! baseline (the Codex and OpenDesign rows) keeps the product usable.
use std::sync::{Arc, OnceLock, RwLock};

use mycode_config::{HomeLayout, locked_update_owned_file, read_owned_file};
use serde::{Deserialize, Serialize};

use super::CatalogDocument;
use super::modelsdev::{MODELS_DEV_API_URL, parse_models_dev};

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
/// Default auto-refresh age: the cache is revalidated at most once a day.
pub const DEFAULT_MAX_AGE_SECS: u64 = 24 * 60 * 60;

/// A catalog document together with its cache metadata.
///
/// The document is kept as raw bytes until something actually consults it:
/// startup reads and validates only the cache envelope, so the
/// multi-megabyte model parse is paid on the first `document()` call (a
/// model-menu open or the first turn), not on every launch.
#[derive(Clone)]
pub struct CachedCatalog {
    source: CatalogSource,
    /// Unix seconds when the cloud copy was last fetched.
    pub fetched_at: u64,
    /// Opaque ETag validator from the cloud copy.
    pub etag: Option<String>,
}

impl std::fmt::Debug for CachedCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedCatalog")
            .field("source", &self.source)
            .field("fetched_at", &self.fetched_at)
            .field("etag", &self.etag)
            .finish()
    }
}

#[derive(Clone)]
enum CatalogSource {
    /// A fully parsed document (a refresh result or a test fixture).
    Ready(Arc<CatalogDocument>),
    /// Raw bytes parsed on the first `document()` call.
    Lazy(Arc<LazyCatalog>),
}

impl std::fmt::Debug for CatalogSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready(document) => f
                .debug_tuple("Ready")
                .field(&document.providers.len())
                .finish(),
            Self::Lazy(lazy) => f
                .debug_struct("Lazy")
                .field("raw_bytes", &lazy.raw.len())
                .field("parsed", &lazy.ready.get().is_some())
                .finish(),
        }
    }
}

impl CatalogSource {
    fn document(&self) -> Arc<CatalogDocument> {
        match self {
            Self::Ready(document) => Arc::clone(document),
            Self::Lazy(lazy) => lazy.document(),
        }
    }
}

/// Raw cache bytes plus the materialized document slot.
struct LazyCatalog {
    /// The full cache file; the `document` field is pulled out of the
    /// envelope at materialize time.
    raw: Arc<[u8]>,
    /// The materialized document; parsing happens once, on first access.
    ready: OnceLock<Arc<CatalogDocument>>,
}

impl LazyCatalog {
    fn document(&self) -> Arc<CatalogDocument> {
        self.ready
            .get_or_init(|| {
                Arc::new(materialize_cache(&self.raw).unwrap_or_else(|| bundled().clone()))
            })
            .clone()
    }
}

/// The deferred half of [`parse_cache`]: the document field is decoded,
/// normalized, and checked for usable providers. Returns `None` when the
/// document is not a usable catalog; the caller falls back to the bundled
/// baseline.
fn materialize_cache(bytes: &[u8]) -> Option<CatalogDocument> {
    let document = serde_json::from_slice::<CacheDocument>(bytes).ok()?;
    if document.format_version != CACHE_FORMAT_VERSION || document.kind != CACHE_KIND {
        return None;
    }
    if document.etag.as_deref().is_some_and(|etag| {
        etag.is_empty() || etag.len() > 256 || etag.chars().any(char::is_control)
    }) {
        return None;
    }
    let normalized = super::normalize(document.document.providers);
    if normalized.providers.is_empty() {
        return None;
    }
    Some(super::attach_subscription_presets(normalized))
}

impl CachedCatalog {
    /// A parsed document wrapped as a cache entry (refresh results, tests).
    #[must_use]
    pub fn from_document(document: CatalogDocument, fetched_at: u64, etag: Option<String>) -> Self {
        Self {
            source: CatalogSource::Ready(Arc::new(document)),
            fetched_at,
            etag,
        }
    }

    /// A cache file kept raw until first use.
    fn lazy_file(raw: Vec<u8>, fetched_at: u64, etag: Option<String>) -> Self {
        Self {
            source: CatalogSource::Lazy(Arc::new(LazyCatalog {
                raw: Arc::from(raw),
                ready: OnceLock::new(),
            })),
            fetched_at,
            etag,
        }
    }

    /// The parsed document, materializing it on the first call.
    ///
    /// Parse failures fall back to the baseline rows so the returned
    /// document is never empty while presets exist.
    #[must_use]
    pub fn document(&self) -> Arc<CatalogDocument> {
        self.source.document()
    }
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

/// The cache envelope fields read eagerly at startup. `document` borrows the
/// raw JSON span so the model list itself is not decoded until the lazy
/// materialize runs. The whole file still has to scan as well-formed JSON,
/// so a truncated or corrupt cache is still caught here.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheEnvelope<'a> {
    format_version: u32,
    kind: String,
    fetched_at: u64,
    etag: Option<String>,
    // Never read: the field's only job is to make the scan cover the whole
    // document span, so malformed JSON inside it still fails the envelope.
    #[serde(borrow)]
    #[allow(dead_code)]
    document: &'a serde_json::value::RawValue,
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

/// The hand-maintained baseline compiled into the binary: the Codex
/// subscription and OpenDesign rows that models.dev does not carry.
/// Copilot and every other provider arrive from the cloud catalog into
/// the cache, so this is the offline floor, not the full catalog.
pub fn bundled() -> &'static CatalogDocument {
    static BASELINE: OnceLock<CatalogDocument> = OnceLock::new();
    BASELINE.get_or_init(|| super::attach_subscription_presets(CatalogDocument::default()))
}

/// Live or cached catalog installed by the process. Empty until startup
/// loads the cache or a refresh replaces it. The slot may still hold raw
/// bytes: `active()` materializes them on first use.
static ACTIVE: RwLock<Option<CatalogSource>> = RwLock::new(None);

/// Publishes the catalog turns should consult before [`bundled`].
pub fn install_active(document: Arc<CatalogDocument>) {
    if let Ok(mut slot) = ACTIVE.write() {
        *slot = Some(CatalogSource::Ready(document));
    }
}

/// Publishes a resolved cache entry without forcing its parse. A lazy
/// source still materializes on the first `active()` call.
pub fn install_active_cached(catalog: &CachedCatalog) {
    if let Ok(mut slot) = ACTIVE.write() {
        *slot = Some(catalog.source.clone());
    }
}

/// The installed live or cached catalog, when one has been published.
#[must_use]
pub fn active() -> Option<Arc<CatalogDocument>> {
    ACTIVE
        .read()
        .ok()
        .and_then(|slot| slot.as_ref().map(CatalogSource::document))
}

/// Drops the installed catalog so lookups use [`bundled`] again.
#[cfg(test)]
pub(crate) fn clear_active() {
    if let Ok(mut slot) = ACTIVE.write() {
        *slot = None;
    }
}

pub(crate) fn unix_now() -> u64 {
    mycode_core::unix_now().as_secs()
}

/// Reads the catalog cache and reports a quarantine when the file was damaged.
///
/// A missing file is `(None, None)`. A readable file whose envelope fails
/// validation is copied aside, replaced with the baseline rows, and
/// returned with those rows so the next launch does not quarantine it
/// again. I/O failures leave the file alone and report no repair.
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
    CachedCatalog::from_document(bundled().clone(), 0, None)
}

/// Validates the cache envelope only: kind, format version, etag grammar,
/// and — because the `document` field is borrowed as raw JSON — overall
/// well-formedness. The model list itself stays undecoded; a document that
/// is well-formed but unusable (empty or shapeless providers) is caught by
/// the lazy materialize, which falls back to the bundled baseline.
fn parse_cache(bytes: &[u8]) -> CacheParse {
    let Ok(envelope) = serde_json::from_slice::<CacheEnvelope>(bytes) else {
        return CacheParse::Damaged;
    };
    if envelope.kind != CACHE_KIND {
        return CacheParse::Damaged;
    }
    if envelope.format_version > 0 && envelope.format_version < CACHE_FORMAT_VERSION {
        return CacheParse::Migrate;
    }
    if envelope.format_version != CACHE_FORMAT_VERSION {
        return CacheParse::Damaged;
    }
    if envelope.etag.as_deref().is_some_and(|etag| {
        etag.is_empty() || etag.len() > 256 || etag.chars().any(char::is_control)
    }) {
        return CacheParse::Damaged;
    }
    CacheParse::Ready(CachedCatalog::lazy_file(
        bytes.to_vec(),
        envelope.fetched_at,
        envelope.etag,
    ))
}

/// Resolves the current catalog: cache when valid, else the baseline rows.
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
    match load_cache_with_repair(home) {
        (Some(cache), repair) => (cache, repair),
        (None, repair) => (bundled_cache(), repair),
    }
}

fn write_cache(home: &HomeLayout, cache: &CachedCatalog) -> Result<(), String> {
    let document = CacheDocument {
        format_version: CACHE_FORMAT_VERSION,
        kind: CACHE_KIND.to_owned(),
        fetched_at: cache.fetched_at,
        etag: cache.etag.clone(),
        document: cache.document().as_ref().clone(),
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
    let document = match blocking(move || {
        let document = parse_models_dev(&bytes);
        // Judge the cloud copy before this build's own rows join it: an
        // unparsable body (a captive portal's HTML page, say) must not
        // replace the cache with nothing but the injected presets.
        if !has_cloud_providers(&document) {
            return Err("catalog document had no usable providers".to_owned());
        }
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
    let refreshed = CachedCatalog::from_document(document, unix_now(), etag);
    if let Err(message) = store_cache(home, &refreshed).await {
        return RefreshOutcome::Unavailable(message);
    }
    RefreshOutcome::Updated(refreshed)
}

#[cfg(test)]
mod tests {
    use super::{
        CACHE_FORMAT_VERSION, CACHE_KIND, CacheParse, CatalogSource, has_cloud_providers,
        parse_cache,
    };
    use serde_json::json;

    fn test_home() -> (std::path::PathBuf, mycode_config::HomeLayout) {
        let root = std::env::temp_dir().join(format!(
            "mycode-catalog-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let home = mycode_config::HomeLayout::from_root(&root).expect("home");
        (root, home)
    }

    #[test]
    fn a_cloud_body_without_providers_is_not_a_catalog() {
        let html = super::parse_models_dev(b"<html>captive portal</html>");
        assert!(!has_cloud_providers(&html));
        let empty = super::parse_models_dev(b"{}");
        assert!(
            !has_cloud_providers(&empty),
            "the injected presets alone are not a catalog"
        );
        let baseline = super::bundled();
        assert!(
            !has_cloud_providers(baseline),
            "the baseline is only the injected presets"
        );
    }

    #[test]
    fn one_odd_models_dev_entry_does_not_empty_the_catalog() {
        let body = json!({
            "acme": {
                "npm": "@ai-sdk/openai-compatible",
                "name": "Acme",
                "api": "https://api.acme.example/v1",
                "doc": null,
                "models": {
                    "think-1": {
                        "name": "Think 1",
                        "reasoning": true,
                        "reasoning_options": [{"type": "effort", "values": [null, "low", "high"]}]
                    },
                    "broken": {"name": 42},
                    "plain": {"name": null}
                }
            },
            "garbage": "not a provider"
        });
        let document = super::parse_models_dev(body.to_string().as_bytes());
        assert!(has_cloud_providers(&document));
        let acme = document.provider("acme").expect("acme survives");
        let think = acme
            .models
            .iter()
            .find(|m| m.id == "think-1")
            .expect("think-1");
        assert_eq!(think.reasoning_efforts, ["low", "high"]);
        assert!(
            acme.models
                .iter()
                .any(|m| m.id == "plain" && m.name == "plain")
        );
        assert!(acme.models.iter().all(|m| m.id != "broken"));
    }

    #[test]
    fn parse_cache_reads_the_envelope_and_defers_the_document() {
        let malformed = "{not json";
        assert!(matches!(
            parse_cache(malformed.as_bytes()),
            CacheParse::Damaged
        ));
        let wrong_kind = json!({
            "formatVersion": CACHE_FORMAT_VERSION,
            "kind": "something-else",
            "fetchedAt": 123,
            "document": { "providers": [] }
        });
        assert!(matches!(
            parse_cache(wrong_kind.to_string().as_bytes()),
            CacheParse::Damaged
        ));

        // Well-formed but unusable provider sets stay `Ready`: the lazy
        // materialize — not the envelope read — falls back to the bundled
        // baseline.
        for document in [
            json!({ "providers": [] }),
            json!({
                "providers": [{
                    "id": "BAD ID",
                    "name": "Bad",
                    "kind": "openai-completions",
                    "baseUrl": "https://example.com/v1",
                    "models": [{ "id": "m1", "name": "m1" }]
                }]
            }),
        ] {
            let file = json!({
                "formatVersion": CACHE_FORMAT_VERSION,
                "kind": CACHE_KIND,
                "fetchedAt": 123,
                "document": document
            });
            let CacheParse::Ready(cached) = parse_cache(file.to_string().as_bytes()) else {
                panic!("expected envelope-ready cache");
            };
            let CatalogSource::Lazy(lazy) = &cached.source else {
                panic!("the document must stay unparsed");
            };
            assert!(lazy.ready.get().is_none());
            let document = cached.document();
            assert!(
                !document.providers.is_empty(),
                "an unusable document falls back to the bundled baseline"
            );
            assert!(document.provider("acme").is_none());
        }
    }

    #[test]
    fn parse_cache_normalizes_valid_providers_on_first_access() {
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
        assert_eq!(cached.fetched_at, 123);
        assert_eq!(cached.etag.as_deref(), Some("\"v1\""));
        let CatalogSource::Lazy(lazy) = &cached.source else {
            panic!("the document must stay unparsed");
        };
        assert!(lazy.ready.get().is_none(), "no document work at read");
        let document = cached.document();
        assert!(lazy.ready.get().is_some(), "first access materializes");
        let acme = document.provider("acme").expect("acme");
        let ids: Vec<_> = acme.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a-model", "b-model"]);
        assert_eq!(acme.models[0].name, "a-model");
        assert_eq!(acme.models[1].reasoning_efforts, ["high"]);
        assert!(document.provider("openai-codex").is_some());
    }

    #[test]
    fn current_stays_lazy_and_materializes_once() {
        let (root, home) = test_home();
        let file = json!({
            "formatVersion": CACHE_FORMAT_VERSION,
            "kind": CACHE_KIND,
            "fetchedAt": 42,
            "etag": null,
            "document": {
                "providers": [{
                    "id": "acme",
                    "name": "Acme",
                    "kind": "openai-completions",
                    "baseUrl": "https://api.acme.example/v1",
                    "models": [{ "id": "m1", "name": "M1" }]
                }]
            }
        });
        let _ = root;
        let bytes = file.to_string().into_bytes();
        mycode_config::locked_update_owned_file(
            &home,
            super::CATALOG_CACHE_PATH,
            super::MAX_CACHE_BYTES,
            |_| Ok(bytes.clone()),
        )
        .unwrap();
        let (cached, repair) = super::current_with_repair(&home);
        assert!(repair.is_none());
        let CatalogSource::Lazy(lazy) = &cached.source else {
            panic!("the cache must resolve without parsing its document");
        };
        assert!(lazy.ready.get().is_none());
        let first = cached.document();
        let second = cached.document();
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "one parse, one doc"
        );
        assert_eq!(cached.fetched_at, 42);
        assert!(first.provider("acme").is_some());
    }
}
