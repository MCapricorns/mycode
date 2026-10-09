//! Device-code and subscription OAuth for GitHub Copilot, xAI, and Codex.
//!
//! These providers do not take only a pasted API key. The user authorizes
//! MYCode, the resulting token is stored in the secret vault, and Copilot
//! turns exchange it for a short-lived bearer. Error messages carry statuses
//! and field names only — never token values. Each request is still sent
//! through the pinned client (DNS pin, checked addresses, no redirects). The
//! caller's User-Agent is copied onto that request, and a per-request timeout
//! is honored when the built request carries one; otherwise the pin timeout
//! stays 30s. The caller's HTTP proxy is not applied, because pinning dials
//! only the addresses that passed the public-address check.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provider id that authenticates through this module.
pub const COPILOT_PROVIDER_ID: &str = "github-copilot";
/// xAI SuperGrok / X Premium subscription.
pub const XAI_PROVIDER_ID: &str = "xai";
/// ChatGPT Plus/Pro Codex subscription.
pub const OPENAI_CODEX_PROVIDER_ID: &str = "openai-codex";
/// Public device-flow client id of the GitHub Copilot CLI app (no secret).
const DEVICE_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
/// Device authorization endpoint.
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
/// Device token polling endpoint.
const DEVICE_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
/// Copilot bearer-token exchange endpoint.
const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
/// Requested device-flow scope.
const DEVICE_SCOPE: &str = "read:user";
/// Bounded string fields from GitHub responses.
const MAX_FIELD_BYTES: usize = 8 * 1024;

const PINNED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// User-Agent and timeout taken from `client` for a pinned OAuth request.
///
/// Building a probe request applies the client's default headers. The client
/// timeout is not copied onto that request unless the caller set one on the
/// request itself, so a missing timeout stays [`PINNED_TIMEOUT`].
fn pinned_call_hints(client: &reqwest::Client) -> (Option<String>, std::time::Duration) {
    let Ok(request) = client.get("https://pinned.invalid/").build() else {
        return (None, PINNED_TIMEOUT);
    };
    let user_agent = request
        .headers()
        .get(reqwest::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let timeout = request.timeout().copied().unwrap_or(PINNED_TIMEOUT);
    (user_agent, timeout)
}

async fn pinned(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<crate::PinnedBody>,
) -> Result<reqwest::Response, String> {
    let (user_agent, timeout) = pinned_call_hints(client);
    crate::send_pinned(crate::PinnedRequest {
        method,
        url: url.to_owned(),
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        body,
        mode: crate::PinMode::CheckRedirect,
        timeout: Some(timeout),
        user_agent,
        cancel: tokio_util::sync::CancellationToken::new(),
    })
    .await
}

/// A started device authorization: what the user sees and what we poll with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCodeStart {
    /// Opaque poll credential.
    pub device_code: String,
    /// Code the user types at the verification page.
    pub user_code: String,
    /// Page the user opens.
    pub verification_uri: String,
    /// Polling cadence in seconds.
    pub interval_secs: u64,
    /// Flow lifetime in seconds.
    pub expires_in_secs: u64,
}

/// One poll attempt against the device token endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceTokenPoll {
    /// Authorization granted; carries the OAuth access token.
    Granted(String),
    /// Authorization granted with access+refresh (xAI / Codex-style).
    GrantedOAuth(OAuthSecret),
    /// The user has not approved yet; keep polling.
    Pending,
    /// GitHub asked us to slow down; extend the interval.
    SlowDown,
    /// The flow is dead (denied, expired, or unknown error kind).
    Denied(&'static str),
}

/// A short-lived Copilot API bearer token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopilotToken {
    /// Bearer value for chat completions.
    pub token: String,
    /// Unix seconds when the token stops working.
    pub expires_at_unix: u64,
}

fn bounded(value: &str) -> Option<&str> {
    if value.is_empty() || value.len() > MAX_FIELD_BYTES {
        None
    } else {
        Some(value)
    }
}

fn field<'a>(payload: &'a Value, name: &str) -> Option<&'a str> {
    payload.get(name).and_then(Value::as_str).and_then(bounded)
}

/// Starts the device authorization flow.
///
/// # Errors
///
/// Returns a transport or endpoint-shape failure without embedded secrets.
pub async fn start_device_flow(client: &reqwest::Client) -> Result<DeviceCodeStart, String> {
    let response = pinned(
        client,
        reqwest::Method::POST,
        DEVICE_CODE_URL,
        &[("accept", "application/json")],
        Some(crate::PinnedBody::Form(vec![
            ("client_id".to_owned(), DEVICE_CLIENT_ID.to_owned()),
            ("scope".to_owned(), DEVICE_SCOPE.to_owned()),
        ])),
    )
    .await
    .map_err(|error| format!("device-code request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "device-code endpoint returned {}",
            response.status()
        ));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("device-code response was not JSON: {error}"))?;
    parse_device_start(&payload).ok_or_else(|| "device-code response was missing fields".to_owned())
}

/// Parses a device authorization response.
#[must_use]
pub fn parse_device_start(payload: &Value) -> Option<DeviceCodeStart> {
    Some(DeviceCodeStart {
        device_code: field(payload, "device_code")?.to_owned(),
        user_code: field(payload, "user_code")?.to_owned(),
        verification_uri: field(payload, "verification_uri")?.to_owned(),
        interval_secs: payload.get("interval").and_then(Value::as_u64).unwrap_or(5),
        expires_in_secs: payload
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(900),
    })
}

/// Polls once for the granted OAuth token.
///
/// # Errors
///
/// Returns a transport failure; grant states arrive as [`DeviceTokenPoll`].
pub async fn poll_device_token(
    client: &reqwest::Client,
    device_code: &str,
) -> Result<DeviceTokenPoll, String> {
    let response = pinned(
        client,
        reqwest::Method::POST,
        DEVICE_TOKEN_URL,
        &[("accept", "application/json")],
        Some(crate::PinnedBody::Form(vec![
            ("client_id".to_owned(), DEVICE_CLIENT_ID.to_owned()),
            ("device_code".to_owned(), device_code.to_owned()),
            (
                "grant_type".to_owned(),
                "urn:ietf:params:oauth:grant-type:device_code".to_owned(),
            ),
        ])),
    )
    .await
    .map_err(|error| format!("device-token poll failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "device-token endpoint returned {}",
            response.status()
        ));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("device-token response was not JSON: {error}"))?;
    Ok(classify_device_poll(&payload))
}

/// Classifies one device-token response payload.
#[must_use]
pub fn classify_device_poll(payload: &Value) -> DeviceTokenPoll {
    if let Some(token) = field(payload, "access_token") {
        return DeviceTokenPoll::Granted(token.to_owned());
    }
    match field(payload, "error") {
        Some("authorization_pending") => DeviceTokenPoll::Pending,
        Some("slow_down") => DeviceTokenPoll::SlowDown,
        Some("expired_token") => DeviceTokenPoll::Denied("the sign-in code expired"),
        Some("access_denied") => DeviceTokenPoll::Denied("the request was denied on GitHub"),
        _ => DeviceTokenPoll::Denied("the sign-in could not be completed"),
    }
}

/// Exchanges the stored OAuth token for a short-lived Copilot bearer token.
///
/// # Errors
///
/// Returns a transport failure or a rejection that means "sign in again".
pub async fn copilot_bearer(
    client: &reqwest::Client,
    github_token: &str,
) -> Result<CopilotToken, String> {
    let authorization = format!("Bearer {github_token}");
    let mut headers = vec![
        ("authorization", authorization.as_str()),
        ("accept", "application/vnd.github+json"),
    ];
    headers.extend(COPILOT_CHAT_HEADERS);
    let response = pinned(
        client,
        reqwest::Method::GET,
        COPILOT_TOKEN_URL,
        &headers,
        None,
    )
    .await
    .map_err(|error| format!("copilot token request failed: {error}"))?;
    if let Some(reason) = match response.status().as_u16() {
        401 | 403 => Some("copilot rejected the saved sign-in — sign in again".to_owned()),
        404 => Some("copilot token exchange is unavailable for this account".to_owned()),
        status if !(200..300).contains(&status) => {
            Some(format!("copilot token endpoint returned {status}"))
        }
        _ => None,
    } {
        return Err(reason);
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("copilot token response was not JSON: {error}"))?;
    parse_copilot_token(&payload)
        .ok_or_else(|| "copilot token response was missing fields".to_owned())
}

/// Headers Pi sends on Copilot token exchange and chat calls.
pub const COPILOT_CHAT_HEADERS: &[(&str, &str)] = &[
    ("user-agent", "GitHubCopilotChat/0.35.0"),
    ("editor-version", "vscode/1.107.0"),
    ("editor-plugin-version", "copilot-chat/0.35.0"),
    ("copilot-integration-id", "vscode-chat"),
];

/// Compact OAuth blob stored in the provider secret vault (pi `auth.json` shape).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthSecret {
    /// Always `oauth`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Access token used as Bearer.
    pub access: String,
    /// Refresh token.
    pub refresh: String,
    /// Unix seconds when `access` should be refreshed.
    pub expires: u64,
    /// ChatGPT account id extracted from the Codex JWT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

impl OAuthSecret {
    /// Encodes the compact vault value.
    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// True when the access token should be refreshed.
    #[must_use]
    pub fn expired(&self, now_unix: u64) -> bool {
        self.expires <= now_unix.saturating_add(60)
    }
}

/// Parses a vault value that may be a raw key or an OAuth blob.
#[must_use]
pub fn parse_oauth_secret(stored: &str) -> Option<OAuthSecret> {
    let trimmed = stored.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    serde_json::from_str::<OAuthSecret>(trimmed)
        .ok()
        .filter(|secret| secret.kind == "oauth" && !secret.access.is_empty())
}

/// ChatGPT account id carried in a Codex access JWT.
#[must_use]
pub fn chatgpt_account_id(access_token: &str) -> Option<String> {
    let payload = access_token.split('.').nth(1)?;
    let mut padded = payload.replace('-', "+").replace('_', "/");
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let bytes = decode_b64(&padded)?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .pointer("/https://api.openai.com/auth/chatgpt_account_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn decode_b64(input: &str) -> Option<Vec<u8>> {
    fn val(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut chunk = [0_u8; 4];
    let mut n = 0;
    for &byte in bytes {
        if byte == b'=' {
            break;
        }
        chunk[n] = val(byte)?;
        n += 1;
        if n == 4 {
            out.push((chunk[0] << 2) | (chunk[1] >> 4));
            out.push((chunk[1] << 4) | (chunk[2] >> 2));
            out.push((chunk[2] << 6) | chunk[3]);
            n = 0;
        }
    }
    if n == 3 {
        out.push((chunk[0] << 2) | (chunk[1] >> 4));
        out.push((chunk[1] << 4) | (chunk[2] >> 2));
    } else if n == 2 {
        out.push((chunk[0] << 2) | (chunk[1] >> 4));
    }
    Some(out)
}

const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const XAI_DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const XAI_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const CODEX_DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_DEVICE_REDIRECT: &str = "https://auth.openai.com/deviceauth/callback";
/// Page the user opens for Codex device login.
pub const CODEX_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
/// Page the user opens for xAI device login.
pub const XAI_VERIFICATION_URI: &str = "https://auth.x.ai/device";

/// Starts the xAI device-code flow (pi `referrer=pi`).
///
/// # Errors
///
/// Returns a transport or endpoint-shape failure without embedded secrets.
pub async fn start_xai_device_flow(client: &reqwest::Client) -> Result<DeviceCodeStart, String> {
    let response = pinned(
        client,
        reqwest::Method::POST,
        XAI_DEVICE_CODE_URL,
        &[("accept", "application/json")],
        Some(crate::PinnedBody::Form(vec![
            ("client_id".to_owned(), XAI_CLIENT_ID.to_owned()),
            ("scope".to_owned(), XAI_SCOPE.to_owned()),
            ("referrer".to_owned(), "pi".to_owned()),
        ])),
    )
    .await
    .map_err(|error| format!("xAI device-code request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "xAI device-code endpoint returned {}",
            response.status()
        ));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("xAI device-code response was not JSON: {error}"))?;
    parse_device_start(&payload)
        .ok_or_else(|| "xAI device-code response was missing fields".to_owned())
}

/// Polls once for the xAI OAuth grant.
///
/// # Errors
///
/// Returns a transport failure; grant states arrive as [`DeviceTokenPoll`].
pub async fn poll_xai_device_token(
    client: &reqwest::Client,
    device_code: &str,
) -> Result<DeviceTokenPoll, String> {
    let response = pinned(
        client,
        reqwest::Method::POST,
        XAI_TOKEN_URL,
        &[("accept", "application/json")],
        Some(crate::PinnedBody::Form(vec![
            ("client_id".to_owned(), XAI_CLIENT_ID.to_owned()),
            ("device_code".to_owned(), device_code.to_owned()),
            (
                "grant_type".to_owned(),
                "urn:ietf:params:oauth:grant-type:device_code".to_owned(),
            ),
        ])),
    )
    .await
    .map_err(|error| format!("xAI device-token poll failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "xAI device-token endpoint returned {}",
            response.status()
        ));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("xAI device-token response was not JSON: {error}"))?;
    if let Ok(secret) = secret_from_token_payload(&payload, None, false) {
        return Ok(DeviceTokenPoll::GrantedOAuth(secret));
    }
    Ok(classify_device_poll(&payload))
}

/// Refreshes an xAI access token.
///
/// # Errors
///
/// Returns a transport or grant failure.
pub async fn refresh_xai_token(
    client: &reqwest::Client,
    refresh: &str,
) -> Result<OAuthSecret, String> {
    refresh_form_token(client, XAI_TOKEN_URL, XAI_CLIENT_ID, refresh, None).await
}

/// Codex device-auth identifiers returned by the usercode endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexDeviceStart {
    /// Opaque poll id.
    pub device_auth_id: String,
    /// Code the user types.
    pub user_code: String,
    /// Polling cadence in seconds.
    pub interval_secs: u64,
}

/// Starts the OpenAI Codex device-code flow.
///
/// # Errors
///
/// Returns a transport or endpoint-shape failure.
pub async fn start_codex_device_flow(client: &reqwest::Client) -> Result<CodexDeviceStart, String> {
    let body = serde_json::to_vec(&serde_json::json!({ "client_id": CODEX_CLIENT_ID }))
        .map_err(|error| format!("Codex device-code request failed: {error}"))?;
    let response = pinned(
        client,
        reqwest::Method::POST,
        CODEX_USER_CODE_URL,
        &[("content-type", "application/json")],
        Some(crate::PinnedBody::Bytes(body.into())),
    )
    .await
    .map_err(|error| format!("Codex device-code request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Codex device-code endpoint returned {}",
            response.status()
        ));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("Codex device-code response was not JSON: {error}"))?;
    let interval = payload
        .get("interval")
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
        .unwrap_or(5);
    Ok(CodexDeviceStart {
        device_auth_id: field(&payload, "device_auth_id")
            .ok_or_else(|| "Codex device-code response was missing fields".to_owned())?
            .to_owned(),
        user_code: field(&payload, "user_code")
            .ok_or_else(|| "Codex device-code response was missing fields".to_owned())?
            .to_owned(),
        interval_secs: interval.max(1),
    })
}

/// One Codex device-auth poll: pending, or an authorization code plus verifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexDevicePoll {
    /// Still waiting.
    Pending,
    /// Authorization code plus PKCE verifier from the server.
    Ready {
        /// Authorization code.
        authorization_code: String,
        /// PKCE verifier issued by the device-auth service.
        code_verifier: String,
    },
    /// The flow is dead.
    Denied(&'static str),
}

/// Polls the Codex device-auth token endpoint once.
///
/// # Errors
///
/// Returns a transport failure.
pub async fn poll_codex_device_token(
    client: &reqwest::Client,
    start: &CodexDeviceStart,
) -> Result<CodexDevicePoll, String> {
    let body = serde_json::to_vec(&serde_json::json!({
        "device_auth_id": start.device_auth_id,
        "user_code": start.user_code,
    }))
    .map_err(|error| format!("Codex device-token poll failed: {error}"))?;
    let response = pinned(
        client,
        reqwest::Method::POST,
        CODEX_DEVICE_TOKEN_URL,
        &[("content-type", "application/json")],
        Some(crate::PinnedBody::Bytes(body.into())),
    )
    .await
    .map_err(|error| format!("Codex device-token poll failed: {error}"))?;
    let status = response.status().as_u16();
    if status == 403 || status == 404 {
        return Ok(CodexDevicePoll::Pending);
    }
    if !response.status().is_success() {
        return Err(format!("Codex device-token endpoint returned {status}"));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("Codex device-token response was not JSON: {error}"))?;
    if let (Some(code), Some(verifier)) = (
        field(&payload, "authorization_code"),
        field(&payload, "code_verifier"),
    ) {
        return Ok(CodexDevicePoll::Ready {
            authorization_code: code.to_owned(),
            code_verifier: verifier.to_owned(),
        });
    }
    match payload.get("error").and_then(|error| {
        error
            .as_str()
            .or_else(|| error.get("code").and_then(Value::as_str))
    }) {
        Some("deviceauth_authorization_pending") | None => Ok(CodexDevicePoll::Pending),
        Some("slow_down") => Ok(CodexDevicePoll::Pending),
        _ => Ok(CodexDevicePoll::Denied(
            "the Codex sign-in could not be completed",
        )),
    }
}

/// Exchanges a Codex device authorization code for OAuth tokens.
///
/// # Errors
///
/// Returns a transport or field-shape failure.
pub async fn exchange_codex_code(
    client: &reqwest::Client,
    authorization_code: &str,
    code_verifier: &str,
) -> Result<OAuthSecret, String> {
    let response = pinned(
        client,
        reqwest::Method::POST,
        CODEX_TOKEN_URL,
        &[("content-type", "application/x-www-form-urlencoded")],
        Some(crate::PinnedBody::Form(vec![
            ("grant_type".to_owned(), "authorization_code".to_owned()),
            ("client_id".to_owned(), CODEX_CLIENT_ID.to_owned()),
            ("code".to_owned(), authorization_code.to_owned()),
            ("code_verifier".to_owned(), code_verifier.to_owned()),
            ("redirect_uri".to_owned(), CODEX_DEVICE_REDIRECT.to_owned()),
        ])),
    )
    .await
    .map_err(|error| format!("Codex token exchange failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Codex token endpoint returned {}",
            response.status()
        ));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("Codex token response was not JSON: {error}"))?;
    secret_from_token_payload(&payload, None, true)
}

/// Refreshes a Codex access token.
///
/// # Errors
///
/// Returns a transport or grant failure.
pub async fn refresh_codex_token(
    client: &reqwest::Client,
    refresh: &str,
) -> Result<OAuthSecret, String> {
    refresh_form_token(
        client,
        CODEX_TOKEN_URL,
        CODEX_CLIENT_ID,
        refresh,
        Some(true),
    )
    .await
}

async fn refresh_form_token(
    client: &reqwest::Client,
    url: &str,
    client_id: &str,
    refresh: &str,
    extract_account: Option<bool>,
) -> Result<OAuthSecret, String> {
    let response = pinned(
        client,
        reqwest::Method::POST,
        url,
        &[("content-type", "application/x-www-form-urlencoded")],
        Some(crate::PinnedBody::Form(vec![
            ("grant_type".to_owned(), "refresh_token".to_owned()),
            ("refresh_token".to_owned(), refresh.to_owned()),
            ("client_id".to_owned(), client_id.to_owned()),
        ])),
    )
    .await
    .map_err(|error| format!("token refresh failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("token refresh returned {}", response.status()));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("token refresh was not JSON: {error}"))?;
    secret_from_token_payload(&payload, Some(refresh), extract_account.unwrap_or(false))
}

fn secret_from_token_payload(
    payload: &Value,
    previous_refresh: Option<&str>,
    extract_account: bool,
) -> Result<OAuthSecret, String> {
    let access = field(payload, "access_token")
        .ok_or_else(|| "token response was missing access_token".to_owned())?;
    let refresh = field(payload, "refresh_token")
        .map(str::to_owned)
        .or_else(|| previous_refresh.map(str::to_owned))
        .ok_or_else(|| "token response was missing refresh_token".to_owned())?;
    let expires_in = payload
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(3600);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let account_id = if extract_account {
        chatgpt_account_id(access)
    } else {
        None
    };
    Ok(OAuthSecret {
        kind: "oauth".to_owned(),
        access: access.to_owned(),
        refresh,
        expires: now.saturating_add(expires_in.saturating_sub(300)),
        account_id,
    })
}

/// Parses a Copilot token exchange response.
#[must_use]
pub fn parse_copilot_token(payload: &Value) -> Option<CopilotToken> {
    Some(CopilotToken {
        token: field(payload, "token")?.to_owned(),
        expires_at_unix: payload.get("expires_at").and_then(Value::as_u64)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{PINNED_TIMEOUT, pinned_call_hints};

    #[test]
    fn pinned_requests_copy_the_caller_user_agent() {
        let client = reqwest::Client::builder()
            .user_agent("mycode-caller/1")
            .build()
            .expect("client");
        let (user_agent, timeout) = pinned_call_hints(&client);
        assert_eq!(user_agent.as_deref(), Some("mycode-caller/1"));
        assert_ne!(user_agent.as_deref(), Some("mycode"));
        assert_eq!(timeout, PINNED_TIMEOUT);
    }
}
