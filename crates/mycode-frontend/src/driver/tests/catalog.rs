//! Gateway lifecycle regressions use only a scratch home and synthetic binding.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mycode_app::{AppSettings, AuthorityRevision, BridgeReply, SettingsSnapshot};

use super::{conversation, scratch_driver};
use crate::driver::{Effect, FrontendDriver, ReplyScope, ToastKind};
use crate::view_model::{FrontendAction, SettingsState, reduce};

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
            while !stopped.load(Ordering::Relaxed) {
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
                    let count = stream.read(&mut chunk).expect("request");
                    assert!(count > 0 && request.len() < 16 * 1024, "HTTP headers");
                    request.extend_from_slice(&chunk[..count]);
                }
                assert!(request.starts_with(b"GET /v1/models HTTP/1.1\r\n"));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("response");
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
    assert_eq!(crate::view_model::model_context_window(&driver.vm), 256_000);
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
            .request(mycode_app::BridgeCommand::RefreshCatalog),
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
    mycode_app::upsert_session_model(
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
    use mycode_app::catalog::{CatalogDocument, CatalogModel, CatalogProvider};

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
        BridgeReply::Catalog(Ok(mycode_app::CatalogInfo {
            document: catalog,
            fetched_at: 1,
        })),
        &mut out,
    );
    assert!(out.is_empty(), "success waits for the settings projection");
    let current_generation = driver.settings_load_generation;
    assert!(current_generation > 0);
    let document = AppSettings {
        magpie: mycode_app::MagpieSettings {
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
        BridgeReply::Catalog(Ok(mycode_app::CatalogInfo {
            document: Arc::new(mycode_app::catalog::CatalogDocument::default()),
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
    let mut state = crate::view_model::WorkspaceState {
        active: Some(conversation("session", "branch")),
        selected_provider: Some("magpie".to_owned()),
        selected_model: Some("p/pinned".to_owned()),
        ..crate::view_model::WorkspaceState::default()
    };
    mycode_app::upsert_session_model(
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
