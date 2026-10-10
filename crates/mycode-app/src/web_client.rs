//! First-party bounded web search over a configured backend family.
//!
//! Querit and custom backends share `POST {endpoint}/v1/search` plus
//! `POST {endpoint}/v1/contents`. AnySearch uses `POST {endpoint}/v1/search`
//! with `{query, max_results}` answering `{data.results}` and
//! `POST {endpoint}/v1/extract` with `{url}` for page text. Everything is
//! bounded: result count, URL count, response bytes, and aggregate payload
//! bytes. The transport is injectable so hosts can substitute their own;
//! the production [`transport::ReqwestWebTransport`] ships with the crate.

pub mod guard;
pub mod transport;

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
        || lower.contains("http 422")
        || lower.contains("unable to extract")
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
#[async_trait::async_trait]
pub trait WebTransport: Send + Sync + 'static {
    /// Posts a JSON body with an optional bearer key and returns the raw
    /// response bytes.
    ///
    /// # Errors
    ///
    /// Returns [`WebError`] for transport-level failures.
    async fn post_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        body: &[u8],
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<Vec<u8>, WebError>;
}

/// Backend family that selects the request and response wire mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchKind {
    /// Querit `POST /v1/search` + `POST /v1/contents`.
    Querit,
    /// AnySearch `POST /v1/search` + `POST /v1/extract`.
    Anysearch,
    /// Querit-compatible custom endpoint.
    #[default]
    Custom,
}

impl SearchKind {
    /// Parses a settings `kind` spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "querit" => Some(Self::Querit),
            "anysearch" => Some(Self::Anysearch),
            "custom" => Some(Self::Custom),
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
            kind: SearchKind::Custom,
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
    /// answering `{"results": {"result": [...]}}`; the flat
    /// `{"results": [...]}` shape is accepted for custom backends.
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
            SearchKind::Querit | SearchKind::Custom => {
                self.search_querit(query, max_results, cancel).await
            }
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
        let hits = payload["results"]["result"].as_array().or_else(|| {
            // Custom backends answer with a flat array under `results`.
            payload["results"].as_array()
        });
        let hits = hits.ok_or(WebError::Protocol)?;
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
            .take(max_results)
            .filter(|result| guard::is_fetchable_url(&result.url))
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
    /// `{"results": [{url, content}]}`; the `pages` shape is accepted for
    /// custom backends. `crawlTimeout` is seconds in `1..=60`.
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
            SearchKind::Querit | SearchKind::Custom => self.contents_querit(urls, cancel).await,
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
        let raw = self
            .post(
                "/v1/contents",
                &body,
                Duration::from_secs(guard::CONTENTS_TIMEOUT_SECS),
                cancel,
            )
            .await?;
        let payload: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
        let pages_wire = payload["results"]
            .as_array()
            .or_else(|| payload["pages"].as_array())
            .ok_or(WebError::Protocol)?;
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
            let raw = self
                .post(
                    "/v1/extract",
                    &body,
                    Duration::from_secs(guard::CONTENTS_TIMEOUT_SECS),
                    cancel.clone(),
                )
                .await?;
            let payload: serde_json::Value =
                serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
            if let Some(error) = anysearch_rejected(&payload) {
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

    #[async_trait::async_trait]
    impl WebTransport for StaticFailure {
        async fn post_json(
            &self,
            _endpoint: &str,
            _bearer: Option<&str>,
            _body: &[u8],
            _timeout: Duration,
            _cancel: CancellationToken,
        ) -> Result<Vec<u8>, WebError> {
            Err(pinned_transport_error(&self.reason))
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

    #[async_trait::async_trait]
    impl WebTransport for StaticJson {
        async fn post_json(
            &self,
            _endpoint: &str,
            _bearer: Option<&str>,
            _body: &[u8],
            _timeout: Duration,
            _cancel: CancellationToken,
        ) -> Result<Vec<u8>, WebError> {
            Ok(self.body.clone())
        }
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
        let fetch_error = client
            .contents(
                &["https://example.com/docs".to_owned()],
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        let fetch_tool = tool_failure("fetch", &fetch_error);
        assert!(
            fetch_tool.contains("extract_failed")
                && fetch_tool.contains("Unable to extract content from the URL.")
                && fetch_tool.contains("do not sleep-retry"),
            "{fetch_tool}"
        );
    }
}
