//! First-party provider runtime.
//!
//! Resolves one configured provider endpoint into a streaming
//! [`Provider`] over one of three wire protocols (`anthropic-messages`,
//! `openai-completions`, `openai-responses`). Vendor differences are data in
//! `settings.json`; credentials never enter settings — the caller supplies a
//! key when resolving. Streaming completions egress through the injectable
//! [`SseTransport`] seam. Catalog refresh and OAuth use the pinned HTTP
//! client. The application fixes the User-Agent on provider requests.
//!
//! [`catalog`] carries the vendor data this runtime is configured from: the
//! models.dev cloud document, its on-disk cache, and the hand-maintained
//! Codex / OpenDesign rows injected into every resolved document. It lives
//! here rather than in `mycode-config` because refreshing it is an HTTP
//! call, and the configuration authority stays free of network dependencies.

mod anthropic_messages;
mod cache;
pub mod catalog;
mod driver;
mod family;
mod http_pin;
mod oauth;
mod openai_completions;
mod openai_responses;
mod proxy;
mod sse;
pub mod transport;
mod wire_common;
mod xml_tool_calls;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use mycode_config::ProviderSettings;
use mycode_core::{EventStream, Provider, ProviderError, ProviderErrorKind, Request};

pub use family::keeps_thinking_when_off;
pub use http_pin::{
    PinMode, PinnedBody, PinnedRequest, connection_addresses, is_public_ip, send_pinned,
};
pub use oauth::{
    CODEX_VERIFICATION_URI, COPILOT_CHAT_HEADERS, COPILOT_PROVIDER_ID, CodexDevicePoll,
    CodexDeviceStart, DeviceCodeStart, DeviceTokenPoll, OAuthSecret, OPENAI_CODEX_PROVIDER_ID,
    ORIGINATOR, PinnedCall, chatgpt_account_id, copilot_bearer, exchange_codex_code,
    parse_oauth_secret, poll_codex_device_token, poll_device_token, refresh_codex_token,
    start_codex_device_flow, start_device_flow,
};
pub use proxy::{
    PROXY_TEST_URL, ProxyTestReport, ProxyTestRoute, set_proxy_preference, test_proxy,
};
pub use transport::{ReqwestTransport, SseTransport, TransportCall};

/// OpenAI-compatible completions path appended to the base URL.
const OPENAI_COMPLETIONS_PATH: &str = "/chat/completions";
/// OpenAI-compatible Responses path appended to the base URL.
const OPENAI_RESPONSES_PATH: &str = "/responses";
/// Anthropic protocol version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// One resolved streaming provider endpoint.
#[derive(Clone)]
pub struct ResolvedProvider {
    /// Provider id from settings, used on the usage log line.
    pub id: String,
    /// Wire protocol kind from settings.
    pub kind: String,
    /// Full request endpoint URL.
    pub endpoint: String,
    /// Model id sent on every request.
    pub model: String,
    /// Request headers (auth, protocol version, User-Agent).
    pub headers: Vec<(String, String)>,
}

/// Appends `prefix + suffix` to the base without doubling: a base that
/// already ends with the full path is used as-is, and a base ending with
/// `prefix` gets only `suffix`.
fn endpoint_with(base: &str, prefix: &str, suffix: &str) -> String {
    let full = format!("{prefix}{suffix}");
    if base.ends_with(&full) {
        base.to_owned()
    } else if base.ends_with(prefix) {
        format!("{base}{suffix}")
    } else {
        format!("{base}{full}")
    }
}

impl ResolvedProvider {
    /// Resolves settings plus a vault-supplied key into one endpoint.
    ///
    /// `user_agent` is the application's fixed agent string
    /// (`mycode_app::default_user_agent`).
    ///
    /// # Errors
    ///
    /// Returns a rejected error for an unknown protocol kind or model not
    /// offered by the provider.
    pub fn resolve(
        settings: &ProviderSettings,
        model: &str,
        api_key: &str,
        user_agent: &str,
    ) -> Result<Self, ProviderError> {
        let base = settings.base_url.trim_end_matches('/');
        let (endpoint, extra) = match settings.kind.as_str() {
            "anthropic-messages" => (
                // A base that already carries the versioned path (common when
                // pasting a vendor console URL) must not be doubled.
                endpoint_with(base, "/v1", "/messages"),
                vec![
                    ("x-api-key".to_owned(), api_key.to_owned()),
                    ("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()),
                ],
            ),
            "openai-completions" => (
                endpoint_with(base, "", OPENAI_COMPLETIONS_PATH),
                vec![("authorization".to_owned(), format!("Bearer {api_key}"))],
            ),
            "openai-responses" => (
                endpoint_with(base, "", OPENAI_RESPONSES_PATH),
                vec![("authorization".to_owned(), format!("Bearer {api_key}"))],
            ),
            _ => {
                return Err(ProviderError::with_message(
                    ProviderErrorKind::Rejected,
                    "unknown provider kind",
                ));
            }
        };
        if !settings.models.iter().any(|m| m == model) {
            return Err(ProviderError::with_message(
                ProviderErrorKind::Rejected,
                "model not offered by this provider",
            ));
        }
        let mut headers = vec![("user-agent".to_owned(), user_agent.to_owned())];
        headers.extend(extra);
        Ok(Self {
            id: settings.id.clone(),
            kind: settings.kind.clone(),
            endpoint,
            model: model.to_owned(),
            headers,
        })
    }
}

/// A streaming provider bound to one endpoint and transport.
#[derive(Clone)]
pub struct WireProvider {
    resolved: ResolvedProvider,
    transport: Arc<dyn SseTransport>,
}

impl WireProvider {
    /// Binds one resolved endpoint to a transport.
    #[must_use]
    pub fn new(resolved: ResolvedProvider, transport: Arc<dyn SseTransport>) -> Self {
        Self {
            resolved,
            transport,
        }
    }

    /// Builds the wire call and whether a retry rule could rewrite it; the
    /// driver consults that flag before attempting a rewrite. Decided from
    /// the JSON value before serialization, not by scanning the encoded bytes.
    fn call_for(&self, request: &Request) -> (TransportCall, bool) {
        let model = self.resolved.model.as_str();
        let endpoint = self.resolved.endpoint.as_str();
        let (body, replays_thinking) = match self.resolved.kind.as_str() {
            "anthropic-messages" => {
                let body = anthropic_messages::build_body(model, endpoint, request);
                let replays = anthropic_messages::replays_thinking(&body);
                (body, replays)
            }
            "openai-responses" => (
                openai_responses::build_body(model, endpoint, request),
                false,
            ),
            _ => (
                openai_completions::build_body(model, endpoint, request),
                false,
            ),
        };
        let retryable = replays_thinking || cache::has_prompt_cache_key(&body);
        let mut headers = self.resolved.headers.clone();
        if let Some(header) = cache::openrouter_session_header(
            &self.resolved.endpoint,
            request.prompt_cache_key.as_deref(),
        ) {
            headers.push(header);
        }
        let call = TransportCall {
            endpoint: self.resolved.endpoint.clone(),
            headers,
            body: serde_json::to_vec(&body).unwrap_or_default().into(),
        };
        (call, retryable)
    }
}

fn reducer_for(kind: &str) -> Box<dyn driver::FrameReducer + Send> {
    match kind {
        "anthropic-messages" => Box::new(anthropic_messages::MessagesReducer::new()),
        "openai-responses" => Box::new(openai_responses::ResponsesReducer::new()),
        _ => Box::new(openai_completions::CompletionsReducer::new()),
    }
}

impl Provider for WireProvider {
    fn stream<'a>(
        &'a self,
        request: &'a Request,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            request.validate()?;
            let (call, retryable) = self.call_for(request);
            let kind = self.resolved.kind.clone();
            let (sender, stream) = EventStream::channel(cancel.clone());
            let transport = Arc::clone(&self.transport);
            tokio::spawn(async move {
                driver::drive(
                    transport,
                    call,
                    retryable,
                    move || reducer_for(&kind),
                    sender,
                    cancel,
                )
                .await;
            });
            Ok(stream)
        })
    }
}
