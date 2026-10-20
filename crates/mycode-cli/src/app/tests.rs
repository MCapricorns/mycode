use super::input::choice_numbers;
use super::render::render_entry;
use super::settings_rows::{SettingRow, settings_items, settings_rows};
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
    render_entry(&entry, &mut lines, false);
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
    let vm = WorkspaceState::default();
    let keys: Vec<String> = settings_rows(&vm)
        .into_iter()
        .filter_map(|row| match row {
            SettingRow::Info { label, .. } => Some(label),
            _ => None,
        })
        .collect();
    assert!(keys.iter().any(|keys| keys == "Ctrl+Enter"));
    assert!(keys.iter().any(|keys| keys == "Ctrl+G"));
    let selectable = settings_items(&vm).len();
    let items = settings_rows(&vm)
        .iter()
        .filter(|row| matches!(row, SettingRow::Item { .. }))
        .count();
    assert_eq!(selectable, items);
}
