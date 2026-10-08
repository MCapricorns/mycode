//! Shared per-process core state owned by the bridge thread, plus the
//! catalog and update-check background refreshes that run against it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;

use mycode_agent::session::SessionId;
use mycode_agent::session::SessionService;
use mycode_config::{
    DocumentRepair, HomeLayout, ProviderSettings, read_app_settings, read_ui_state,
    read_ui_state_with_repair,
};
use mycode_providers::catalog::{CachedCatalog, RefreshOutcome};
use tokio_util::sync::CancellationToken;

use crate::settings_io::render_config_error;
use crate::{BridgeEvent, BridgeReply, CatalogInfo};

/// Update checks identify the app to GitHub's API.
pub(crate) const UPDATE_USER_AGENT: &str = concat!("mycode-updates/", env!("CARGO_PKG_VERSION"));

/// Shared per-process core state owned by the bridge thread.
pub(crate) struct CoreState {
    pub(crate) service: SessionService,
    pub(crate) home: HomeLayout,
    /// Resolved provider catalog; swapped in place by refreshes.
    pub(crate) catalog: Arc<RwLock<CatalogInfo>>,
    /// Per-session project directories for tool runs.
    pub(crate) projects: Arc<Mutex<HashMap<String, PathBuf>>>,
    /// Cached short-lived Copilot bearer token and its unix expiry.
    pub(crate) copilot: Arc<tokio::sync::Mutex<Option<(String, u64)>>>,
    /// Live turn cancellation tokens by session id; Escape targets these.
    pub(crate) turn_cancels: Arc<Mutex<HashMap<String, Arc<CancellationToken>>>>,
    /// One token per running subagent, keyed by `session_id:call_id`.
    pub(crate) subagent_cancels: Arc<Mutex<HashMap<String, CancellationToken>>>,
    /// Live MCP clients, reused until settings change or a connection dies.
    pub(crate) mcp_pool: Arc<tokio::sync::Mutex<Option<crate::mcp_tools::McpPool>>>,
    /// Documents reset before the UI asked for settings or UI state.
    startup_repairs: Mutex<Vec<DocumentRepair>>,
}

impl CoreState {
    pub(crate) fn new(
        home: HomeLayout,
        cached: CachedCatalog,
        mut startup_repairs: Vec<DocumentRepair>,
    ) -> Self {
        // Session→project bindings survive restarts through the durable UI
        // state; seed the in-memory map so tool working directories resolve
        // before the desktop re-binds anything. A damaged `ui.json` is reset
        // here, and the notice is kept for the first UI load.
        let mut projects = HashMap::new();
        if let Ok((ui_state, repair)) = read_ui_state_with_repair(&home) {
            if let Some(repair) = repair {
                startup_repairs.push(repair);
            }
            for (session_id, project) in &ui_state.session_projects {
                if let Some(session) = SessionId::parse(session_id) {
                    projects.insert(session.as_str().to_owned(), PathBuf::from(project));
                }
            }
        }
        Self {
            service: SessionService::new(&home),
            catalog: Arc::new(RwLock::new(CatalogInfo {
                document: Arc::new(cached.document),
                fetched_at: cached.fetched_at,
            })),
            home,
            projects: Arc::new(Mutex::new(projects)),
            copilot: Arc::new(tokio::sync::Mutex::new(None)),
            turn_cancels: Arc::new(Mutex::new(HashMap::new())),
            subagent_cancels: Arc::new(Mutex::new(HashMap::new())),
            mcp_pool: Arc::new(tokio::sync::Mutex::new(None)),
            startup_repairs: Mutex::new(startup_repairs),
        }
    }

    /// Copies startup repair notices. Later loads still need the same list.
    pub(crate) fn startup_repairs_snapshot(&self) -> Vec<DocumentRepair> {
        self.startup_repairs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The tool working directory for one session.
    pub(crate) fn project_dir(&self, session_id: &str) -> PathBuf {
        self.projects
            .lock()
            .expect("projects")
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| self.home.root().join(mycode_config::SCRATCH_DIR))
    }
}

/// Validates and binds one project directory to a session.
pub(crate) fn set_project_dir(
    state: &CoreState,
    session_id: &str,
    path: Option<&str>,
) -> Result<(), String> {
    let Some(path) = path else {
        state.projects.lock().expect("projects").remove(session_id);
        return Ok(());
    };
    let directory = PathBuf::from(path);
    if !directory.is_absolute() || !directory.is_dir() {
        return Err("choose an existing directory".to_owned());
    }
    state
        .projects
        .lock()
        .expect("projects")
        .insert(session_id.to_owned(), directory);
    Ok(())
}

pub(crate) fn catalog_info_from(cache: CachedCatalog) -> CatalogInfo {
    CatalogInfo {
        document: Arc::new(cache.document),
        fetched_at: cache.fetched_at,
    }
}

/// Refreshes the provider catalog and reports a successful swap.
pub(crate) async fn refresh_catalog(
    state: &CoreState,
    events: &crate::BridgeEventTx,
    force: bool,
) -> BridgeReply {
    let settings = match read_app_settings(&state.home) {
        Ok(settings) => settings,
        Err(error) => return BridgeReply::Catalog(Err(render_config_error(&error))),
    };
    let outcome = mycode_providers::catalog::refresh(
        &state.home,
        &settings.effective_user_agent(),
        force,
        mycode_providers::catalog::DEFAULT_MAX_AGE_SECS,
    )
    .await;
    match outcome {
        RefreshOutcome::Fresh(cache) | RefreshOutcome::NotModified(cache) => {
            BridgeReply::Catalog(Ok(catalog_info_from(cache)))
        }
        RefreshOutcome::Updated(cache) => {
            let info = catalog_info_from(cache);
            let providers = info.document.providers.len();
            let fetched_at = info.fetched_at;
            if let Ok(mut guard) = state.catalog.write() {
                *guard = info.clone();
            }
            let _ = events.try_send(BridgeEvent::CatalogUpdated {
                providers,
                fetched_at,
            });
            BridgeReply::Catalog(Ok(info))
        }
        RefreshOutcome::Unavailable(message) => BridgeReply::Catalog(Err(message)),
    }
}

/// One background catalog refresh shortly after startup.
pub(crate) fn spawn_catalog_refresh(state: Arc<CoreState>, events: crate::BridgeEventTx) {
    tokio::spawn(async move {
        refresh_catalog(&state, &events, false).await;
    });
}

/// One background update check shortly after startup.
pub(crate) fn spawn_update_check(state: Arc<CoreState>, events: crate::BridgeEventTx) {
    tokio::spawn(async move {
        let home = state.home.clone();
        let Ok(Ok(ui_state)) = tokio::task::spawn_blocking(move || read_ui_state(&home)).await
        else {
            return;
        };
        if !ui_state.auto_update {
            return;
        }
        if let Some(event) =
            startup_update_event(crate::updates::latest_release(UPDATE_USER_AGENT).await)
        {
            let _ = events.try_send(event);
        }
    });
}

/// Turns one startup check into an event. `Ok(None)` stays quiet (current or
/// an unpublished target). A transport or pin failure must be visible.
fn startup_update_event(
    result: Result<Option<crate::updates::UpdateOffer>, String>,
) -> Option<crate::BridgeEvent> {
    match result {
        Ok(Some(offer)) => Some(crate::BridgeEvent::UpdateAvailable { offer }),
        Ok(None) => None,
        Err(message) => Some(crate::BridgeEvent::UpdateCheckFailed { message }),
    }
}

/// Catalog or settings context window for compaction. Zero means the
/// Codex-style fallback threshold.
pub(crate) fn model_context_window(
    state: &CoreState,
    provider: &ProviderSettings,
    model: &str,
) -> u64 {
    if let Some(limit) = provider.context_limit.filter(|tokens| *tokens > 0) {
        return limit;
    }
    let Ok(catalog) = state.catalog.read() else {
        return 0;
    };
    let document = &catalog.document;
    document
        .provider(&provider.id)
        .or_else(|| {
            document
                .providers
                .iter()
                .find(|item| item.base_url == provider.base_url)
        })
        .and_then(|item| item.models.iter().find(|entry| entry.id == model))
        .map(|entry| entry.context)
        .filter(|context| *context > 0)
        .unwrap_or(0)
}

/// models.dev `limit.output` for this model. `None` when the catalog does
/// not publish an output cap; callers must not substitute a fixed 4096.
pub(crate) fn model_output_limit(
    state: &CoreState,
    provider: &ProviderSettings,
    model: &str,
) -> Option<u64> {
    let Ok(catalog) = state.catalog.read() else {
        return None;
    };
    let document = &catalog.document;
    document
        .provider(&provider.id)
        .or_else(|| {
            document
                .providers
                .iter()
                .find(|item| item.base_url == provider.base_url)
        })
        .and_then(|item| item.models.iter().find(|entry| entry.id == model))
        .map(|entry| entry.output)
        .filter(|output| *output > 0)
}

#[cfg(test)]
mod tests {
    use super::startup_update_event;
    use crate::BridgeEvent;
    use crate::updates::UpdateOffer;

    #[test]
    fn startup_check_failure_is_an_event_and_current_is_quiet() {
        let failed = startup_update_event(Err(
            "update check failed: resolved addresses are not all public".to_owned(),
        ));
        match failed {
            Some(BridgeEvent::UpdateCheckFailed { message }) => {
                assert!(message.contains("not all public"), "{message}");
            }
            other => panic!("startup failure was swallowed: {other:?}"),
        }
        assert!(startup_update_event(Ok(None)).is_none());
        let offer = UpdateOffer {
            version: "0.9.18".to_owned(),
            notes_url: "https://github.com/MCapricorns/mycode/releases/tag/v0.9.18".to_owned(),
            asset_url: "https://github.com/MCapricorns/mycode/releases/download/v0.9.18/mycode-desktop-v0.9.18-x86_64-unknown-linux-gnu.zip".to_owned(),
            asset_size: 1,
            checksum_url: "https://github.com/example/app.zip.sha256".to_owned(),
        };
        assert!(matches!(
            startup_update_event(Ok(Some(offer))),
            Some(BridgeEvent::UpdateAvailable { .. })
        ));
    }
}
