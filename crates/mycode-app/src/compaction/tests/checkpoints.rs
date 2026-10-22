use super::{CompactStatus, padded, scratch_home, summary_wire_with, user, user_text};

/// A checkpoint whose covered count runs past this ledger was cut from
/// other history. Clamping the transcript to nothing would retell the stale
/// checkpoint instead of the messages it replaced.
#[tokio::test]
async fn a_checkpoint_that_does_not_fit_the_ledger_is_ignored() {
    let (root, home) = scratch_home();
    mycode_agent::write_compaction(
        &home,
        "session-a",
        &mycode_agent::CompactionCheckpoint {
            format_version: mycode_agent::COMPACTION_FORMAT_VERSION,
            kind: mycode_agent::COMPACTION_KIND.to_owned(),
            session_id: "session-a".to_owned(),
            branch_id: "branch-a".to_owned(),
            covered_head: "head-0".to_owned(),
            covered_messages: 40,
            summary: "STALE-SUMMARY of a longer history".to_owned(),
            model: "model-a".to_owned(),
            created_at_unix: 1,
        },
    )
    .expect("seed checkpoint");
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let bodies = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let wire = summary_wire_with(calls.clone(), Some(bodies.clone()));
    let history = vec![
        user(&padded("OLD-ONE ", 39_000)),
        user(&padded("OLD-TWO ", 39_000)),
        user(&padded("TAIL ", 39_000)),
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
        context_window: 30_000,
        overhead_tokens: 0,
        measure: super::super::Measure::default(),
        on_summarize: None,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    let compacted =
        super::super::compact_history(&scope, history, super::super::CompactMode::Auto).await;
    assert_eq!(compacted.status, CompactStatus::Wrote);
    let sent = bodies.lock().concat();
    assert!(
        sent.contains("OLD-ONE") && sent.contains("OLD-TWO"),
        "{sent}"
    );
    assert!(!sent.contains("STALE-SUMMARY"));
    assert!(!user_text(&compacted.messages[0]).contains("STALE-SUMMARY"));
    let _ = std::fs::remove_dir_all(root);
}
