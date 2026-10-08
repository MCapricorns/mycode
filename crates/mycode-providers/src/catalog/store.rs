//! Catalog persistence and cloud refresh.
//!
//! Resolution order: a young cached document wins without network traffic;
//! otherwise one conditional GET revalidates the cloud document and rewrites
//! the cache; on any failure the cached or vendored snapshot keeps the
//! product fully functional offline.
use std::sync::OnceLock;
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

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// Reads the cached catalog from the owned home, when present and valid.
///
/// An older copy of the same kind is replaced with the bundled snapshot so
/// newly added option fields are present, then refresh can update it. A
/// document that cannot be parsed is backed up and replaced; see
/// [`load_cache_with_repair`].
#[must_use]
pub fn load_cache(home: &HomeLayout) -> Option<CachedCatalog> {
    load_cache_with_repair(home).0
}

/// Reads the catalog cache and reports a quarantine when the file was damaged.
///
/// A missing file is `(None, None)`. A readable but unusable file is copied
/// aside, replaced with the bundled snapshot, and returned with that snapshot
/// so the next launch does not quarantine it again. I/O failures leave the
/// file alone and report no repair.
#[must_use]
pub fn load_cache_with_repair(
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
    if document.etag.as_ref().is_some_and(|etag| etag.len() > 256) {
        return CacheParse::Damaged;
    }
    CacheParse::Ready(CachedCatalog {
        document: document.document,
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
        .unwrap_or_else(|_| CachedCatalog {
            document: bundled().clone(),
            fetched_at: 0,
            etag: None,
        })
}

async fn store_cache(home: &HomeLayout, cache: &CachedCatalog) -> Result<(), String> {
    let home = home.clone();
    let cache = cache.clone();
    blocking(move || write_cache(&home, &cache)).await
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
        user_agent: Some(user_agent.to_owned()),
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
            .or(existing.etag.clone());
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
    let bytes = match response.bytes().await {
        Ok(bytes) if bytes.len() <= MAX_CLOUD_BYTES => bytes,
        Ok(_) => return RefreshOutcome::Unavailable("catalog document oversized".to_owned()),
        Err(error) => return RefreshOutcome::Unavailable(format!("catalog read failed: {error}")),
    };
    let document = match blocking(move || Ok(parse_models_dev(&bytes))).await {
        Ok(document) => document,
        Err(message) => return RefreshOutcome::Unavailable(message),
    };
    if document.providers.is_empty() {
        return RefreshOutcome::Unavailable("catalog document had no usable providers".to_owned());
    }
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

/// Builds the HTTP client used for catalog and update downloads.
///
/// # Errors
///
/// Returns the reqwest build error message.
pub fn http_client(user_agent: &str) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(user_agent.to_owned())
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| format!("http client unavailable: {error}"))
}
