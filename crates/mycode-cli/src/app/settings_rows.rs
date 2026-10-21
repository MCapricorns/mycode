//! The settings page's row model: section headers, selectable items, and
//! informational rows such as the shortcut list, built from the shared
//! `WorkspaceState` on every paint.

use mycode_app::{ProxyTestRoute, brief_error};
use mycode_config::{HomeLayout, McpServerSettings};
use mycode_frontend::shortcuts::{Surface, shortcuts};
use mycode_frontend::view_model::{
    ProxyTestState, UpdateState, WorkspaceState, model_display_name, provider_display_name,
    selected_reasoning_level,
};

use super::settings::SettingItem;

/// A settings line: a dim section header, a selectable item row, or a row
/// that only informs (a shortcut).
pub(super) enum SettingRow {
    Header(String),
    Item {
        item: SettingItem,
        label: String,
        value: String,
        dim: bool,
    },
    Info {
        label: String,
        value: String,
    },
}

/// The selectable items of the settings page, in display order.
pub(super) fn settings_items(vm: &WorkspaceState, home: &HomeLayout) -> Vec<SettingItem> {
    settings_rows(vm, home)
        .into_iter()
        .filter_map(|row| match row {
            SettingRow::Item { item, .. } => Some(item),
            SettingRow::Header(_) | SettingRow::Info { .. } => None,
        })
        .collect()
}

/// The stored manual proxy URL, or `None` while following the system.
pub(super) fn manual_proxy(vm: &WorkspaceState) -> Option<String> {
    let stored = vm.settings.as_ref()?.proxy.clone();
    match mycode_config::ProxyPreference::parse(&stored) {
        Some(mycode_config::ProxyPreference::Url(url)) => Some(url),
        _ => None,
    }
}

/// The "Proxy test" row value: which route the probe took and how it went.
fn proxy_test_value(test: &ProxyTestState) -> String {
    match test {
        ProxyTestState::Idle => "Enter to fetch google.com through the proxy".to_owned(),
        ProxyTestState::Testing => "testing…".to_owned(),
        ProxyTestState::Invalid(message) => format!("✗ {message}"),
        ProxyTestState::Done(report) => {
            let route = match &report.route {
                ProxyTestRoute::Direct => "direct (no system proxy set)".to_owned(),
                ProxyTestRoute::System(uri) => format!("system proxy {uri}"),
                ProxyTestRoute::Manual(url) => format!("proxy {url}"),
            };
            match &report.outcome {
                Ok((status, elapsed)) => format!(
                    "✓ google.com · {route} · HTTP {status} · {} ms",
                    elapsed.as_millis()
                ),
                Err(message) => format!(
                    "✗ google.com unreachable · {route} · {}",
                    brief_error(message)
                ),
            }
        }
    }
}

/// What an MCP server points at: the command line, or the HTTP endpoint.
fn server_target(server: &McpServerSettings) -> String {
    if server.transport == "stdio" {
        let mut line = server.command.clone().unwrap_or_default();
        for arg in &server.args {
            line.push(' ');
            // Quoted like the form takes it, so an argument with spaces does
            // not read as several.
            if arg.contains(char::is_whitespace) {
                line.push('"');
                line.push_str(arg);
                line.push('"');
            } else {
                line.push_str(arg);
            }
        }
        line
    } else {
        server.endpoint.clone().unwrap_or_default()
    }
}

/// The full settings page: headers and rows in display order.
pub(super) fn settings_rows(vm: &WorkspaceState, home: &HomeLayout) -> Vec<SettingRow> {
    let mut rows = Vec::new();
    let settings = vm.settings.as_ref();

    rows.push(SettingRow::Header("Model".to_owned()));
    let model = match (
        vm.selected_provider.as_deref(),
        vm.selected_model.as_deref(),
    ) {
        (Some(provider), Some(model)) => format!(
            "{} / {}",
            provider_display_name(vm, provider),
            model_display_name(vm, provider, model)
        ),
        (_, Some(model)) => model.to_owned(),
        _ => "none — install or start magpie (usemagpie.ai)".to_owned(),
    };
    rows.push(SettingRow::Item {
        item: SettingItem::Model,
        label: "Model".to_owned(),
        value: model,
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::Thinking,
        label: "Thinking".to_owned(),
        value: selected_reasoning_level(vm),
        dim: false,
    });

    rows.push(SettingRow::Header("General".to_owned()));
    rows.push(SettingRow::Item {
        item: SettingItem::Theme,
        label: "Theme".to_owned(),
        value: settings.map(|s| s.theme.clone()).unwrap_or_default(),
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::Language,
        label: "Language".to_owned(),
        value: settings
            .map(|s| s.language.clone())
            .unwrap_or_else(|| "auto".to_owned()),
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::FontSize,
        label: "Font size".to_owned(),
        value: settings
            .map(|s| s.font_size.clone())
            .unwrap_or_else(|| "m".to_owned()),
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::ThinkingTrace,
        label: "Show thinking".to_owned(),
        value: if vm.thinking_collapsed {
            "off".to_owned()
        } else {
            "on".to_owned()
        },
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::Proxy,
        label: "Proxy".to_owned(),
        value: match manual_proxy(vm) {
            Some(url) => format!("manual · {url}  (Enter edit · ←→ follow system)"),
            None => "follow system  (Enter type a proxy · ←→ manual)".to_owned(),
        },
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::ProxyTest,
        label: "Proxy test".to_owned(),
        value: proxy_test_value(&vm.proxy_test),
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::AutoUpdate,
        label: "Auto-update".to_owned(),
        value: if vm.auto_update {
            "on".to_owned()
        } else {
            "off".to_owned()
        },
        dim: false,
    });

    rows.push(SettingRow::Header("MCP".to_owned()));
    let configured = settings.map_or(&[][..], |settings| settings.mcp_servers.as_slice());
    for (index, server) in configured.iter().enumerate() {
        let probing = if vm.mcp_probing.contains(&server.id) {
            " · probing…"
        } else {
            ""
        };
        rows.push(SettingRow::Item {
            item: SettingItem::Mcp(index),
            label: format!("[{}] {}", if server.enabled { "x" } else { " " }, server.id),
            value: format!("{}{}", server.transport, probing),
            dim: !server.enabled,
        });
    }
    // magpie's Library file is read-only here: magpie owns it, and a server
    // that already uses the id wins, exactly as the connect merge decides.
    let (library, library_error) = match mycode_config::global_mcp_servers(home) {
        Ok(servers) => (servers, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let library: Vec<&McpServerSettings> = library
        .iter()
        .filter(|server| {
            !configured
                .iter()
                .any(|configured| configured.id == server.id)
        })
        .collect();
    for server in &library {
        rows.push(SettingRow::Info {
            label: server.id.clone(),
            value: format!(
                "{} · {} · magpie{}",
                server.transport,
                server_target(server),
                // A disabled shared server is listed but never connects.
                if server.enabled { "" } else { " · off" }
            ),
        });
    }
    if let Some(error) = &library_error {
        rows.push(SettingRow::Info {
            label: "magpie".to_owned(),
            value: format!("✗ {error}"),
        });
    }
    if configured.is_empty() && library.is_empty() && library_error.is_none() {
        rows.push(SettingRow::Header("  (none configured)".to_owned()));
    }

    if !vm.skills.is_empty() {
        rows.push(SettingRow::Header("Skills".to_owned()));
        for (index, skill) in vm.skills.iter().enumerate() {
            let disabled = settings.is_some_and(|s| s.skills.disabled.contains(&skill.slug));
            rows.push(SettingRow::Item {
                item: SettingItem::Skill(index),
                label: format!("[{}] /{}", if disabled { " " } else { "x" }, skill.slug),
                value: skill.title.clone(),
                dim: disabled,
            });
        }
    }

    rows.push(SettingRow::Header("Keyboard shortcuts".to_owned()));
    for shortcut in shortcuts(Surface::Terminal) {
        let value = shortcut.sentence();
        rows.push(SettingRow::Info {
            label: shortcut.keys,
            value,
        });
    }

    rows.push(SettingRow::Header(format!(
        "About — mycode-cli {}",
        env!("CARGO_PKG_VERSION")
    )));
    let update = match &vm.update {
        UpdateState::Idle => "press Enter to check".to_owned(),
        UpdateState::Checking => "checking…".to_owned(),
        UpdateState::UpToDate => "up to date".to_owned(),
        UpdateState::Available { version, .. } => {
            format!("v{version} available — Enter to download")
        }
        UpdateState::Downloading { version } => format!("downloading v{version}…"),
        UpdateState::Ready { version } => {
            format!("v{version} ready — installs on exit; Enter to quit and install")
        }
        UpdateState::Failed(message) => format!("failed: {message} — Enter to retry"),
    };
    rows.push(SettingRow::Item {
        item: SettingItem::Update,
        label: "Update".to_owned(),
        value: update,
        dim: false,
    });
    rows
}
