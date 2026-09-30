//! Shared per-process core state owned by the bridge thread, plus the
//! catalog and update-check background refreshes that run against it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;

use mycode_agent::session::SessionId;
use mycode_agent::session::SessionService;
use mycode_config::{HomeLayout, ProviderSettings, read_app_settings, read_ui_state};
use mycode_providers::catalog::{CachedCatalog, RefreshOutcome, http_client};
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
}

impl CoreState {
    pub(crate) fn new(home: HomeLayout, cached: CachedCatalog) -> Self {
        // Session→project bindings survive restarts through the durable UI
        // state; seed the in-memory map so tool working directories resolve
        // before the desktop re-binds anything.
        let mut projects = HashMap::new();
        if let Ok(ui_state) = read_ui_state(&home) {
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
        }
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
    let client = match http_client(&settings.effective_user_agent()) {
        Ok(client) => client,
        Err(message) => return BridgeReply::Catalog(Err(message)),
    };
    let outcome = mycode_providers::catalog::refresh(
        &state.home,
        &client,
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
        let Ok(client) = http_client(UPDATE_USER_AGENT) else {
            return;
        };
        if let Ok(Some(offer)) = crate::updates::latest_release(&client).await {
            let _ = events.try_send(BridgeEvent::UpdateAvailable { offer });
        }
    });
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
