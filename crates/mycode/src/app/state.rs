//! Shared per-process core state owned by the bridge thread, plus the
//! gateway-model and update-check background refreshes that run against it.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::agent::session::SessionId;
use crate::agent::session::SessionService;
use crate::config::{
    DocumentRepair, HomeLayout, ProviderSettings, read_app_settings, read_ui_state,
    read_ui_state_with_repair,
};
use crate::providers::catalog::CatalogDocument;
use tokio_util::sync::CancellationToken;

use crate::app::settings_io::render_config_error;
use crate::app::{BridgeEvent, BridgeReply, CatalogInfo};

/// User-Agent for GitHub update checks.
pub(crate) const UPDATE_USER_AGENT: &str = concat!("mycode-updates/", env!("CARGO_PKG_VERSION"));

/// Shared per-process core state owned by the bridge thread.
pub(crate) struct CoreState {
    pub(crate) service: SessionService,
    pub(crate) home: HomeLayout,
    /// The model list magpie's gateway published, swapped on every refresh.
    /// An empty document means the gateway has not answered yet.
    pub(crate) catalog: Arc<RwLock<Arc<CatalogDocument>>>,
    /// Unix seconds of the last successful gateway read; 0 before the first.
    pub(crate) catalog_fetched_at: Arc<AtomicU64>,
    /// Only the most recently started gateway refresh may publish its result.
    /// The lock also keeps generation checks and publication together.
    catalog_refresh_generation: Mutex<u64>,
    /// Per-session project directories for tool runs.
    pub(crate) projects: Arc<Mutex<HashMap<String, PathBuf>>>,
    /// Live turn and `/compact` tokens by session id; Escape targets these.
    pub(crate) turn_cancels: Arc<Mutex<HashMap<String, LiveWork>>>,
    /// One token per running subagent, keyed by `session_id:call_id`.
    pub(crate) subagent_cancels: Arc<Mutex<HashMap<String, Arc<CancellationToken>>>>,
    /// Bounded run records partitioned by session and reused across turns.
    pub(crate) agent_runtimes: Mutex<HashMap<String, Arc<crate::tools::builtin::AgentRuntime>>>,
    /// Steer inbox for the live turn of each session. Removed when the turn ends.
    pub(crate) steer_inboxes: Arc<Mutex<HashMap<String, Arc<crate::agent::SteerInbox>>>>,
    /// Provider tokens per estimated token, by session, as its last turn
    /// measured them. A turn's first request has no reply of its own yet.
    token_scales: Mutex<HashMap<String, crate::app::compaction::TokenScale>>,
    /// Live MCP clients, reused until settings change or a connection dies.
    pub(crate) mcp_pool: Arc<tokio::sync::Mutex<Option<crate::app::mcp_tools::McpPool>>>,
    /// Code-mode script cells; they outlive the turn that started them so
    /// `wait_code` can collect them across turns.
    pub(crate) code_cells: Arc<crate::tools::CodeCells>,
    /// Documents reset before the UI asked for settings or UI state.
    startup_repairs: Mutex<Vec<DocumentRepair>>,
    /// Orders whole-document `ui.json` replacements. Each save runs on the
    /// blocking pool; without this an older snapshot dispatched first could
    /// finish last and overwrite the newer one. The tokio mutex is FIFO, so
    /// writes land in the order the frontend sent them.
    pub(crate) ui_state_writes: tokio::sync::Mutex<()>,
}

/// One session's live work in [`CoreState::turn_cancels`]: the token a
/// cancel fires, and whether a reopen should show a turn or a `/compact`.
#[derive(Clone)]
pub(crate) struct LiveWork {
    pub(crate) token: Arc<CancellationToken>,
    pub(crate) kind: WorkKind,
}

impl LiveWork {
    pub(crate) fn cancel(&self) {
        self.token.cancel();
    }
}

/// What a session's live work is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkKind {
    Turn,
    Compaction,
}

impl CoreState {
    pub(crate) fn new(home: HomeLayout, mut startup_repairs: Vec<DocumentRepair>) -> Self {
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
            catalog: Arc::new(RwLock::new(Arc::new(CatalogDocument::default()))),
            catalog_fetched_at: Arc::new(AtomicU64::new(0)),
            catalog_refresh_generation: Mutex::new(0),
            home,
            projects: Arc::new(Mutex::new(projects)),
            turn_cancels: Arc::new(Mutex::new(HashMap::new())),
            subagent_cancels: Arc::new(Mutex::new(HashMap::new())),
            agent_runtimes: Mutex::new(HashMap::new()),
            steer_inboxes: Arc::new(Mutex::new(HashMap::new())),
            token_scales: Mutex::new(HashMap::new()),
            mcp_pool: Arc::new(tokio::sync::Mutex::new(None)),
            code_cells: Arc::new(crate::tools::CodeCells::new()),
            startup_repairs: Mutex::new(startup_repairs),
            ui_state_writes: tokio::sync::Mutex::new(()),
        }
    }

    /// What the session's live work is doing, when something is running.
    pub(crate) fn live_work(&self, session_id: &str) -> Option<WorkKind> {
        self.turn_cancels
            .lock()
            .get(session_id)
            .map(|work| work.kind)
    }

    pub(crate) fn token_scale(&self, session_id: &str) -> crate::app::compaction::TokenScale {
        self.token_scales
            .lock()
            .get(session_id)
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn remember_token_scale(
        &self,
        session_id: &str,
        scale: crate::app::compaction::TokenScale,
    ) {
        self.token_scales
            .lock()
            .insert(session_id.to_owned(), scale);
    }

    pub(crate) fn forget_token_scale(&self, session_id: &str) {
        self.token_scales.lock().remove(session_id);
    }

    /// Copies startup repair notices. Later loads still need the same list.
    pub(crate) fn startup_repairs_snapshot(&self) -> Vec<DocumentRepair> {
        self.startup_repairs.lock().clone()
    }

    pub(crate) fn project_dir(&self, session_id: &str) -> PathBuf {
        self.projects
            .lock()
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| self.home.root().join(crate::config::SCRATCH_DIR))
    }

    /// The model list the picker and the turn planner read.
    pub(crate) fn models(&self) -> Arc<CatalogDocument> {
        self.catalog
            .read()
            .map(|guard| Arc::clone(&guard))
            .unwrap_or_default()
    }
}

/// Validates and binds one project directory to a session.
pub(crate) fn set_project_dir(
    state: &CoreState,
    session_id: &str,
    path: Option<&str>,
) -> Result<(), String> {
    let mut projects = state.projects.lock();
    let Some(path) = path else {
        projects.remove(session_id);
        return Ok(());
    };
    let directory = PathBuf::from(path);
    if !directory.is_absolute() || !directory.is_dir() {
        return Err("choose an existing directory".to_owned());
    }
    projects.insert(session_id.to_owned(), directory);
    Ok(())
}

/// The catalog plus the time the gateway answered.
pub(crate) fn catalog_info(state: &CoreState) -> CatalogInfo {
    CatalogInfo {
        document: state.models(),
        fetched_at: state.catalog_fetched_at.load(Ordering::Relaxed),
    }
}

/// Reads magpie's `/v1/models` and publishes the new list.
///
/// A missing or disabled binding clears the list; an offline gateway keeps
/// the previous one and reports why, so a running session is not emptied by
/// a transient failure.
pub(crate) async fn refresh_catalog(
    state: &CoreState,
    events: &crate::app::BridgeEventTx,
    _force: bool,
) -> BridgeReply {
    let generation = {
        let mut current = state.catalog_refresh_generation.lock();
        *current = current.wrapping_add(1);
        *current
    };
    let read_home = state.home.clone();
    let loaded = tokio::task::spawn_blocking(move || read_app_settings(&read_home)).await;
    let gateway = {
        let current = state.catalog_refresh_generation.lock();
        if *current != generation {
            return BridgeReply::Catalog(Ok(catalog_info(state)));
        }
        let settings = match loaded {
            Ok(Ok(settings)) => settings,
            Ok(Err(error)) => return BridgeReply::Catalog(Err(render_config_error(&error))),
            Err(error) => {
                return BridgeReply::Catalog(Err(format!("settings task failed: {error}")));
            }
        };
        crate::app::settings_io::apply_network(&settings);
        if !settings.magpie.is_active() {
            // A missing or disabled binding is a real error for a manual
            // refresh: the Models page toast would otherwise report success
            // while the list it asked for was just cleared.
            let message = if settings.magpie.is_default() {
                "magpie is not connected: install the agent plugin and pick a model in magpie"
                    .to_owned()
            } else {
                "magpie is not connected: pick a model in magpie first".to_owned()
            };
            if let Ok(mut guard) = state.catalog.write() {
                *guard = Arc::new(CatalogDocument::default());
            }
            state.catalog_fetched_at.store(0, Ordering::Relaxed);
            let _ = events.send(BridgeEvent::CatalogUpdated {
                providers: 0,
                fetched_at: 0,
            });
            return BridgeReply::Catalog(Err(message));
        }
        match crate::app::magpie::Gateway::from_settings(&settings.magpie) {
            Ok(gateway) => gateway,
            Err(message) => return BridgeReply::Catalog(Err(message)),
        }
    };
    let fetched = gateway.fetch_models().await;
    let current = state.catalog_refresh_generation.lock();
    if *current != generation {
        // A slow response (including a failure) from an old binding cannot
        // replace the new list or return stale models to the frontend.
        return BridgeReply::Catalog(Ok(catalog_info(state)));
    }
    match fetched {
        Ok(fetched) => {
            let document = Arc::new(crate::providers::catalog::magpie::document_from_provider(
                fetched.provider,
            ));
            let providers = document.providers.len();
            if let Ok(mut guard) = state.catalog.write() {
                *guard = Arc::clone(&document);
            }
            state
                .catalog_fetched_at
                .store(fetched.fetched_at, Ordering::Relaxed);
            let _ = events.send(BridgeEvent::CatalogUpdated {
                providers,
                fetched_at: fetched.fetched_at,
            });
            BridgeReply::Catalog(Ok(CatalogInfo {
                document,
                fetched_at: fetched.fetched_at,
            }))
        }
        Err(message) => BridgeReply::Catalog(Err(message)),
    }
}

/// One background gateway read, queued once the UI reports ready. A failure
/// leaves the previous list in place and stays quiet: the Models page and the
/// turn report the gateway error when they need it.
pub(crate) fn spawn_catalog_refresh(state: Arc<CoreState>, events: crate::app::BridgeEventTx) {
    tokio::spawn(async move {
        refresh_catalog(&state, &events, false).await;
    });
}

/// One background update check, queued once the UI reports ready.
pub(crate) fn spawn_update_check(state: Arc<CoreState>, events: crate::app::BridgeEventTx) {
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
            startup_update_event(crate::app::updates::latest_release(UPDATE_USER_AGENT).await)
        {
            let _ = events.send(event);
        }
    });
}

/// Turns one startup check into an event. `Ok(None)` stays quiet (current or
/// an unpublished target). A transport or pin failure must be visible.
fn startup_update_event(
    result: Result<Option<crate::app::updates::UpdateOffer>, String>,
) -> Option<crate::app::BridgeEvent> {
    match result {
        Ok(Some(offer)) => Some(crate::app::BridgeEvent::UpdateAvailable { offer }),
        Ok(None) => None,
        Err(message) => Some(crate::app::BridgeEvent::UpdateCheckFailed { message }),
    }
}

/// Gateway or settings context window for compaction. Zero means the
/// Codex-style fallback threshold.
pub(crate) fn model_context_window(
    state: &CoreState,
    provider: &ProviderSettings,
    model: &str,
) -> u64 {
    catalog_context_window(&state.catalog, provider, model)
}

/// [`model_context_window`] against a catalog handle, for callers (a
/// subagent host) that hold the catalog but not the whole core state.
pub(crate) fn catalog_context_window(
    catalog: &RwLock<Arc<CatalogDocument>>,
    provider: &ProviderSettings,
    model: &str,
) -> u64 {
    if let Some(limit) = provider.context_limit.filter(|tokens| *tokens > 0) {
        return limit;
    }
    let Ok(catalog) = catalog.read() else {
        return 0;
    };
    catalog
        .model_for_endpoint(&provider.id, Some(provider.base_url.as_str()), model)
        .map(|entry| entry.context)
        .filter(|context| *context > 0)
        .unwrap_or(0)
}

/// Output-token cap for one turn.
///
/// A configured `maxOutput` wins, then the gateway's `max_output_tokens`,
/// then the Messages fallback of 32_000 (`OUTPUT_TOKEN_MAX` in OpenCode).
pub(crate) fn model_output_limit(
    state: &CoreState,
    provider: &ProviderSettings,
    model: &str,
) -> Option<u64> {
    if let Some(limit) = provider.max_output.filter(|tokens| *tokens > 0) {
        return Some(limit);
    }
    if let Ok(catalog) = state.catalog.read()
        && let Some(limit) = catalog
            .model_for_endpoint(&provider.id, Some(provider.base_url.as_str()), model)
            .map(|entry| entry.output)
            .filter(|output| *output > 0)
    {
        return Some(limit);
    }
    Some(32_000)
}

#[cfg(test)]
mod tests {
    use super::startup_update_event;
    use crate::app::BridgeEvent;
    use crate::app::updates::UpdateOffer;

    async fn held_gateway(
        model: &str,
    ) -> (
        String,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("gateway listener");
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let body = format!(r#"{{"data":[{{"id":"{model}"}}]}}"#);
        let (started, requested) = tokio::sync::oneshot::channel();
        let (release, ready) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("gateway request");
            let mut request = Vec::new();
            while !request.windows(4).any(|chunk| chunk == b"\r\n\r\n") {
                let mut chunk = [0; 1024];
                let count = stream.read(&mut chunk).await.expect("HTTP headers");
                assert!(count > 0 && request.len() < 16 * 1024);
                request.extend_from_slice(&chunk[..count]);
            }
            started.send(()).expect("request observer");
            ready.await.expect("response release");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("response");
        });
        (base_url, requested, release, worker)
    }

    #[tokio::test]
    async fn older_gateway_responses_cannot_replace_a_new_binding_or_reconnect_a_disabled_one() {
        for disabled in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "mycode-catalog-generation-{}-{}",
                std::process::id(),
                crate::core::unix_now().as_nanos()
            ));
            std::fs::create_dir_all(&root).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let home = crate::config::HomeLayout::from_root(&root).unwrap();
            let state = std::sync::Arc::new(super::CoreState::new(home, Vec::new()));
            let write_binding = |url: Option<&str>| {
                let mut settings = crate::config::AppSettings::default();
                if let Some(url) = url {
                    settings.magpie.enabled = true;
                    settings.magpie.base_url = url.into();
                    settings.magpie.api_key = "fixture-key".into();
                    settings.magpie.model = "selected".into();
                }
                std::fs::write(
                    root.join("settings.json"),
                    serde_json::to_vec(&settings).unwrap(),
                )
                .unwrap();
            };
            let (old_url, old_requested, release_old, old_gateway) = held_gateway("old").await;
            write_binding(Some(&old_url));
            let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
            let old_refresh = {
                let state = state.clone();
                let events = events.clone();
                tokio::spawn(async move { super::refresh_catalog(&state, &events, true).await })
            };
            tokio::time::timeout(std::time::Duration::from_secs(5), old_requested)
                .await
                .expect("old request started")
                .unwrap();
            let newer = if disabled {
                write_binding(None);
                super::refresh_catalog(&state, &events, true).await
            } else {
                let (new_url, requested, release, gateway) = held_gateway("new").await;
                write_binding(Some(&new_url));
                release.send(()).unwrap();
                let reply = super::refresh_catalog(&state, &events, true).await;
                requested.await.unwrap();
                gateway.await.unwrap();
                reply
            };
            assert_eq!(
                matches!(newer, crate::app::BridgeReply::Catalog(Err(_))),
                disabled
            );
            release_old.send(()).unwrap();
            let crate::app::BridgeReply::Catalog(Ok(stale_reply)) = old_refresh.await.unwrap()
            else {
                panic!("superseded refresh should return the current catalog");
            };
            old_gateway.await.unwrap();
            assert_eq!(stale_reply.document, state.models());
            if disabled {
                assert!(state.models().providers.is_empty());
                assert_eq!(stale_reply.fetched_at, 0);
            } else {
                assert_eq!(state.models().providers[0].models[0].id, "new");
            }
            assert!(matches!(
                received.try_recv(),
                Ok(BridgeEvent::CatalogUpdated { .. })
            ));
            assert!(
                received.try_recv().is_err(),
                "stale refresh must not publish an event"
            );
            std::sync::Arc::try_unwrap(state)
                .unwrap_or_else(|_| panic!("refresh tasks released state"))
                .service
                .shutdown()
                .await;
            std::fs::remove_dir_all(root).unwrap();
        }
    }

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
            asset_url: "https://github.com/MCapricorns/mycode/releases/download/v0.9.18/mycode-desktop-v0.9.18-x86_64-pc-windows-msvc.zip".to_owned(),
            checksum_url: "https://github.com/example/app.zip.sha256".to_owned(),
        };
        assert!(matches!(
            startup_update_event(Ok(Some(offer))),
            Some(BridgeEvent::UpdateAvailable { .. })
        ));
    }

    #[tokio::test]
    async fn max_output_prefers_the_setting_then_the_catalog_then_32000() {
        use crate::config::{HomeLayout, ProviderSettings};
        use crate::providers::catalog::{CatalogDocument, CatalogModel, CatalogProvider};

        let root = std::env::temp_dir().join(format!(
            "mycode-max-output-{}-{}",
            std::process::id(),
            crate::core::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("root");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
        }
        let home = HomeLayout::from_root(&root).expect("home");
        let state = super::CoreState::new(home, Vec::new());
        *state.catalog.write().expect("catalog") = std::sync::Arc::new(CatalogDocument {
            providers: vec![CatalogProvider {
                id: "magpie".to_owned(),
                models: vec![CatalogModel {
                    id: "listed".to_owned(),
                    output: 8_000,
                    ..CatalogModel::default()
                }],
                ..CatalogProvider::default()
            }],
        });
        let mut provider = ProviderSettings::magpie(
            "http://127.0.0.1:3425/v1",
            vec!["listed".to_owned(), "custom".to_owned()],
            Vec::new(),
        );
        provider.max_output = Some(4_096);
        assert_eq!(
            super::model_output_limit(&state, &provider, "listed"),
            Some(4_096)
        );
        provider.max_output = None;
        assert_eq!(
            super::model_output_limit(&state, &provider, "listed"),
            Some(8_000)
        );
        assert_eq!(
            super::model_output_limit(&state, &provider, "custom"),
            Some(32_000)
        );
        provider.max_output = Some(0);
        assert_eq!(
            super::model_output_limit(&state, &provider, "custom"),
            Some(32_000)
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
