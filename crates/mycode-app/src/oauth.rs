//! Provider OAuth device flows: Copilot, xAI, and Codex sign-in with their
//! poll loops, token refresh, and settings upserts.

use std::sync::Arc;

use mycode_config::{ProviderSettings, replace_app_settings};
use mycode_providers::catalog::http_client;
use mycode_providers::{
    CODEX_VERIFICATION_URI, COPILOT_CHAT_HEADERS, COPILOT_PROVIDER_ID, CodexDevicePoll,
    DeviceCodeStart, DeviceTokenPoll, OAuthSecret, OPENAI_CODEX_PROVIDER_ID, XAI_PROVIDER_ID,
    copilot_bearer, exchange_codex_code, parse_oauth_secret, poll_codex_device_token,
    poll_device_token, poll_xai_device_token, refresh_codex_token, refresh_xai_token,
    start_codex_device_flow, start_device_flow, start_xai_device_flow,
};

use crate::settings_io::{load_settings, render_config_error, save_provider_key};
use crate::state::{CoreState, UPDATE_USER_AGENT};
use crate::{BridgeEvent, BridgeReply, CopilotSignInInfo};

/// OpenAI device codes live fifteen minutes. `CodexDeviceStart` carries no
/// expiry field (unlike the Copilot/xAI `DeviceCodeStart`), so the poll
/// deadline mirrors the documented lifetime.
const CODEX_FLOW_LIFETIME_SECS: u64 = 15 * 60;

// ---- GitHub Copilot OAuth device flow ----

/// Starts a device flow, opens the browser, and spawns the poll loop that
/// finishes the sign-in (or reports failure) over the event channel.
pub(crate) async fn oauth_sign_in(
    state: Arc<CoreState>,
    events: crate::BridgeEventTx,
    provider_id: String,
    models: Vec<String>,
) -> BridgeReply {
    let client = match http_client(UPDATE_USER_AGENT) {
        Ok(client) => client,
        Err(message) => return BridgeReply::CopilotSignInStarted(Err(message)),
    };
    match provider_id.as_str() {
        XAI_PROVIDER_ID => {
            let start = match start_xai_device_flow(&client).await {
                Ok(start) => start,
                Err(message) => return BridgeReply::CopilotSignInStarted(Err(message)),
            };
            let uri = if start.verification_uri.is_empty() {
                mycode_providers::XAI_VERIFICATION_URI.to_owned()
            } else {
                start.verification_uri.clone()
            };
            open_browser(&uri);
            spawn_xai_poll(state, events, client, start.clone(), models);
            BridgeReply::CopilotSignInStarted(Ok(CopilotSignInInfo {
                user_code: start.user_code,
                verification_uri: uri,
            }))
        }
        OPENAI_CODEX_PROVIDER_ID => {
            let start = match start_codex_device_flow(&client).await {
                Ok(start) => start,
                Err(message) => return BridgeReply::CopilotSignInStarted(Err(message)),
            };
            open_browser(CODEX_VERIFICATION_URI);
            spawn_codex_poll(state, events, client, start.clone(), models);
            BridgeReply::CopilotSignInStarted(Ok(CopilotSignInInfo {
                user_code: start.user_code,
                verification_uri: CODEX_VERIFICATION_URI.to_owned(),
            }))
        }
        COPILOT_PROVIDER_ID => {
            let start = match start_device_flow(&client).await {
                Ok(start) => start,
                Err(message) => return BridgeReply::CopilotSignInStarted(Err(message)),
            };
            open_browser(&start.verification_uri);
            spawn_copilot_poll(state, events, client, start.clone(), models);
            BridgeReply::CopilotSignInStarted(Ok(CopilotSignInInfo {
                user_code: start.user_code,
                verification_uri: start.verification_uri,
            }))
        }
        // Unknown ids must not silently ride the Copilot flow.
        _ => BridgeReply::CopilotSignInStarted(Err(format!(
            "unknown sign-in provider '{provider_id}'"
        ))),
    }
}

/// Shared device-flow poll loop for Copilot and xAI: sleeps the flow's poll
/// interval, enforces its expiry deadline, and forwards terminal outcomes
/// over the event channel. `extract` pulls this flow's granted payload out
/// of a poll result (the other `Granted` flavor is ignored, as before);
/// `finish` completes the sign-in with it.
#[allow(clippy::too_many_arguments)]
fn spawn_device_poll<V, P, PF, X, F, FF>(
    state: Arc<CoreState>,
    events: crate::BridgeEventTx,
    client: reqwest::Client,
    start: DeviceCodeStart,
    models: Vec<String>,
    poll: P,
    extract: X,
    finish: F,
) where
    P: Fn(reqwest::Client, String) -> PF + Send + 'static,
    PF: std::future::Future<Output = Result<DeviceTokenPoll, String>> + Send,
    X: Fn(DeviceTokenPoll) -> Option<V> + Send + 'static,
    F: Fn(Arc<CoreState>, V, Vec<String>) -> FF + Send + 'static,
    FF: std::future::Future<Output = Result<(), String>> + Send,
    V: Send + 'static,
{
    let mut interval_secs = start.interval_secs.max(1);
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(start.expires_in_secs.max(1));
    let device_code = start.device_code;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
            if std::time::Instant::now() >= deadline {
                let _ = events.try_send(BridgeEvent::CopilotSignInFailed {
                    message: "the sign-in code expired before authorization".to_owned(),
                });
                return;
            }
            match poll(client.clone(), device_code.clone()).await {
                Ok(DeviceTokenPoll::Pending) => {}
                Ok(DeviceTokenPoll::SlowDown) => interval_secs += 5,
                Ok(DeviceTokenPoll::Denied(reason)) => {
                    let _ = events.try_send(BridgeEvent::CopilotSignInFailed {
                        message: reason.to_owned(),
                    });
                    return;
                }
                Ok(outcome) => {
                    // Granted or GrantedOAuth: whichever this flow treats as
                    // its success payload; the other flavor keeps polling.
                    let Some(granted) = extract(outcome) else {
                        continue;
                    };
                    let outcome = finish(state.clone(), granted, models).await;
                    let _ = events.try_send(match outcome {
                        Ok(()) => BridgeEvent::CopilotSignedIn,
                        Err(message) => BridgeEvent::CopilotSignInFailed { message },
                    });
                    return;
                }
                Err(message) => {
                    let _ = events.try_send(BridgeEvent::CopilotSignInFailed { message });
                    return;
                }
            }
        }
    });
}

fn spawn_copilot_poll(
    state: Arc<CoreState>,
    events: crate::BridgeEventTx,
    client: reqwest::Client,
    start: DeviceCodeStart,
    models: Vec<String>,
) {
    spawn_device_poll(
        state,
        events,
        client,
        start,
        models,
        |client, code| async move { poll_device_token(&client, &code).await },
        |outcome| match outcome {
            DeviceTokenPoll::Granted(token) => Some(token),
            _ => None,
        },
        |state, github_token, models| async move {
            finish_copilot_sign_in(&state, &github_token, &models).await
        },
    );
}

fn spawn_xai_poll(
    state: Arc<CoreState>,
    events: crate::BridgeEventTx,
    client: reqwest::Client,
    start: DeviceCodeStart,
    models: Vec<String>,
) {
    spawn_device_poll(
        state,
        events,
        client,
        start,
        models,
        |client, code| async move { poll_xai_device_token(&client, &code).await },
        |outcome| match outcome {
            DeviceTokenPoll::GrantedOAuth(secret) => Some(secret),
            _ => None,
        },
        |state, secret, models| async move {
            finish_oauth_sign_in(&state, XAI_PROVIDER_ID, &secret, &models).await
        },
    );
}

fn spawn_codex_poll(
    state: Arc<CoreState>,
    events: crate::BridgeEventTx,
    client: reqwest::Client,
    start: mycode_providers::CodexDeviceStart,
    models: Vec<String>,
) {
    let interval_secs = start.interval_secs.max(1);
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(CODEX_FLOW_LIFETIME_SECS);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
            if std::time::Instant::now() >= deadline {
                let _ = events.try_send(BridgeEvent::CopilotSignInFailed {
                    message: "the sign-in code expired before authorization".to_owned(),
                });
                return;
            }
            match poll_codex_device_token(&client, &start).await {
                Ok(CodexDevicePoll::Ready {
                    authorization_code,
                    code_verifier,
                }) => {
                    let outcome =
                        match exchange_codex_code(&client, &authorization_code, &code_verifier)
                            .await
                        {
                            Ok(secret) => {
                                finish_oauth_sign_in(
                                    &state,
                                    OPENAI_CODEX_PROVIDER_ID,
                                    &secret,
                                    &models,
                                )
                                .await
                            }
                            Err(message) => Err(message),
                        };
                    let _ = events.try_send(match outcome {
                        Ok(()) => BridgeEvent::CopilotSignedIn,
                        Err(message) => BridgeEvent::CopilotSignInFailed { message },
                    });
                    return;
                }
                Ok(CodexDevicePoll::Pending) => {}
                Ok(CodexDevicePoll::Denied(reason)) => {
                    let _ = events.try_send(BridgeEvent::CopilotSignInFailed {
                        message: reason.to_owned(),
                    });
                    return;
                }
                Err(message) => {
                    let _ = events.try_send(BridgeEvent::CopilotSignInFailed { message });
                    return;
                }
            }
        }
    });
}

async fn finish_oauth_sign_in(
    state: &CoreState,
    provider_id: &str,
    secret: &OAuthSecret,
    models: &[String],
) -> Result<(), String> {
    save_provider_key(&state.home, provider_id, &secret.encode())?;
    upsert_catalog_provider(state, provider_id, models)
}

/// Verifies the grant works, stores the OAuth token, and configures the
/// provider entry.
async fn finish_copilot_sign_in(
    state: &CoreState,
    github_token: &str,
    models: &[String],
) -> Result<(), String> {
    let client = http_client(UPDATE_USER_AGENT)?;
    let bearer = copilot_bearer(&client, github_token).await?;
    *state.copilot.lock().await = Some((bearer.token, bearer.expires_at_unix));
    save_provider_key(&state.home, COPILOT_PROVIDER_ID, github_token)?;
    upsert_copilot_provider(state, models)
}

fn upsert_copilot_provider(state: &CoreState, models: &[String]) -> Result<(), String> {
    upsert_catalog_provider(state, COPILOT_PROVIDER_ID, models)
}

/// Adds or refreshes a catalog provider entry with the chosen models.
fn upsert_catalog_provider(
    state: &CoreState,
    provider_id: &str,
    models: &[String],
) -> Result<(), String> {
    let catalog = state.catalog.read().ok().and_then(|guard| {
        guard.document.provider(provider_id).map(|preset| {
            (
                preset.kind.clone(),
                preset.base_url.clone(),
                preset.models.clone(),
            )
        })
    });
    let (kind, base_url, catalog_models) = catalog.unwrap_or_else(|| {
        (
            mycode_providers::catalog::KIND_OPENAI_COMPLETIONS.to_owned(),
            String::new(),
            Vec::new(),
        )
    });
    if base_url.is_empty() {
        return Err(format!("{provider_id} is not in the model catalog"));
    }
    let bound: Vec<String> = if models.is_empty() {
        catalog_models
            .iter()
            .filter(|model| model.tool_call)
            .take(6)
            .map(|model| model.id.clone())
            .collect()
    } else {
        models.to_vec()
    };
    if bound.is_empty() {
        return Err(format!("no {provider_id} models were selected"));
    }
    let loaded = load_settings(&state.home)?;
    let mut settings = loaded.settings;
    let revision = loaded.revision;
    match settings
        .providers
        .iter_mut()
        .find(|provider| provider.id == provider_id)
    {
        Some(existing) => {
            existing.models = bound;
            existing.enabled = true;
            existing.kind = kind;
            existing.base_url = base_url;
        }
        None => settings.providers.push(ProviderSettings {
            id: provider_id.to_owned(),
            kind,
            base_url,
            models: bound,
            enabled: true,
            context_limit: None,
            max_output: None,
        }),
    }
    replace_app_settings(&state.home, revision, &settings)
        .map_err(|error| render_config_error(&error))?;
    Ok(())
}

/// Resolves the Authorization credential and extra headers for one provider
/// request: Copilot exchanges its long-lived token for a live bearer, OAuth
/// providers refresh expiring access tokens in place.
pub(crate) async fn resolve_request_auth(
    state: &CoreState,
    provider: &ProviderSettings,
    stored_key: &str,
) -> Result<(String, Vec<(String, String)>), String> {
    if provider.id == COPILOT_PROVIDER_ID || provider.base_url.contains("githubcopilot.com") {
        let token = ensure_copilot_bearer(state, stored_key).await?;
        let extra = COPILOT_CHAT_HEADERS
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        return Ok((token, extra));
    }
    if let Some(mut secret) = parse_oauth_secret(stored_key) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        if secret.expired(now) {
            let client = http_client(UPDATE_USER_AGENT)?;
            secret = match provider.id.as_str() {
                id if id == XAI_PROVIDER_ID => refresh_xai_token(&client, &secret.refresh).await?,
                id if id == OPENAI_CODEX_PROVIDER_ID => {
                    refresh_codex_token(&client, &secret.refresh).await?
                }
                _ => secret,
            };
            let _ = save_provider_key(&state.home, &provider.id, &secret.encode());
        }
        let mut extra = Vec::new();
        if provider.id == OPENAI_CODEX_PROVIDER_ID {
            extra.push(("originator".to_owned(), "pi".to_owned()));
            extra.push((
                "openai-beta".to_owned(),
                "responses=experimental".to_owned(),
            ));
            if let Some(account) = secret
                .account_id
                .clone()
                .or_else(|| mycode_providers::chatgpt_account_id(&secret.access))
            {
                extra.push(("chatgpt-account-id".to_owned(), account));
            }
        }
        return Ok((secret.access, extra));
    }
    Ok((stored_key.to_owned(), Vec::new()))
}

/// Returns a live Copilot bearer, exchanging a fresh one when the cached copy
/// is stale.
async fn ensure_copilot_bearer(state: &CoreState, github_token: &str) -> Result<String, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let mut cache = state.copilot.lock().await;
    if let Some((token, expires_at)) = cache.as_ref()
        && *expires_at > now.saturating_add(60)
    {
        return Ok(token.clone());
    }
    let client = http_client(UPDATE_USER_AGENT)?;
    let bearer = copilot_bearer(&client, github_token).await?;
    let token = bearer.token;
    *cache = Some((token.clone(), bearer.expires_at_unix));
    Ok(token)
}

/// Opens one verification page in the default browser, best-effort.
fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = url;
    }
}
