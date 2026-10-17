//! Provider OAuth device flows: Copilot and Codex sign-in with their poll
//! loops, token refresh, and settings upserts.

use std::sync::Arc;

use mycode_config::{HomeLayout, ProviderSettings, replace_app_settings};
use mycode_providers::{
    CODEX_VERIFICATION_URI, COPILOT_CHAT_HEADERS, COPILOT_PROVIDER_ID, CodexDevicePoll,
    DeviceCodeStart, DeviceTokenPoll, OAuthSecret, OPENAI_CODEX_PROVIDER_ID, PinnedCall,
    copilot_bearer, exchange_codex_code, parse_oauth_secret, poll_codex_device_token,
    poll_device_token, refresh_codex_token, start_codex_device_flow, start_device_flow,
};

use crate::settings_io::{load_settings, render_config_error, save_provider_key};
use crate::state::{CoreState, UPDATE_USER_AGENT};
use crate::{BridgeEvent, BridgeReply, CopilotSignInInfo};

/// OpenAI device codes live fifteen minutes. `CodexDeviceStart` carries no
/// expiry field (unlike the Copilot `DeviceCodeStart`), so the poll
/// deadline mirrors the documented lifetime.
const CODEX_FLOW_LIFETIME_SECS: u64 = 15 * 60;

fn oauth_call() -> PinnedCall {
    PinnedCall::new(UPDATE_USER_AGENT, std::time::Duration::from_secs(30))
}

// Device flows for Copilot and Codex.

/// Starts a device flow, opens the browser, and spawns the poll loop that
/// finishes the sign-in (or reports failure) over the event channel.
pub(crate) async fn oauth_sign_in(
    state: Arc<CoreState>,
    events: crate::BridgeEventTx,
    provider_id: String,
    models: Vec<String>,
) -> BridgeReply {
    let call = oauth_call();
    match provider_id.as_str() {
        OPENAI_CODEX_PROVIDER_ID => {
            let start = match start_codex_device_flow(&call).await {
                Ok(start) => start,
                Err(message) => return BridgeReply::CopilotSignInStarted(Err(message)),
            };
            open_browser(CODEX_VERIFICATION_URI);
            spawn_codex_poll(state, events, call, start.clone(), models);
            BridgeReply::CopilotSignInStarted(Ok(CopilotSignInInfo {
                user_code: start.user_code,
                verification_uri: CODEX_VERIFICATION_URI.to_owned(),
            }))
        }
        COPILOT_PROVIDER_ID => {
            let start = match start_device_flow(&call).await {
                Ok(start) => start,
                Err(message) => return BridgeReply::CopilotSignInStarted(Err(message)),
            };
            open_browser(&start.verification_uri);
            spawn_copilot_poll(state, events, call, start.clone(), models);
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

/// Copilot device-flow poll loop: sleeps the flow's poll interval, enforces
/// its expiry deadline, and forwards the terminal outcome over the event
/// channel.
fn spawn_copilot_poll(
    state: Arc<CoreState>,
    events: crate::BridgeEventTx,
    call: PinnedCall,
    start: DeviceCodeStart,
    models: Vec<String>,
) {
    let mut interval_secs = start.interval_secs.max(1);
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(start.expires_in_secs.max(1));
    let device_code = start.device_code;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
            if std::time::Instant::now() >= deadline {
                let _ = events.send(BridgeEvent::CopilotSignInFailed {
                    message: "the sign-in code expired before authorization".to_owned(),
                });
                return;
            }
            match poll_device_token(&call, &device_code).await {
                Ok(DeviceTokenPoll::Pending) => {}
                Ok(DeviceTokenPoll::SlowDown) => interval_secs += 5,
                Ok(DeviceTokenPoll::Denied(reason)) => {
                    let _ = events.send(BridgeEvent::CopilotSignInFailed {
                        message: reason.to_owned(),
                    });
                    return;
                }
                Ok(DeviceTokenPoll::Granted(github_token)) => {
                    let outcome = finish_copilot_sign_in(&state, &github_token, &models).await;
                    let _ = events.send(match outcome {
                        Ok(()) => BridgeEvent::CopilotSignedIn,
                        Err(message) => BridgeEvent::CopilotSignInFailed { message },
                    });
                    return;
                }
                Err(message) => {
                    let _ = events.send(BridgeEvent::CopilotSignInFailed { message });
                    return;
                }
            }
        }
    });
}

fn spawn_codex_poll(
    state: Arc<CoreState>,
    events: crate::BridgeEventTx,
    call: PinnedCall,
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
                let _ = events.send(BridgeEvent::CopilotSignInFailed {
                    message: "the sign-in code expired before authorization".to_owned(),
                });
                return;
            }
            match poll_codex_device_token(&call, &start).await {
                Ok(CodexDevicePoll::Ready {
                    authorization_code,
                    code_verifier,
                }) => {
                    let outcome =
                        match exchange_codex_code(&call, &authorization_code, &code_verifier).await
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
                    let _ = events.send(match outcome {
                        Ok(()) => BridgeEvent::CopilotSignedIn,
                        Err(message) => BridgeEvent::CopilotSignInFailed { message },
                    });
                    return;
                }
                Ok(CodexDevicePoll::Pending) => {}
                Ok(CodexDevicePoll::Denied(reason)) => {
                    let _ = events.send(BridgeEvent::CopilotSignInFailed {
                        message: reason.to_owned(),
                    });
                    return;
                }
                Err(message) => {
                    let _ = events.send(BridgeEvent::CopilotSignInFailed { message });
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
    // The Codex backend serves its own catalog at `/models` (models.dev does
    // not carry it), so fetch the live list with the fresh token before
    // binding. A fetch failure keeps the preset-based flow intact.
    let live = if provider_id == OPENAI_CODEX_PROVIDER_ID {
        let account = secret
            .account_id
            .clone()
            .or_else(|| mycode_providers::chatgpt_account_id(&secret.access));
        mycode_providers::catalog::codex::fetch_live_models(
            &oauth_call(),
            &secret.access,
            account.as_deref(),
        )
        .await
        .ok()
    } else {
        None
    };
    let bound: Vec<String> = match live.as_ref() {
        Some(live) if !live.is_empty() => {
            let served: Vec<String> = live.iter().map(|model| model.id.clone()).collect();
            let kept: Vec<String> = models
                .iter()
                .filter(|id| served.iter().any(|served| served == *id))
                .cloned()
                .collect();
            // Picks the preset offered but the account does not serve are
            // dropped; none surviving means the whole live list is bound.
            if kept.is_empty() { served } else { kept }
        }
        _ => models.to_vec(),
    };
    let home = state.home.clone();
    let provider_id = provider_id.to_owned();
    let encoded = secret.encode();
    let catalog = state.catalog.clone();
    tokio::task::spawn_blocking(move || {
        save_provider_key(&home, &provider_id, &encoded)?;
        upsert_catalog_provider(&home, &catalog, &provider_id, &bound)?;
        if let Some(live) = live {
            // A store failure must not undo the sign-in; the in-memory
            // merge below still puts the live rows to work this run.
            if let Err(message) = mycode_providers::catalog::codex::write_store(&home, &live) {
                eprintln!("mycode: could not store the Codex model list: {message}");
            }
            install_live_codex_catalog(&catalog, &live)?;
        }
        Ok(())
    })
    .await
    .map_err(|error| format!("sign-in persistence failed: {error}"))?
}

/// Merges a freshly fetched Codex listing into the live catalog row so the
/// model picker shows it without a restart.
fn install_live_codex_catalog(
    catalog: &std::sync::RwLock<mycode_providers::catalog::CachedCatalog>,
    live: &[mycode_providers::catalog::codex::LiveModel],
) -> Result<(), String> {
    let mut guard = catalog
        .write()
        .map_err(|_| "catalog state is unavailable".to_owned())?;
    let mut document = guard.document().as_ref().clone();
    mycode_providers::catalog::codex::attach(&mut document, Some(live));
    let document = Arc::new(document);
    *guard = mycode_providers::catalog::CachedCatalog::from_document(
        document.as_ref().clone(),
        guard.fetched_at,
        guard.etag.clone(),
    );
    drop(guard);
    mycode_providers::catalog::install_active(document);
    Ok(())
}

/// One background refresh of the stored Codex model list, queued once the
/// UI reports ready. A signed-in account keeps its backend-served rows
/// current between sign-ins; every failure stays silent (the preset and
/// any stored list still stand).
pub(crate) fn spawn_codex_models_refresh(state: Arc<CoreState>) {
    tokio::spawn(async move {
        let home = state.home.clone();
        let stored = tokio::task::spawn_blocking(move || {
            mycode_config::read_provider_secrets(&home)
                .ok()
                .and_then(|secrets| {
                    secrets
                        .key(OPENAI_CODEX_PROVIDER_ID)
                        .and_then(parse_oauth_secret)
                })
        })
        .await
        .ok()
        .flatten();
        let Some(mut secret) = stored else {
            return;
        };
        let now = crate::unix_now().as_secs();
        if secret.expired(now)
            && let Ok(fresh) =
                refresh_oauth_secret(&state.home, OPENAI_CODEX_PROVIDER_ID, secret.clone(), now)
                    .await
        {
            secret = fresh;
        }
        let account = secret
            .account_id
            .clone()
            .or_else(|| mycode_providers::chatgpt_account_id(&secret.access));
        let Ok(live) = mycode_providers::catalog::codex::fetch_live_models(
            &oauth_call(),
            &secret.access,
            account.as_deref(),
        )
        .await
        else {
            return;
        };
        let home = state.home.clone();
        let catalog = state.catalog.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = mycode_providers::catalog::codex::write_store(&home, &live);
            let _ = install_live_codex_catalog(&catalog, &live);
        })
        .await;
    });
}

/// Verifies the grant works, stores the OAuth token, and configures the
/// provider entry.
async fn finish_copilot_sign_in(
    state: &CoreState,
    github_token: &str,
    models: &[String],
) -> Result<(), String> {
    let call = oauth_call();
    let bearer = copilot_bearer(&call, github_token).await?;
    *state.copilot.lock().await = Some((bearer.token, bearer.expires_at_unix));
    let home = state.home.clone();
    let github_token = github_token.to_owned();
    let models = models.to_vec();
    let catalog = state.catalog.clone();
    tokio::task::spawn_blocking(move || {
        save_provider_key(&home, COPILOT_PROVIDER_ID, &github_token)?;
        upsert_catalog_provider(&home, &catalog, COPILOT_PROVIDER_ID, &models)
    })
    .await
    .map_err(|error| format!("sign-in persistence failed: {error}"))?
}

/// Adds or refreshes a catalog provider entry with the chosen models.
fn upsert_catalog_provider(
    home: &HomeLayout,
    catalog: &std::sync::RwLock<mycode_providers::catalog::CachedCatalog>,
    provider_id: &str,
    models: &[String],
) -> Result<(), String> {
    let catalog = catalog.read().ok().and_then(|guard| {
        guard.document().provider(provider_id).map(|preset| {
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
    let loaded = load_settings(home)?;
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
            hidden_models: Vec::new(),
        }),
    }
    replace_app_settings(home, revision, &settings).map_err(|error| render_config_error(&error))?;
    Ok(())
}

/// Whether an expired OAuth blob stored for `provider_id` can be refreshed
/// in place. Only Codex holds a refresh grant. Copilot exchanges its device
/// token per request instead, and any other blob (such as one left by the
/// retired xAI sign-in) is refused so a stale access token is not reused.
fn refreshes_oauth(provider_id: &str) -> bool {
    provider_id == OPENAI_CODEX_PROVIDER_ID
}

/// Shown when a stored OAuth blob expired and has no refresh grant here.
const OAUTH_EXPIRED: &str =
    "the saved sign-in expired and cannot be refreshed — paste an API key in Settings";

/// Resolves the Authorization credential and extra headers for one provider
/// request: Copilot exchanges its long-lived token for a live bearer, OAuth
/// providers refresh expiring access tokens in place.
pub(crate) async fn resolve_request_auth(
    home: &HomeLayout,
    copilot: &tokio::sync::Mutex<Option<(String, u64)>>,
    provider: &ProviderSettings,
    stored_key: &str,
) -> Result<(String, Vec<(String, String)>), String> {
    if provider.id == COPILOT_PROVIDER_ID || provider.base_url.contains("githubcopilot.com") {
        let token = ensure_copilot_bearer(copilot, stored_key).await?;
        let extra = COPILOT_CHAT_HEADERS
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        return Ok((token, extra));
    }
    if let Some(mut secret) = parse_oauth_secret(stored_key) {
        let now = crate::unix_now().as_secs();
        if secret.expired(now) {
            secret = refresh_oauth_secret(home, &provider.id, secret, now).await?;
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

/// Serializes OAuth refreshes across turns and subagents.
///
/// Refresh grants rotate: two turns that start together on an expired token
/// both spent the same refresh token, so the slower one failed (or stored the
/// loser of the race). The second waiter now reuses the first one's result.
static OAUTH_REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Refreshes one expired OAuth blob, or adopts a fresher one another turn
/// stored while this one waited for the refresh lock.
async fn refresh_oauth_secret(
    home: &HomeLayout,
    provider_id: &str,
    expired: OAuthSecret,
    now: u64,
) -> Result<OAuthSecret, String> {
    if !refreshes_oauth(provider_id) {
        return Err(OAUTH_EXPIRED.to_owned());
    }
    let _refreshing = OAUTH_REFRESH.lock().await;
    let check_home = home.clone();
    let check_id = provider_id.to_owned();
    let stored = tokio::task::spawn_blocking(move || {
        mycode_config::read_provider_secrets(&check_home)
            .ok()
            .and_then(|secrets| secrets.key(&check_id).and_then(parse_oauth_secret))
    })
    .await
    .ok()
    .flatten();
    if let Some(stored) = stored
        && !stored.expired(now)
    {
        return Ok(stored);
    }
    let secret = refresh_codex_token(&oauth_call(), &expired.refresh).await?;
    let save_home = home.clone();
    let provider_id = provider_id.to_owned();
    let encoded = secret.encode();
    let _ =
        tokio::task::spawn_blocking(move || save_provider_key(&save_home, &provider_id, &encoded))
            .await;
    Ok(secret)
}

/// Returns a live Copilot bearer, exchanging a fresh one when the cached copy
/// is stale.
async fn ensure_copilot_bearer(
    copilot: &tokio::sync::Mutex<Option<(String, u64)>>,
    github_token: &str,
) -> Result<String, String> {
    let now = crate::unix_now().as_secs();
    let mut cache = copilot.lock().await;
    if let Some((token, expires_at)) = cache.as_ref()
        && *expires_at > now.saturating_add(60)
    {
        return Ok(token.clone());
    }
    let call = oauth_call();
    let bearer = copilot_bearer(&call, github_token).await?;
    let token = bearer.token;
    *cache = Some((token.clone(), bearer.expires_at_unix));
    Ok(token)
}

/// Opens the verification page on Windows and macOS. Other targets leave
/// the URL for the sign-in panel; this function does not launch a browser.
///
/// The device-flow server names the URL, so only a plain `https://` URL is
/// launched. Windows hands it to the URL protocol handler directly: `cmd
/// /c start` would parse `&`, `|`, and `^` in the URL as shell syntax.
fn open_browser(url: &str) {
    if !browsable_url(url) {
        return;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", url])
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

/// An `https://` URL with a host and no whitespace, quote, or control
/// character.
fn browsable_url(url: &str) -> bool {
    url.strip_prefix("https://").is_some_and(|rest| {
        !rest.is_empty()
            && !rest.starts_with('/')
            && !rest
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '"')
    })
}

#[cfg(test)]
mod tests {
    use super::refreshes_oauth;
    use mycode_providers::{COPILOT_PROVIDER_ID, OPENAI_CODEX_PROVIDER_ID};

    #[test]
    fn only_plain_https_urls_open_a_browser() {
        assert!(super::browsable_url("https://github.com/login/device"));
        assert!(super::browsable_url(
            "https://auth.openai.com/codex/device?a=1&b=2"
        ));
        assert!(!super::browsable_url("http://github.com/login/device"));
        assert!(!super::browsable_url(
            "file:///C:/Windows/System32/calc.exe"
        ));
        assert!(!super::browsable_url("https://"));
        assert!(!super::browsable_url("https:///etc/passwd"));
        assert!(!super::browsable_url("https://x.example/\" & calc"));
        assert!(!super::browsable_url("https://x.example/\nline"));
    }

    #[test]
    fn expired_oauth_refresh_covers_codex_only() {
        assert!(refreshes_oauth(OPENAI_CODEX_PROVIDER_ID));
        assert!(!refreshes_oauth(COPILOT_PROVIDER_ID));
        // The xAI sign-in was removed; its leftover blobs are not refreshed.
        assert!(!refreshes_oauth("xai"));
        assert!(!refreshes_oauth("unknown-provider"));
    }

    fn scratch_home(label: &str) -> (std::path::PathBuf, mycode_config::HomeLayout) {
        let root = std::env::temp_dir().join(format!(
            "mycode-oauth-{label}-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let home = mycode_config::HomeLayout::from_root(&root).unwrap();
        (root, home)
    }

    fn secret(access: &str, expires: u64) -> mycode_providers::OAuthSecret {
        mycode_providers::OAuthSecret {
            kind: "oauth".to_owned(),
            access: access.to_owned(),
            refresh: format!("{access}-refresh"),
            expires,
            account_id: None,
        }
    }

    fn provider(id: &str, base_url: &str) -> mycode_config::ProviderSettings {
        mycode_config::ProviderSettings {
            id: id.to_owned(),
            kind: "openai-responses".to_owned(),
            base_url: base_url.to_owned(),
            models: vec!["model".to_owned()],
            enabled: true,
            context_limit: None,
            max_output: None,
            hidden_models: Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_waiting_refresh_adopts_the_token_another_turn_stored() {
        let (root, home) = scratch_home("adopt");
        let now = crate::unix_now().as_secs();
        // The turn that won the race already stored a rotated token.
        crate::settings_io::save_provider_key(
            &home,
            OPENAI_CODEX_PROVIDER_ID,
            &secret("rotated", now + 3600).encode(),
        )
        .unwrap();
        let codex = provider(
            OPENAI_CODEX_PROVIDER_ID,
            "https://chatgpt.com/backend-api/codex",
        );
        // This turn read the old token before the race; it must not spend
        // the already rotated refresh grant.
        let adopted =
            super::refresh_oauth_secret(&home, &codex.id, secret("stale", now), now)
                .await
                .expect("no network refresh");
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(adopted.access, "rotated");
    }

    #[tokio::test]
    async fn an_expired_retired_xai_sign_in_asks_for_an_api_key() {
        let (root, home) = scratch_home("xai");
        let now = crate::unix_now().as_secs();
        let xai = provider("xai", "https://api.x.ai/v1");
        let stale = secret("old-grok", now).encode();
        let copilot = tokio::sync::Mutex::new(None);
        let outcome = super::resolve_request_auth(&home, &copilot, &xai, &stale).await;
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(outcome, Err(super::OAUTH_EXPIRED.to_owned()));
    }
}
