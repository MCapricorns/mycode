use mycode_core::{ContentBlock, TextBlock, ToolCall};

use super::scripted::{
    assistant_message, large_history, scripted_wire, tool_result_message,
};
use super::{
    CompactStatus, PLAUSIBLE_SUMMARY, TokenScale, padded, scratch_home, summary_wire, user,
    user_text,
};
use crate::compaction::echo::{split_echo, user_echo, with_echo};

#[test]
fn kept_user_messages_round_trip_through_the_stored_summary() {
    let echo = [
        "first ask".to_owned(),
        "a correction\nover two lines".to_owned(),
    ];
    let stored = with_echo("the summary", &echo);
    let (summary, kept) = split_echo(&stored);
    assert_eq!(summary, "the summary");
    assert_eq!(kept, ["first ask", "a correction\nover two lines"]);
    assert_eq!(
        split_echo("a plain summary"),
        ("a plain summary", Vec::new())
    );
    assert_eq!(with_echo("a plain summary", &[]), "a plain summary");
}

#[test]
fn the_newest_user_messages_that_fit_are_kept_oldest_first() {
    let head = vec![
        user("ask one"),
        assistant_message(vec![ContentBlock::Text(TextBlock::new("model words"))]),
        tool_result_message("call-1", "tool output"),
        user(&padded("long paste ", 40_000)),
        user("ask three"),
    ];
    let kept = user_echo(&["from the last checkpoint"], &head, 1_000, TokenScale::ONE);
    // The 10k-token paste fits only as its ends, and nothing older after it.
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert!(kept[0].starts_with("long paste ") && kept[0].contains("characters omitted"));
    assert!(kept[0].chars().count() < 4_100);
    assert_eq!(kept[1], "ask three");
    let all = user_echo(
        &["from the last checkpoint"],
        &head[..3],
        1_000,
        TokenScale::ONE,
    );
    assert_eq!(all, ["from the last checkpoint", "ask one"]);
}

fn read_call(id: &str) -> std::sync::Arc<mycode_core::Message> {
    assistant_message(vec![ContentBlock::ToolCall(ToolCall::new(
        id,
        "read",
        serde_json::json!({"path": "src/parser.c"}),
    ))])
}

/// In a long tool run the kept tail is all tool output, so a summary was
/// the only trace of what the user asked. Their words now follow it as
/// written, are stored with the checkpoint, and carry into the next
/// summary; the card still shows the model's summary alone.
#[tokio::test]
async fn the_users_words_outlive_the_summary_that_replaces_them() {
    let (root, home) = scratch_home();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wire = summary_wire(calls.clone());
    let history = vec![
        user("FIRST-ASK: port the parser to Rust"),
        read_call("call-1"),
        tool_result_message("call-1", &padded("OUTPUT-ONE ", 100_000)),
        user("CORRECTION: keep the C API"),
        read_call("call-2"),
        tool_result_message("call-2", &padded("OUTPUT-TWO ", 100_000)),
        read_call("call-3"),
        tool_result_message("call-3", &padded("OUTPUT-THREE ", 20_000)),
    ];
    let scope = super::super::CompactScope {
        wire: &wire,
        model: "model-a",
        checkpoint: Some(super::super::CheckpointSlot {
            home: &home,
            session_id: "session-a",
            branch_id: "branch-a",
            head: "head-1",
        }),
        // ~51k trigger, 20k tail: the last call and its result stay.
        context_window: 60_000,
        overhead_tokens: 0,
        measure: super::super::Measure::default(),
        on_summarize: None,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    let compacted =
        super::super::compact_history(&scope, history, super::super::CompactMode::Auto).await;
    assert_eq!(compacted.status, CompactStatus::Wrote);
    assert_eq!(compacted.summary.as_deref(), Some(PLAUSIBLE_SUMMARY));
    assert_eq!(compacted.messages.len(), 3);
    let sent = user_text(&compacted.messages[0]);
    let at = |marker: &str| {
        sent.find(marker)
            .unwrap_or_else(|| panic!("{marker} missing from {sent}"))
    };
    assert!(at("handoff notes") < at("FIRST-ASK: port the parser to Rust"));
    assert!(at("FIRST-ASK: port the parser to Rust") < at("CORRECTION: keep the C API"));
    let stored =
        super::super::stored_checkpoint(&home, "session-a", "branch-a").expect("checkpoint");
    assert!(stored.summary.contains("CORRECTION: keep the C API"));

    // The next compaction finds them in the summary it replaces.
    let mut later = compacted.messages;
    later.extend([
        user("NEXT-ASK: now the tests"),
        read_call("call-4"),
        tool_result_message("call-4", &padded("OUTPUT-FOUR ", 100_000)),
        read_call("call-5"),
        tool_result_message("call-5", &padded("OUTPUT-FIVE ", 100_000)),
    ]);
    let in_memory = super::super::CompactScope {
        checkpoint: None,
        ..scope
    };
    let again =
        super::super::compact_history(&in_memory, later, super::super::CompactMode::Auto).await;
    assert_eq!(again.status, CompactStatus::Replaced);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    let sent = user_text(&again.messages[0]);
    let (_, kept) = split_echo(&sent);
    assert_eq!(
        kept,
        [
            "FIRST-ASK: port the parser to Rust",
            "CORRECTION: keep the C API",
            "NEXT-ASK: now the tests"
        ]
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A summary over the cap keeps its end, where the next steps are.
#[tokio::test]
async fn an_overlong_summary_keeps_its_next_steps() {
    let (root, home) = scratch_home();
    let long: &'static str = Box::leak(
        format!(
            "{}NEXT-STEPS: finish the port",
            padded("SUMMARY-START ", 30_000)
        )
        .into_boxed_str(),
    );
    let (wire, _) = scripted_wire(
        "openai-completions",
        "https://example.com/v1",
        "model-a",
        vec![("stop", long)],
    );
    let scope = super::super::CompactScope {
        wire: &wire,
        model: "model-a",
        checkpoint: Some(super::super::CheckpointSlot {
            home: &home,
            session_id: "session-b",
            branch_id: "branch-b",
            head: "head-1",
        }),
        context_window: 200_000,
        overhead_tokens: 0,
        measure: super::super::Measure::default(),
        on_summarize: None,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    let compacted =
        super::super::compact_history(&scope, large_history(), super::super::CompactMode::Manual)
            .await;
    assert_eq!(compacted.status, CompactStatus::Wrote);
    let summary = compacted.summary.expect("summary");
    assert!(summary.starts_with("SUMMARY-START"));
    assert!(summary.ends_with("NEXT-STEPS: finish the port"));
    assert!(summary.contains("characters omitted"));
    assert!(summary.chars().count() < 24_100);
    let _ = std::fs::remove_dir_all(root);
}
