//! First-party bounded web search over a configured backend family.
//!
//! Querit uses `POST {endpoint}/v1/search` plus
//! `POST {endpoint}/v1/contents`. AnySearch uses `POST {endpoint}/v1/search`
//! with `{query, max_results}` answering `{data.results}` and
//! `POST {endpoint}/v1/extract` with `{url}` for page text. Exa uses
//! `POST {endpoint}/search` answering `{results}` (highlights requested for
//! snippets) and `POST {endpoint}/contents` with `{urls, text}` answering
//! `{results, statuses}` for page text. Everything is bounded: result count,
//! URL count, response bytes, and aggregate payload bytes. The transport is
//! injectable so hosts can substitute their own; the production
//! [`transport::ReqwestWebTransport`] ships with the crate.

pub mod guard;
pub mod transport;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// One ranked search hit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchResult {
    /// Page URL. Hits that fail [`guard::is_fetchable_url`] are dropped.
    pub url: String,
    pub title: String,
    pub snippet: String,
}

/// One fetched page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageContent {
    pub url: String,
    pub content: String,
    /// Set when the extractor or [`bound_page`] cut the body.
    pub truncated: bool,
}

/// Errors surfaced by the web client.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebError {
    /// The request or a response URL violated the guard rules.
    #[error("blocked by URL policy")]
    Blocked,
    /// The backend response violated the bounded contract.
    #[error("backend response is malformed or oversized")]
    Protocol,
    /// The backend could not be reached. The string is the visible reason
    /// (pin rejection, DNS, TLS, or connect failure) when one is known.
    #[error("{0}")]
    Unavailable(String),
    /// The caller cancelled the request.
    #[error("cancelled")]
    Cancelled,
}

impl WebError {
    /// A reachability failure. An empty reason keeps the generic sentence.
    #[must_use]
    pub(crate) fn unavailable(reason: impl AsRef<str>) -> Self {
        let reason = reason.as_ref().trim();
        if reason.is_empty() {
            Self::Unavailable("search backend is unavailable".to_owned())
        } else {
            Self::Unavailable(format!("search backend is unavailable: {reason}"))
        }
    }
}

/// Text `web_search` / `fetch_content` return when the client fails.
///
/// `action` is `search` or `fetch`, matching the tool the model called.
/// A permanent extract failure tells the model not to sleep-retry that URL.
#[must_use]
pub(crate) fn tool_failure(action: &str, error: &WebError) -> String {
    let mut text = format!("{action} failed: {error}");
    if action == "fetch" && permanent_extract_failure(&text) {
        text.push(' ');
        text.push_str(PERMANENT_EXTRACT_NOTE);
    }
    text
}

/// Shown when AnySearch cannot extract a page. Retrying the same URL does not help.
const PERMANENT_EXTRACT_NOTE: &str = "Permanent failure for this URL: do not sleep-retry; try a different URL or continue without this page.";

fn permanent_extract_failure(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("extract_failed")
        || lower.contains("unable to extract")
        || http_status_between(&lower, 400, 499)
}

fn http_status_between(lower: &str, start: u16, end: u16) -> bool {
    let mut rest = lower;
    while let Some(index) = rest.find("http ") {
        let after = &rest[index + 5..];
        let digits: String = after.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        if let Ok(code) = digits.parse::<u16>()
            && (start..=end).contains(&code)
        {
            return true;
        }
        let skip = digits.len();
        if skip >= after.len() {
            break;
        }
        rest = &after[skip..];
    }
    false
}

fn permanent_fetch_page(url: &str, error: &WebError) -> Option<PageContent> {
    let mut content = tool_failure("fetch", error);
    if !permanent_extract_failure(&content) {
        return None;
    }
    if !content.contains("do not sleep-retry") {
        content.push(' ');
        content.push_str(PERMANENT_EXTRACT_NOTE);
    }
    Some(PageContent {
        url: url.to_owned(),
        content,
        truncated: false,
    })
}

/// `error_code` and `message` from an AnySearch error object, as one line.
///
/// Missing or blank fields are omitted. Whitespace is collapsed and each
/// field is capped so the tool error stays a single short sentence.
fn anysearch_error_detail(payload: &serde_json::Value) -> Option<String> {
    let error_code = json_text(payload, "error_code", 80);
    let message = json_text(payload, "message", 240);
    match (error_code, message) {
        (Some(code), Some(message)) => Some(format!("{code}: {message}")),
        (Some(code), None) => Some(code),
        (None, Some(message)) => Some(message),
        (None, None) => None,
    }
}

fn json_text(payload: &serde_json::Value, key: &str, max_chars: usize) -> Option<String> {
    let raw = payload.get(key)?.as_str()?;
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    let mut chars = flat.chars();
    let mut text: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        text.push('…');
    }
    Some(text)
}

/// AnySearch answers `code != 0` for an application failure, including on HTTP 200.
fn anysearch_rejected(payload: &serde_json::Value) -> Option<WebError> {
    let failed = payload
        .get("code")
        .and_then(serde_json::Value::as_i64)
        .is_some_and(|code| code != 0);
    if !failed {
        return None;
    }
    Some(WebError::unavailable(
        anysearch_error_detail(payload).unwrap_or_default(),
    ))
}

/// Outbound POST seam for the web client.
///
/// The method returns a boxed future so the trait stays object-safe for
/// `dyn WebTransport` dispatch; implementers write `Box::pin(async move { … })`.
pub trait WebTransport: Send + Sync + 'static {
    /// Posts a JSON body with an optional bearer key and returns the raw
    /// response bytes.
    ///
    /// # Errors
    ///
    /// Returns [`WebError`] for transport-level failures.
    fn post_json<'a>(
        &'a self,
        endpoint: &'a str,
        bearer: Option<&'a str>,
        body: &'a [u8],
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, WebError>> + Send + 'a>>;
}

/// Backend family that selects the request and response wire mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    /// Querit `POST /v1/search` + `POST /v1/contents`.
    Querit,
    /// AnySearch `POST /v1/search` + `POST /v1/extract`.
    Anysearch,
    /// Exa `POST /search` + `POST /contents`. Bearer or `x-api-key`; the
    /// transport already sends Bearer.
    Exa,
}

impl SearchKind {
    /// Parses a settings `kind` spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "querit" => Some(Self::Querit),
            "anysearch" => Some(Self::Anysearch),
            "exa" => Some(Self::Exa),
            _ => None,
        }
    }
}

/// Client bound to one backend endpoint.
#[derive(Clone)]
pub struct WebClient {
    endpoint: String,
    bearer: Option<String>,
    transport: Arc<dyn WebTransport>,
    timeout: Duration,
    kind: SearchKind,
}

impl WebClient {
    /// Creates one client over an https backend endpoint from settings.
    /// `bearer` rides every request as the Authorization header.
    ///
    /// # Errors
    ///
    /// Returns [`WebError::Blocked`] when the endpoint itself violates the
    /// URL policy.
    pub fn new(
        endpoint: &str,
        bearer: Option<String>,
        transport: Arc<dyn WebTransport>,
    ) -> Result<Self, WebError> {
        if !guard::is_fetchable_url(endpoint) {
            return Err(WebError::Blocked);
        }
        Ok(Self {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            bearer: bearer.filter(|key| !key.trim().is_empty()),
            transport,
            timeout: Duration::from_secs(guard::DEFAULT_TIMEOUT_SECS),
            kind: SearchKind::Querit,
        })
    }

    /// Selects the wire mapping for this backend family.
    #[must_use]
    pub fn with_kind(mut self, kind: SearchKind) -> Self {
        self.kind = kind;
        self
    }

    /// Runs one bounded search.
    ///
    /// Wire contract (Querit): `POST /v1/search` with `{"query", "count"}`
    /// answering `{"results": {"result": [...]}}`.
    ///
    /// # Errors
    ///
    /// Returns [`WebError`] for guard, transport, or contract violations.
    pub async fn search(
        &self,
        query: &str,
        max_results: usize,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchResult>, WebError> {
        if query.trim().is_empty() {
            return Err(WebError::Blocked);
        }
        let max_results = max_results.clamp(1, guard::MAX_SEARCH_RESULTS);
        match self.kind {
            SearchKind::Anysearch => self.search_anysearch(query, max_results, cancel).await,
            SearchKind::Exa => self.search_exa(query, max_results, cancel).await,
            SearchKind::Querit => self.search_querit(query, max_results, cancel).await,
        }
    }

    async fn search_querit(
        &self,
        query: &str,
        max_results: usize,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchResult>, WebError> {
        let body = serde_json::to_vec(&serde_json::json!({
            "query": query,
            "count": max_results,
        }))
        .map_err(|_| WebError::Protocol)?;
        let raw = self.post("/v1/search", &body, self.timeout, cancel).await?;
        let payload: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
        let hits = payload["results"]["result"]
            .as_array()
            .ok_or(WebError::Protocol)?;
        self.collect_hits(hits, max_results, |hit| {
            (
                hit["url"].as_str(),
                hit["title"].as_str(),
                hit["snippet"].as_str(),
            )
        })
    }

    async fn search_anysearch(
        &self,
        query: &str,
        max_results: usize,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchResult>, WebError> {
        let body = serde_json::to_vec(&serde_json::json!({
            "query": query,
            "max_results": max_results,
        }))
        .map_err(|_| WebError::Protocol)?;
        let raw = self.post("/v1/search", &body, self.timeout, cancel).await?;
        let payload: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
        if let Some(error) = anysearch_rejected(&payload) {
            return Err(error);
        }
        let hits = payload["data"]["results"]
            .as_array()
            .ok_or(WebError::Protocol)?;
        self.collect_hits(hits, max_results, |hit| {
            (
                hit["url"].as_str(),
                hit["title"].as_str(),
                hit["snippet"].as_str().or_else(|| hit["content"].as_str()),
            )
        })
    }

    /// Exa `POST /search`. `highlights` buys one short excerpt per hit so the
    /// tool's snippet slot is filled without a second contents call.
    async fn search_exa(
        &self,
        query: &str,
        max_results: usize,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchResult>, WebError> {
        let body = serde_json::to_vec(&serde_json::json!({
            "query": query,
            "numResults": max_results,
            "type": "auto",
            "contents": {
                "highlights": { "numSentences": 2, "highlightsPerUrl": 1 },
            },
        }))
        .map_err(|_| WebError::Protocol)?;
        let raw = self.post("/search", &body, self.timeout, cancel).await?;
        let payload: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
        let hits = payload["results"].as_array().ok_or(WebError::Protocol)?;
        self.collect_hits(hits, max_results, |hit| {
            let snippet = hit["highlights"]
                .as_array()
                .and_then(|highlights| highlights.iter().find_map(|line| line.as_str()))
                .or_else(|| hit["text"].as_str())
                .or_else(|| hit["summary"].as_str());
            (hit["url"].as_str(), hit["title"].as_str(), snippet)
        })
    }

    fn collect_hits(
        &self,
        hits: &[serde_json::Value],
        max_results: usize,
        fields: impl Fn(&serde_json::Value) -> (Option<&str>, Option<&str>, Option<&str>),
    ) -> Result<Vec<SearchResult>, WebError> {
        let results: Vec<SearchResult> = hits
            .iter()
            .filter_map(|hit| {
                let (url, title, snippet) = fields(hit);
                let url = url?;
                Some(SearchResult {
                    url: url.to_owned(),
                    title: title.unwrap_or(url).to_owned(),
                    snippet: snippet.unwrap_or_default().to_owned(),
                })
            })
            // Drop blocked hits before counting, so one unfetchable top hit
            // does not cost the model a usable result further down.
            .filter(|result| guard::is_fetchable_url(&result.url))
            .take(max_results)
            .map(|mut result| {
                result.title = guard::sanitize_remote_text(&result.title);
                result.snippet = guard::sanitize_remote_text(&result.snippet);
                result
            })
            .collect();
        Ok(results)
    }

    /// Fetches bounded page text for the given URLs.
    ///
    /// Wire contract (Querit): `POST /v1/contents` with
    /// `{"urls", "format": "text", "crawlTimeout", "extrasMeta"}` answering
    /// `{"results": [{url, content}]}`. `crawlTimeout` is seconds in
    /// `1..=60`.
    ///
    /// # Errors
    ///
    /// Returns [`WebError`] for guard, transport, or contract violations.
    pub async fn contents(
        &self,
        urls: &[String],
        cancel: CancellationToken,
    ) -> Result<Vec<PageContent>, WebError> {
        if urls.is_empty() || urls.len() > guard::MAX_CONTENTS_URLS {
            return Err(WebError::Blocked);
        }
        for url in urls {
            if !guard::is_fetchable_url(url) {
                return Err(WebError::Blocked);
            }
        }
        match self.kind {
            SearchKind::Anysearch => self.contents_anysearch(urls, cancel).await,
            SearchKind::Exa => self.contents_exa(urls, cancel).await,
            SearchKind::Querit => self.contents_querit(urls, cancel).await,
        }
    }

    async fn contents_querit(
        &self,
        urls: &[String],
        cancel: CancellationToken,
    ) -> Result<Vec<PageContent>, WebError> {
        let body = serde_json::to_vec(&serde_json::json!({
            "urls": urls,
            "format": "text",
            "crawlTimeout": guard::CRAWL_TIMEOUT_SECS,
            "extrasMeta": false,
        }))
        .map_err(|_| WebError::Protocol)?;
        let raw = match self
            .post(
                "/v1/contents",
                &body,
                Duration::from_secs(guard::CONTENTS_TIMEOUT_SECS),
                cancel,
            )
            .await
        {
            Ok(raw) => raw,
            Err(error) => {
                let pages: Vec<_> = urls
                    .iter()
                    .filter_map(|url| permanent_fetch_page(url, &error))
                    .collect();
                if pages.len() == urls.len() {
                    return Ok(pages);
                }
                return Err(error);
            }
        };
        let payload: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
        let pages_wire = payload["results"].as_array().ok_or(WebError::Protocol)?;
        self.collect_pages(urls, pages_wire)
    }

    async fn contents_anysearch(
        &self,
        urls: &[String],
        cancel: CancellationToken,
    ) -> Result<Vec<PageContent>, WebError> {
        let mut pages = Vec::new();
        let mut total = 0usize;
        for url in urls {
            let body = serde_json::to_vec(&serde_json::json!({ "url": url }))
                .map_err(|_| WebError::Protocol)?;
            let raw = match self
                .post(
                    "/v1/extract",
                    &body,
                    Duration::from_secs(guard::CONTENTS_TIMEOUT_SECS),
                    cancel.clone(),
                )
                .await
            {
                Ok(raw) => raw,
                Err(error) => {
                    if let Some(page) = permanent_fetch_page(url, &error) {
                        pages.push(page);
                        continue;
                    }
                    return Err(error);
                }
            };
            let payload: serde_json::Value =
                serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
            if let Some(error) = anysearch_rejected(&payload) {
                if let Some(page) = permanent_fetch_page(url, &error) {
                    pages.push(page);
                    continue;
                }
                return Err(error);
            }
            let data = &payload["data"];
            let returned = data["url"].as_str().unwrap_or(url);
            if returned != url {
                return Err(WebError::Protocol);
            }
            let content = guard::sanitize_remote_text(data["content"].as_str().unwrap_or_default());
            let page = PageContent {
                url: url.clone(),
                content,
                truncated: false,
            };
            let (page, stop) = bound_page(page, &mut total);
            pages.push(page);
            if stop {
                return Ok(pages);
            }
        }
        Ok(pages)
    }

    /// Exa `POST /contents`. Per-URL failures arrive in `statuses` and get a
    /// failure page in place, so one bad link does not sink the batch.
    async fn contents_exa(
        &self,
        urls: &[String],
        cancel: CancellationToken,
    ) -> Result<Vec<PageContent>, WebError> {
        let body = serde_json::to_vec(&serde_json::json!({
            "urls": urls,
            // One past the per-page cap so a hit sitting exactly on it is
            // still reported as truncated.
            "text": { "maxCharacters": guard::MAX_PAGE_BYTES + 1 },
        }))
        .map_err(|_| WebError::Protocol)?;
        let raw = self
            .post(
                "/contents",
                &body,
                Duration::from_secs(guard::CONTENTS_TIMEOUT_SECS),
                cancel,
            )
            .await?;
        let payload: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
        let results = payload["results"].as_array().ok_or(WebError::Protocol)?;
        let statuses = payload["statuses"].as_array();
        let mut total = 0usize;
        let mut pages = Vec::new();
        for url in urls {
            let wire = results.iter().find(|wire| {
                wire["id"].as_str() == Some(url.as_str())
                    || wire["url"].as_str() == Some(url.as_str())
            });
            let page = match wire {
                Some(wire) => PageContent {
                    url: url.clone(),
                    content: guard::sanitize_remote_text(wire["text"].as_str().unwrap_or_default()),
                    truncated: false,
                },
                None => {
                    let tag = statuses
                        .and_then(|statuses| {
                            statuses.iter().find(|status| {
                                status["id"].as_str() == Some(url.as_str())
                                    && status["status"].as_str() == Some("error")
                            })
                        })
                        .and_then(|status| status["error"]["tag"].as_str())
                        .unwrap_or("no content returned");
                    PageContent {
                        url: url.clone(),
                        content: format!("fetch failed: {tag}"),
                        truncated: false,
                    }
                }
            };
            let (page, stop) = bound_page(page, &mut total);
            pages.push(page);
            if stop {
                return Ok(pages);
            }
        }
        Ok(pages)
    }

    fn collect_pages(
        &self,
        urls: &[String],
        pages_wire: &[serde_json::Value],
    ) -> Result<Vec<PageContent>, WebError> {
        let allowed: std::collections::HashSet<&String> = urls.iter().collect();
        let mut total = 0usize;
        let mut pages = Vec::new();
        for wire in pages_wire {
            let url = wire["url"].as_str().ok_or(WebError::Protocol)?.to_owned();
            if !allowed.contains(&url) {
                return Err(WebError::Protocol);
            }
            let content = guard::sanitize_remote_text(wire["content"].as_str().unwrap_or_default());
            let page = PageContent {
                url,
                content,
                truncated: wire["truncated"].as_bool().unwrap_or(false),
            };
            let (page, stop) = bound_page(page, &mut total);
            pages.push(page);
            if stop {
                return Ok(pages);
            }
        }
        Ok(pages)
    }

    async fn post(
        &self,
        path: &str,
        body: &[u8],
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<Vec<u8>, WebError> {
        let endpoint = format!("{}{path}", self.endpoint);
        let raw = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(WebError::Cancelled),
            raw = self
                .transport
                .post_json(&endpoint, self.bearer.as_deref(), body, timeout, cancel.clone()) => raw?,
        };
        if raw.len() > guard::MAX_RESPONSE_BYTES {
            return Err(WebError::Protocol);
        }
        Ok(raw)
    }
}

/// Applies the per-page character cap, then the aggregate byte budget.
///
/// `true` means this page filled the budget and the caller should stop.
fn bound_page(mut page: PageContent, total: &mut usize) -> (PageContent, bool) {
    if page.content.chars().count() > guard::MAX_PAGE_BYTES {
        page.content = page.content.chars().take(guard::MAX_PAGE_BYTES).collect();
        page.truncated = true;
    }
    *total += page.content.len();
    if *total > guard::MAX_RESPONSE_BYTES {
        page.truncated = true;
        return (page, true);
    }
    (page, false)
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use mycode_providers::{PinMode, connection_addresses};
    use tokio_util::sync::CancellationToken;

    use super::transport::{http_error_reason, pinned_transport_error};
    use super::{SearchKind, WebClient, WebError, WebTransport, tool_failure};

    struct StaticFailure {
        reason: String,
    }

    impl WebTransport for StaticFailure {
        fn post_json<'a>(
            &'a self,
            _endpoint: &'a str,
            _bearer: Option<&'a str>,
            _body: &'a [u8],
            _timeout: Duration,
            _cancel: CancellationToken,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<u8>, WebError>> + Send + 'a>,
        > {
            Box::pin(async move { Err(pinned_transport_error(&self.reason)) })
        }
    }

    #[tokio::test]
    async fn pin_rejection_reaches_the_web_tool_error() {
        let reason = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://api.anysearch.com/v1/search",
            &[IpAddr::from([127, 0, 0, 1])],
        )
        .unwrap_err();
        assert_eq!(
            reason,
            "resolved addresses are not all public: api.anysearch.com"
        );
        let client = WebClient::new(
            "https://api.anysearch.com",
            None,
            Arc::new(StaticFailure { reason }),
        )
        .unwrap()
        .with_kind(SearchKind::Anysearch);
        let search_error = client
            .search("rust fake-ip", 3, CancellationToken::new())
            .await
            .unwrap_err();
        let search_tool = mycode_tools::ToolError::Execution(tool_failure("search", &search_error));
        assert!(
            search_tool
                .to_string()
                .contains("resolved addresses are not all public: api.anysearch.com"),
            "{search_tool}"
        );
        let fetch_error = client
            .contents(
                &["https://example.com/docs".to_owned()],
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        let fetch_tool = mycode_tools::ToolError::Execution(tool_failure("fetch", &fetch_error));
        assert!(
            fetch_tool
                .to_string()
                .contains("resolved addresses are not all public: api.anysearch.com"),
            "{fetch_tool}"
        );
    }

    struct StaticJson {
        body: Vec<u8>,
    }

    impl WebTransport for StaticJson {
        fn post_json<'a>(
            &'a self,
            _endpoint: &'a str,
            _bearer: Option<&'a str>,
            _body: &'a [u8],
            _timeout: Duration,
            _cancel: CancellationToken,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<u8>, WebError>> + Send + 'a>,
        > {
            Box::pin(async move { Ok(self.body.clone()) })
        }
    }

    #[tokio::test]
    async fn blocked_hits_do_not_use_up_the_result_count() {
        let body = serde_json::json!({
            "results": [
                {"url": "http://insecure.example/a", "title": "blocked", "snippet": ""},
                {"url": "https://example.com/one", "title": "one", "snippet": ""},
                {"url": "https://example.com/two", "title": "two", "snippet": ""},
                {"url": "https://example.com/three", "title": "three", "snippet": ""}
            ]
        });
        let client = WebClient::new(
            "https://api.exa.ai",
            None,
            Arc::new(StaticJson {
                body: serde_json::to_vec(&body).unwrap(),
            }),
        )
        .unwrap()
        .with_kind(SearchKind::Exa);
        let hits = client
            .search("query", 2, CancellationToken::new())
            .await
            .unwrap();
        let titles: Vec<_> = hits.iter().map(|hit| hit.title.as_str()).collect();
        assert_eq!(titles, ["one", "two"]);
    }

    #[tokio::test]
    async fn exa_search_maps_highlights_into_snippets() {
        let body = serde_json::json!({
            "results": [
                {
                    "id": "https://example.com/one",
                    "url": "https://example.com/one",
                    "title": "one",
                    "highlights": ["first excerpt"]
                },
                {
                    "url": "https://example.com/two",
                    "text": "body text"
                },
                {"url": "http://insecure.example/x", "title": "blocked"}
            ]
        });
        let client = WebClient::new(
            "https://api.exa.ai",
            Some("key".to_owned()),
            Arc::new(StaticJson {
                body: serde_json::to_vec(&body).unwrap(),
            }),
        )
        .unwrap()
        .with_kind(SearchKind::Exa);
        let hits = client
            .search("query", 8, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://example.com/one");
        assert_eq!(hits[0].snippet, "first excerpt");
        // A missing title falls back to the URL; `text` fills the snippet.
        assert_eq!(hits[1].title, "https://example.com/two");
        assert_eq!(hits[1].snippet, "body text");
    }

    #[tokio::test]
    async fn exa_contents_report_per_url_failures_in_place() {
        let body = serde_json::json!({
            "results": [
                {"id": "https://example.com/ok", "text": "page body"}
            ],
            "statuses": [
                {"id": "https://example.com/ok", "status": "success"},
                {
                    "id": "https://example.com/bad",
                    "status": "error",
                    "error": {"tag": "CRAWL_NOT_FOUND", "httpStatusCode": 404}
                }
            ]
        });
        let client = WebClient::new(
            "https://api.exa.ai",
            Some("key".to_owned()),
            Arc::new(StaticJson {
                body: serde_json::to_vec(&body).unwrap(),
            }),
        )
        .unwrap()
        .with_kind(SearchKind::Exa);
        let pages = client
            .contents(
                &[
                    "https://example.com/ok".to_owned(),
                    "https://example.com/bad".to_owned(),
                ],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].content, "page body");
        assert!(
            pages[1].content.contains("CRAWL_NOT_FOUND"),
            "{}",
            pages[1].content
        );
    }

    #[test]
    fn search_kind_parses_each_settings_spelling() {
        assert_eq!(SearchKind::parse("querit"), Some(SearchKind::Querit));
        assert_eq!(SearchKind::parse("anysearch"), Some(SearchKind::Anysearch));
        assert_eq!(SearchKind::parse("exa"), Some(SearchKind::Exa));
        assert_eq!(SearchKind::parse("custom"), None);
        assert_eq!(SearchKind::parse("tavily"), None);
    }

    const EXTRACT_FAILED: &[u8] = br#"{"code":-1,"error_code":"extract_failed","message":"Unable to extract content from the URL."}"#;

    #[test]
    fn anysearch_422_body_reaches_the_fetch_tool_error() {
        let reason = http_error_reason(reqwest::StatusCode::UNPROCESSABLE_ENTITY, EXTRACT_FAILED);
        assert_eq!(
            reason,
            "HTTP 422 Unprocessable Entity (extract_failed: Unable to extract content from the URL.)"
        );
        let error = WebError::unavailable(reason);
        assert_eq!(
            tool_failure("fetch", &error),
            "fetch failed: search backend is unavailable: HTTP 422 Unprocessable Entity (extract_failed: Unable to extract content from the URL.) Permanent failure for this URL: do not sleep-retry; try a different URL or continue without this page."
        );
        let search = tool_failure("search", &error);
        assert!(
            search.contains("HTTP 422") && search.contains("extract_failed"),
            "{search}"
        );
        assert!(
            search.contains("Unable to extract content from the URL."),
            "{search}"
        );
        assert!(
            !search.contains("sleep-retry"),
            "search keeps the body without the extract retry note: {search}"
        );
    }

    #[test]
    fn http_error_body_keeps_status_when_json_is_partial_or_absent() {
        let html = http_error_reason(reqwest::StatusCode::BAD_GATEWAY, b"<html>nope</html>");
        assert_eq!(html, "HTTP 502 Bad Gateway");
        assert!(!tool_failure("fetch", &WebError::unavailable(html)).contains("sleep-retry"));

        let code_only = http_error_reason(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            br#"{"code":-1,"error_code":"extract_failed"}"#,
        );
        assert_eq!(code_only, "HTTP 422 Unprocessable Entity (extract_failed)");

        let message_only = http_error_reason(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            br#"{"message":"Unable to extract content from the URL."}"#,
        );
        assert_eq!(
            message_only,
            "HTTP 422 Unprocessable Entity (Unable to extract content from the URL.)"
        );

        let blank = http_error_reason(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            br#"{"error_code":"  ","message":""}"#,
        );
        assert_eq!(blank, "HTTP 422 Unprocessable Entity");

        let wrapped = http_error_reason(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            br#"{"error_code":"backend_down","message":"try later"}"#,
        );
        assert_eq!(
            wrapped,
            "HTTP 503 Service Unavailable (backend_down: try later)"
        );
        let collapsed = http_error_reason(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            br#"{"error_code":"extract_failed","message":"Unable to extract\ncontent from the URL."}"#,
        );
        assert_eq!(
            collapsed,
            "HTTP 422 Unprocessable Entity (extract_failed: Unable to extract content from the URL.)"
        );
    }

    #[tokio::test]
    async fn anysearch_error_object_reaches_search_and_fetch_tool_errors() {
        let client = WebClient::new(
            "https://api.anysearch.com",
            None,
            Arc::new(StaticJson {
                body: EXTRACT_FAILED.to_vec(),
            }),
        )
        .unwrap()
        .with_kind(SearchKind::Anysearch);
        let search_error = client
            .search("rust extract", 3, CancellationToken::new())
            .await
            .unwrap_err();
        let search_tool = tool_failure("search", &search_error);
        assert!(
            search_tool.contains("extract_failed")
                && search_tool.contains("Unable to extract content from the URL."),
            "{search_tool}"
        );
        assert!(!search_tool.contains("sleep-retry"), "{search_tool}");
        let pages = client
            .contents(
                &["https://example.com/docs".to_owned()],
                CancellationToken::new(),
            )
            .await
            .expect("extract_failed is page text, not a tool error");
        let fetch_tool = &pages[0].content;
        assert!(
            fetch_tool.contains("extract_failed")
                && fetch_tool.contains("Unable to extract content from the URL.")
                && fetch_tool.contains("do not sleep-retry"),
            "{fetch_tool}"
        );
        assert_eq!(pages[0].url, "https://example.com/docs");
    }

    struct StatusFailure {
        reason: String,
    }

    impl WebTransport for StatusFailure {
        fn post_json<'a>(
            &'a self,
            _endpoint: &'a str,
            _bearer: Option<&'a str>,
            _body: &'a [u8],
            _timeout: Duration,
            _cancel: CancellationToken,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<u8>, WebError>> + Send + 'a>,
        > {
            Box::pin(async move { Err(WebError::unavailable(self.reason.clone())) })
        }
    }

    #[tokio::test]
    async fn http_4xx_fetch_is_a_page_and_5xx_stays_an_error() {
        let client = WebClient::new(
            "https://api.anysearch.com",
            None,
            Arc::new(StatusFailure {
                reason: "HTTP 422 Unprocessable Entity (extract_failed: Unable to extract content from the URL.)".to_owned(),
            }),
        )
        .unwrap()
        .with_kind(SearchKind::Anysearch);
        let pages = client
            .contents(
                &["https://example.com/docs".to_owned()],
                CancellationToken::new(),
            )
            .await
            .expect("422 is final page text");
        assert!(
            pages[0].content.contains("HTTP 422"),
            "{}",
            pages[0].content
        );
        assert!(
            pages[0].content.contains("do not sleep-retry"),
            "{}",
            pages[0].content
        );

        let down = WebClient::new(
            "https://api.anysearch.com",
            None,
            Arc::new(StatusFailure {
                reason: "HTTP 502 Bad Gateway".to_owned(),
            }),
        )
        .unwrap()
        .with_kind(SearchKind::Anysearch);
        let error = down
            .contents(
                &["https://example.com/docs".to_owned()],
                CancellationToken::new(),
            )
            .await
            .expect_err("502 stays a transport error");
        assert!(error.to_string().contains("HTTP 502"), "{error}");
        assert!(!tool_failure("fetch", &error).contains("sleep-retry"));
    }
}
