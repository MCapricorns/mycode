//! Gateway lifecycle regressions use only a scratch home and synthetic binding.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::app::{BridgeReply, SettingsSnapshot};
use crate::config::{AppSettings, AuthorityRevision};

use super::{conversation, scratch_driver};
use crate::frontend::driver::{Effect, FrontendDriver, ReplyScope, ToastKind};
use crate::frontend::view_model::{FrontendAction, SettingsState, reduce};

struct GatewayFixture {
    base_url: String,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl GatewayFixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        listener.set_nonblocking(true).expect("nonblocking");
        let base_url = format!("http://{}/v1", listener.local_addr().expect("address"));
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            let body = r#"{"object":"list","data":[
                {"id":"a/other","context_window":64000},
                {"id":"p/pinned","context_window":128000},
                {"id":"z/default","context_window":256000}
            ]}"#;
            'requests: while !stopped.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                };
                stream.set_nonblocking(false).expect("blocking connection");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("read timeout");
                let mut request = Vec::new();
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let mut chunk = [0; 1024];
                    let count = match stream.read(&mut chunk) {
                        // Shutdown can cancel an automatic refresh after TCP
                        // connects but before its request has been written.
                        Ok(0) => continue 'requests,
                        Ok(count) => count,
                        Err(error) if disconnected(&error) => continue 'requests,
                        Err(error) => panic!("request: {error}"),
                    };
                    assert!(request.len() + count <= 16 * 1024, "HTTP headers");
                    request.extend_from_slice(&chunk[..count]);
                }
                assert!(request.starts_with(b"GET /v1/models HTTP/1.1\r\n"));
                if let Err(error) = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ) {
                    assert!(disconnected(&error), "response: {error}");
                }
            }
        });
        Self {
            base_url,
            stop,
            worker: Some(worker),
        }
    }

    /// Match the plugin's first-create shape: valid binding, no envelope.
    fn write_binding(&self, root: &std::path::Path, model: &str) {
        let body = format!(
            r#"{{"magpie":{{"enabled":true,"baseUrl":"{}","apiKey":"fixture-key","model":"{model}"}},"appearance":{{"theme":"dark","language":"en","fontSize":"m"}}}}"#,
            self.base_url
        );
        std::fs::write(root.join("settings.json"), body).expect("synthetic binding");
    }
}

fn disconnected(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::UnexpectedEof
    )
}

impl Drop for GatewayFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let result = self.worker.take().expect("worker").join();
        if !std::thread::panicking() {
            result.expect("gateway");
        }
    }
}

fn loaded(settings: AppSettings, revision: u64) -> BridgeReply {
    BridgeReply::Settings(Ok(SettingsSnapshot {
        settings,
        revision: AuthorityRevision::new(revision).unwrap_or(AuthorityRevision::ABSENT),
        provider_keys: Vec::new(),
        mcp_keys: Vec::new(),
        repairs: Vec::new(),
    }))
}

#[test]
fn magpie_install_notice_waits_for_both_startup_documents() {
    for active in [false, true] {
        for ui_first in [false, true] {
            let (mut driver, root) = scratch_driver("magpie-startup-order");
            let mut settings = AppSettings::default();
            if active {
                settings.magpie.enabled = true;
                settings.magpie.base_url = "http://127.0.0.1:3425/v1".to_owned();
                settings.magpie.api_key = "fixture-key".to_owned();
                settings.magpie.model = "provider/model".to_owned();
            }
            let ui = BridgeReply::UiState(Ok((crate::config::UiState::default(), Vec::new())));
            let settings = loaded(settings, 0);
            let [first, second] = if ui_first {
                [ui, settings]
            } else {
                [settings, ui]
            };
            let mut out = Vec::new();
            driver.on_reply(ReplyScope::Global, first, &mut out);
            assert!(!driver.magpie_prompted);
            assert!(
                !out.iter()
                    .any(|effect| matches!(effect, Effect::Toast { .. }))
            );

            driver.on_reply(ReplyScope::Global, second, &mut out);
            assert_eq!(driver.magpie_prompted, !active);
            assert_eq!(
                out.iter()
                    .filter(|effect| matches!(
                        effect,
                        Effect::Toast {
                            kind: ToastKind::Warning,
                            ..
                        }
                    ))
                    .count(),
                usize::from(!active)
            );
            out.clear();
            driver.prompt_magpie_install(&mut out);
            assert!(out.is_empty(), "the notice is shown only once");
            driver.shutdown();
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn poll_until(
    driver: &mut FrontendDriver,
    out: &mut Vec<Effect>,
    ready: impl Fn(&FrontendDriver) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        driver.poll(out);
        while let Some(event) = driver.next_event() {
            driver.on_event(event, out);
        }
        if ready(driver) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "catalog lifecycle did not settle"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_watched_gateway_binding_refreshes_models_without_opening_a_menu() {
    let (mut driver, root) = scratch_driver("watched-gateway");
    let mut out = Vec::new();
    driver.on_reply(
        ReplyScope::Global,
        loaded(AppSettings::default(), 0),
        &mut out,
    );
    driver.watch_library();
    let gateway = GatewayFixture::start();
    gateway.write_binding(&root, "p/pinned");
    poll_until(&mut driver, &mut out, |driver| {
        driver.vm.catalog.as_ref().is_some_and(|catalog| {
            catalog
                .provider("magpie")
                .is_some_and(|provider| provider.models.iter().any(|model| model.id == "p/pinned"))
        })
    });
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn settings_reload_refreshes_library_without_opening_settings() {
    let (mut driver, root) = scratch_driver("library-refresh");
    let mut out = Vec::new();
    driver.on_reply(
        ReplyScope::Global,
        loaded(AppSettings::default(), 0),
        &mut out,
    );
    assert!(driver.vm.skills.is_empty());
    assert!(driver.vm.mcp_library.is_empty());

    let skill_dir = root.join("skills").join("sample");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "# Sample skill\n").unwrap();
    std::fs::write(
        root.join("mcp.json"),
        r#"{"mcpServers":{"magpie":{"url":"http://localhost:3425/mcp/magpie/hub/mycode"}}}"#,
    )
    .unwrap();
    driver.on_reply(
        ReplyScope::Global,
        loaded(AppSettings::default(), 0),
        &mut out,
    );
    assert_eq!(driver.vm.skills.len(), 1);
    assert_eq!(driver.vm.skills[0].slug, "sample");
    assert_eq!(driver.vm.mcp_library.len(), 1);
    assert_eq!(driver.vm.mcp_library[0].id, "magpie");
    assert!(driver.vm.mcp_library_error.is_none());

    std::fs::remove_file(skill_dir.join("SKILL.md")).unwrap();
    driver.on_reply(
        ReplyScope::Global,
        loaded(AppSettings::default(), 0),
        &mut out,
    );
    assert!(driver.vm.skills.is_empty(), "removed skills must disappear");
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn library_changes_refresh_skills_and_servers_without_navigation() {
    let (driver, root) = scratch_driver("library-live");
    assert_live_library_refreshes(driver, root);
}

#[cfg(unix)]
#[test]
fn library_watches_follow_a_symlinked_home_ancestor_and_new_skill_directories() {
    let (mut driver, root) = scratch_driver("library-live-alias");
    let alias = root.with_extension("alias");
    std::os::unix::fs::symlink(root.parent().unwrap(), &alias).unwrap();
    driver.home =
        crate::config::HomeLayout::from_root(alias.join(root.file_name().unwrap())).unwrap();
    assert_live_library_refreshes(driver, root);
    std::fs::remove_file(alias).unwrap();
}

fn assert_live_library_refreshes(mut driver: FrontendDriver, root: std::path::PathBuf) {
    let mut out = Vec::new();
    driver.watch_library();
    assert!(driver.library_watcher.is_some());
    driver.on_reply(
        ReplyScope::Global,
        loaded(AppSettings::default(), 0),
        &mut out,
    );
    let skill = root.join("skills/sample");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "# Live skill\n").unwrap();
    std::fs::write(
        root.join("mcp.json"),
        r#"{"mcpServers":{"live":{"url":"http://localhost:3425/mcp"}}}"#,
    )
    .unwrap();
    poll_until(&mut driver, &mut out, |driver| {
        driver.vm.skills.iter().any(|skill| skill.slug == "sample")
            && driver
                .vm
                .mcp_library
                .iter()
                .any(|server| server.id == "live")
    });
    // The first refresh attaches a recursive watch to the newly created skills root.
    std::fs::write(skill.join("SKILL.md"), "# Updated skill\n").unwrap();
    poll_until(&mut driver, &mut out, |driver| {
        driver
            .vm
            .skills
            .iter()
            .any(|skill| skill.title == "Updated skill")
    });
    std::fs::remove_dir_all(&skill).unwrap();
    std::fs::remove_file(root.join("mcp.json")).unwrap();
    poll_until(&mut driver, &mut out, |driver| {
        driver.vm.skills.is_empty() && driver.vm.mcp_library.is_empty()
    });
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_library_refresh_invalidates_changed_server_tools_and_old_probes() {
    let (mut driver, root) = scratch_driver("library-probe-invalidation");
    let mut out = Vec::new();
    driver.on_reply(
        ReplyScope::Global,
        loaded(AppSettings::default(), 0),
        &mut out,
    );
    std::fs::write(root.join("mcp.json"), r#"{"mcpServers":{"docs":{"url":"http://localhost:3425/old"},"keep":{"url":"http://localhost:3425/keep"}}}"#).unwrap();
    driver.reload_mcp_library(&mut out);
    let old = driver
        .vm
        .mcp_library
        .iter()
        .find(|server| server.id == "docs")
        .unwrap()
        .clone();
    let scope = ReplyScope::McpProbe(Box::new(old));
    assert!(scope.applies(&driver.vm, None));
    driver.vm.mcp_tools = vec![
        ("docs".into(), vec!["old_tool".into()]),
        ("keep".into(), vec!["kept_tool".into()]),
    ];
    driver.vm.mcp_probing = vec!["docs".into()];
    std::fs::write(root.join("mcp.json"), r#"{"mcpServers":{"docs":{"url":"http://localhost:3425/new"},"keep":{"url":"http://localhost:3425/keep"}}}"#).unwrap();
    driver.reload_mcp_library(&mut out);
    assert!(!scope.applies(&driver.vm, None));
    assert!(driver.vm.mcp_probing.is_empty());
    assert_eq!(
        driver.vm.mcp_tools,
        vec![("keep".to_owned(), vec!["kept_tool".to_owned()])]
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn catalog_first_start_refresh_reloads_binding_without_restart() {
    let gateway = GatewayFixture::start();
    let (mut driver, root) = scratch_driver("catalog-first-start");
    let mut out = Vec::new();
    // Bootstrap read before the plugin wrote the initial binding.
    driver.on_reply(
        ReplyScope::Global,
        loaded(AppSettings::default(), 0),
        &mut out,
    );
    assert!(driver.vm.selected_model.is_none());
    gateway.write_binding(&root, "z/default");
    driver.refresh_catalog();
    poll_until(&mut driver, &mut out, |driver| {
        driver.vm.selected_model.as_deref() == Some("z/default")
    });
    let settings = driver.vm.settings.as_ref().expect("settings");
    assert_eq!(settings.magpie.model, "z/default");
    assert_eq!(settings.providers[0].models.len(), 3);
    assert!(!settings.dirty);
    assert_eq!(driver.vm.selected_provider.as_deref(), Some("magpie"));
    assert_eq!(
        crate::frontend::view_model::model_context_window(&driver.vm),
        256_000
    );
    assert!(out.iter().any(|effect| matches!(
        effect,
        Effect::Toast { kind: ToastKind::Info, text }
            if text == "Provider catalog refreshed" || text == "服务商目录已刷新"
    )));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn catalog_background_update_reaches_an_empty_composer() {
    let gateway = GatewayFixture::start();
    let (mut driver, root) = scratch_driver("catalog-background");
    let mut out = Vec::new();
    driver.on_reply(
        ReplyScope::Global,
        loaded(AppSettings::default(), 0),
        &mut out,
    );
    gateway.write_binding(&root, "z/default");
    // Populate the core's cache without delivering the refresh reply to the UI.
    let reply = super::super::block_on(
        driver
            .bridge
            .request(crate::app::BridgeCommand::RefreshCatalog),
    );
    assert!(matches!(reply, BridgeReply::Catalog(Ok(_))));
    poll_until(&mut driver, &mut out, |driver| {
        driver.vm.selected_model.as_deref() == Some("z/default")
    });
    assert!(driver.vm.catalog.is_some());
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn catalog_external_model_change_preserves_drafts_and_session_pins() {
    let gateway = GatewayFixture::start();
    let (mut driver, root) = scratch_driver("catalog-external-change");
    gateway.write_binding(&root, "z/default");
    let mut out = Vec::new();
    driver.refresh_catalog();
    poll_until(&mut driver, &mut out, |driver| {
        driver.vm.selected_model.as_deref() == Some("z/default")
    });
    {
        let settings = driver.vm.settings.as_mut().expect("settings");
        settings.proxy = "http://127.0.0.1:7890".to_owned();
        settings.theme = "light".to_owned();
        settings.reasoning = Some("high".to_owned());
        settings.dirty = true;
        settings.edit_epoch = 7;
    }
    gateway.write_binding(&root, "a/other");
    driver.refresh_catalog();
    poll_until(&mut driver, &mut out, |driver| {
        driver.vm.settings.as_ref().expect("settings").magpie.model == "a/other"
    });
    assert_eq!(driver.vm.selected_model.as_deref(), Some("a/other"));
    let settings = driver.vm.settings.as_ref().expect("settings");
    assert_eq!(settings.theme, "light");
    assert_eq!(settings.proxy, "http://127.0.0.1:7890");
    assert_eq!(settings.reasoning.as_deref(), Some("high"));
    assert!(settings.dirty);
    assert_eq!(settings.edit_epoch, 7);

    driver.vm.active = Some(conversation("pinned-session", "branch"));
    driver.vm.selected_model = Some("p/pinned".to_owned());
    crate::config::upsert_session_model(
        &mut driver.vm.session_models,
        "pinned-session",
        "magpie",
        "p/pinned",
        Some("high".to_owned()),
    );
    let pins = driver.vm.session_models.clone();
    driver.vm.settings.as_mut().expect("settings").saving = true;
    gateway.write_binding(&root, "z/default");
    driver.refresh_catalog();
    poll_until(&mut driver, &mut out, |driver| {
        driver.vm.settings.as_ref().expect("settings").magpie.model == "z/default"
    });
    assert_eq!(driver.vm.selected_model.as_deref(), Some("p/pinned"));
    assert_eq!(driver.vm.session_models, pins);
    let settings = driver.vm.settings.as_ref().expect("settings");
    assert_eq!(settings.theme, "light");
    assert!(settings.dirty && settings.saving);
    assert_eq!(settings.edit_epoch, 7);
    assert_eq!(settings.reasoning.as_deref(), Some("high"));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn catalog_before_settings_rejects_a_late_bootstrap_snapshot() {
    use crate::providers::catalog::{CatalogDocument, CatalogModel, CatalogProvider};

    let (mut driver, root) = scratch_driver("catalog-before-settings");
    let mut out = Vec::new();
    let catalog = Arc::new(CatalogDocument {
        providers: vec![CatalogProvider {
            id: "magpie".to_owned(),
            models: ["a/other", "z/default"]
                .into_iter()
                .map(|id| CatalogModel {
                    id: id.to_owned(),
                    ..CatalogModel::default()
                })
                .collect(),
            ..CatalogProvider::default()
        }],
    });
    driver.on_reply(
        ReplyScope::CatalogRefresh,
        BridgeReply::Catalog(Ok(crate::app::CatalogInfo {
            document: catalog,
            fetched_at: 1,
        })),
        &mut out,
    );
    assert!(out.is_empty(), "success waits for the settings projection");
    let current_generation = driver.settings_load_generation;
    assert!(current_generation > 0);
    let document = AppSettings {
        magpie: crate::config::MagpieSettings {
            enabled: true,
            base_url: "http://127.0.0.1:3425/v1".to_owned(),
            api_key: "fixture-key".to_owned(),
            model: "z/default".to_owned(),
            hidden_models: Vec::new(),
        },
        ..AppSettings::default()
    };
    driver.on_reply(
        ReplyScope::Settings {
            generation: current_generation,
        },
        loaded(document, 1),
        &mut out,
    );
    driver.on_reply(
        ReplyScope::Settings {
            generation: current_generation - 1,
        },
        loaded(AppSettings::default(), 0),
        &mut out,
    );
    assert_eq!(driver.vm.selected_model.as_deref(), Some("z/default"));
    assert!(out.iter().any(|effect| matches!(
        effect,
        Effect::Toast {
            kind: ToastKind::Info,
            ..
        }
    )));
    assert_eq!(
        driver.vm.settings.as_ref().expect("settings").providers[0]
            .models
            .len(),
        2
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn catalog_lazy_read_cannot_announce_a_manual_refresh_success() {
    let (mut driver, root) = scratch_driver("catalog-refresh-toast");
    let mut out = Vec::new();
    driver.refresh_catalog();
    driver.on_reply(
        ReplyScope::Global,
        BridgeReply::Catalog(Ok(crate::app::CatalogInfo {
            document: Arc::new(crate::providers::catalog::CatalogDocument::default()),
            fetched_at: 0,
        })),
        &mut out,
    );
    assert!(out.is_empty(), "the lazy read must not toast success");
    driver.on_reply(
        ReplyScope::CatalogRefresh,
        BridgeReply::Catalog(Err("fixture offline".to_owned())),
        &mut out,
    );
    assert!(out.iter().any(|effect| matches!(
        effect,
        Effect::Toast { kind: ToastKind::Error, text } if text == "fixture offline"
    )));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn catalog_reload_keeps_a_session_pin_when_the_binding_is_disabled() {
    let mut state = crate::frontend::view_model::WorkspaceState {
        active: Some(conversation("session", "branch")),
        selected_provider: Some("magpie".to_owned()),
        selected_model: Some("p/pinned".to_owned()),
        ..crate::frontend::view_model::WorkspaceState::default()
    };
    crate::config::upsert_session_model(
        &mut state.session_models,
        "session",
        "magpie",
        "p/pinned",
        None,
    );
    let pins = state.session_models.clone();
    reduce(
        &mut state,
        FrontendAction::SettingsLoaded(Box::new(SettingsState::from_settings(
            &AppSettings::default(),
            1,
        ))),
    );
    assert_eq!(state.selected_model.as_deref(), Some("p/pinned"));
    assert_eq!(state.session_models, pins);
}
