use super::input::choice_numbers;
use super::render::render_entry;
use super::settings_rows::{SettingRow, SharedServers, settings_items, settings_rows};
use super::theme::Palette;
use super::wheel_cursor;
use mycode_app::{ConversationEntry, EntryKind};
use mycode_frontend::view_model::WorkspaceState;

/// A quoted sbt `[error]` line is answer text; only the agent's
/// interruption line becomes a `✖` error row.
#[test]
fn only_the_interruption_line_renders_as_an_error_row() {
    let entry = ConversationEntry {
        event_id: "evt-1".into(),
        kind: EntryKind::AssistantMessage,
        text: "```\n[error] Foo.scala:3: type mismatch\n```\n\
               [error] the response was interrupted: transport failed"
            .into(),
        call_id: None,
        thinking: String::new(),
    };
    let mut lines = Vec::new();
    render_entry(&entry, &mut lines, false, Palette::DARK);
    let rows: Vec<String> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    let errors: Vec<&String> = rows.iter().filter(|row| row.starts_with('✖')).collect();
    assert_eq!(errors.len(), 1, "{rows:#?}");
    assert!(errors[0].contains("the response was interrupted: transport failed"));
}

#[test]
fn choice_numbers_accept_lists_and_reject_free_text() {
    assert_eq!(choice_numbers("2", 3), Some(vec![2]));
    assert_eq!(choice_numbers(" 1, 3 3", 3), Some(vec![1, 3]));
    assert_eq!(choice_numbers("4", 3), None);
    assert_eq!(choice_numbers("0", 3), None);
    assert_eq!(choice_numbers("1 and 2", 3), None);
    assert_eq!(choice_numbers("", 3), None);
}

#[test]
fn settings_list_the_shortcuts_without_selecting_them() {
    let home = scratch_home("shortcuts");
    let vm = WorkspaceState::default();
    let keys: Vec<String> = page_rows(&vm, &home)
        .into_iter()
        .filter_map(|row| match row {
            SettingRow::Info { label, .. } => Some(label),
            _ => None,
        })
        .collect();
    assert!(keys.iter().any(|keys| keys == "Ctrl+Enter"));
    assert!(keys.iter().any(|keys| keys == "Ctrl+G"));
    let selectable = page_items(&vm, &home).len();
    let items = page_rows(&vm, &home)
        .iter()
        .filter(|row| matches!(row, SettingRow::Item { .. }))
        .count();
    assert_eq!(selectable, items);
}

/// One scratch home, so magpie's `mcp.json` can be written beside it.
fn scratch_home(label: &str) -> mycode_config::HomeLayout {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let root = std::env::temp_dir().join(format!(
        "mycode-cli-settings-{label}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    mycode_config::HomeLayout::from_root(root).unwrap()
}

/// The settings rows for one scratch home, with a cold shared-server cache.
fn page_rows(vm: &WorkspaceState, home: &mycode_config::HomeLayout) -> Vec<SettingRow> {
    settings_rows(vm, home, &mut SharedServers::default())
}

/// The selectable items for one scratch home.
fn page_items(
    vm: &WorkspaceState,
    home: &mycode_config::HomeLayout,
) -> Vec<super::settings::SettingItem> {
    settings_items(vm, home, &mut SharedServers::default())
}

/// magpie's shared servers are listed read-only, so the MCP section never
/// says "none configured" while connected tools exist.
#[test]
fn settings_list_the_servers_magpie_shares() {
    let home = scratch_home("mcp");
    std::fs::write(
        home.root().join("mcp.json"),
        r#"{"mcpServers":{"anysearch":{"type":"http","url":"https://api.anysearch.com/mcp"}}}"#,
    )
    .unwrap();
    let vm = WorkspaceState::default();
    let rows = page_rows(&vm, &home);
    assert!(
        rows.iter().any(|row| matches!(
            row,
            SettingRow::Info { label, value }
                if label == "anysearch" && value.contains("https://api.anysearch.com/mcp")
        )),
        "magpie's server is missing from the MCP rows"
    );
    assert!(!rows.iter().any(|row| matches!(
        row,
        SettingRow::Header(title) if title.contains("none configured")
    )));
    // Read-only: magpie's rows are not selectable settings items.
    assert!(
        page_items(&vm, &home)
            .iter()
            .all(|item| !matches!(item, super::settings::SettingItem::Mcp(_)))
    );
    let _ = std::fs::remove_dir_all(home.root());
}

/// A user server with the same id wins over magpie's row, exactly as the
/// connect merge decides, and only user servers get toggle rows.
#[test]
fn a_configured_server_shadows_the_shared_one() {
    let home = scratch_home("shadow");
    std::fs::write(
        home.root().join("mcp.json"),
        r#"{"mcpServers":{"anysearch":{"type":"http","url":"https://shared.example/mcp"},"context7":{"type":"http","url":"https://mcp.context7.com/mcp"}}}"#,
    )
    .unwrap();
    let mut settings = mycode_config::AppSettings::default();
    settings.mcp_servers.push(mycode_config::McpServerSettings {
        id: "anysearch".to_owned(),
        enabled: false,
        transport: "http".to_owned(),
        command: None,
        args: Vec::new(),
        env: std::collections::BTreeMap::new(),
        endpoint: Some("https://mine.example/mcp".to_owned()),
        key_header: None,
        headers: std::collections::BTreeMap::new(),
    });
    let vm = WorkspaceState {
        settings: Some(mycode_frontend::view_model::SettingsState::from_settings(
            &settings, 0,
        )),
        ..WorkspaceState::default()
    };
    let rows = page_rows(&vm, &home);
    let labels: Vec<&str> = rows
        .iter()
        .filter_map(|row| match row {
            SettingRow::Item { label, .. } | SettingRow::Info { label, .. } => Some(label.as_str()),
            SettingRow::Header(_) => None,
        })
        .collect();
    assert!(labels.contains(&"context7"), "{labels:?}");
    // The shared anysearch row is gone; only the user's own (still
    // selectable, marked off) row remains.
    assert!(
        !rows.iter().any(|row| matches!(
            row,
            SettingRow::Info { label, .. } if label == "anysearch"
        )),
        "the user's own anysearch row is the only one: {labels:?}"
    );
    assert!(
        rows.iter().any(|row| matches!(
            row,
            SettingRow::Item { label, .. } if label == "[ ] anysearch"
        )),
        "{labels:?}"
    );
    // The user row stays selectable and keeps its index.
    let items = page_items(&vm, &home);
    let item_rows = rows
        .iter()
        .filter(|row| matches!(row, SettingRow::Item { .. }))
        .count();
    assert_eq!(items.len(), item_rows);
    let mcp_items: Vec<&super::settings::SettingItem> = items
        .iter()
        .filter(|item| matches!(item, super::settings::SettingItem::Mcp(_)))
        .collect();
    assert_eq!(mcp_items.len(), 1, "only the user's server is selectable");
    assert!(matches!(mcp_items[0], super::settings::SettingItem::Mcp(0)));
    let _ = std::fs::remove_dir_all(home.root());
}

/// A broken shared config is reported, not hidden behind a second
/// "none configured" claim.
#[test]
fn a_broken_shared_config_is_reported_without_none_configured() {
    let home = scratch_home("broken");
    std::fs::write(home.root().join("mcp.json"), "{").unwrap();
    let vm = WorkspaceState::default();
    let rows = page_rows(&vm, &home);
    assert!(rows.iter().any(|row| matches!(
        row,
        SettingRow::Info { label, value } if label == "magpie" && value.starts_with('✗')
    )));
    assert!(!rows.iter().any(|row| matches!(
        row,
        SettingRow::Header(title) if title.contains("none configured")
    )));
    let _ = std::fs::remove_dir_all(home.root());
}

/// A shared server magpie switched off is listed, but marked so the row does
/// not promise tools that never connect.
#[test]
fn a_disabled_shared_server_is_marked_off() {
    let home = scratch_home("off");
    std::fs::write(
        home.root().join("mcp.json"),
        r#"{"mcpServers":{"anysearch":{"type":"http","url":"https://api.anysearch.com/mcp","disabled":true}}}"#,
    )
    .unwrap();
    let vm = WorkspaceState::default();
    let rows = page_rows(&vm, &home);
    assert!(
        rows.iter().any(|row| matches!(
            row,
            SettingRow::Info { label, value }
                if label == "anysearch" && value.ends_with("· off")
        )),
        "the disabled shared server is not marked off"
    );
    let _ = std::fs::remove_dir_all(home.root());
}

/// Wheel notches step three rows and never leave the cursor off the list,
/// even after the list shrank under it.
#[test]
fn wheel_cursor_steps_three_rows_and_stays_on_the_list() {
    assert_eq!(wheel_cursor(0, false, 0), 0);
    assert_eq!(wheel_cursor(0, true, 0), 0);
    assert_eq!(wheel_cursor(0, false, 10), 3);
    assert_eq!(wheel_cursor(9, true, 10), 6);
    assert_eq!(wheel_cursor(1, true, 10), 0);
    // One notch past the end lands on the last row, not beyond it.
    assert_eq!(wheel_cursor(8, false, 10), 9);
    assert_eq!(wheel_cursor(9, false, 10), 9);
    // The list shrank since the cursor was set: clamp, then step.
    assert_eq!(wheel_cursor(20, true, 5), 1);
    assert_eq!(wheel_cursor(20, false, 5), 4);
}
