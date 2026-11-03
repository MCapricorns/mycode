use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{MAX_LOG_BYTES, MAX_LOG_ENTRIES, evaluate};
use crate::tools::code_mode::cell::{CodeCells, ScriptOutcome};
use crate::tools::code_mode::dispatch::HostRequest;

struct UndrainedRun {
    outcome: ScriptOutcome,
    logs: Vec<(u64, String)>,
    output: String,
}

/// Leave the receiver undrained until evaluation finishes: the same queue
/// pressure as logs waiting behind an exclusive tool, without a large stress
/// allocation or a timing-dependent worker. A broken admission check retains
/// at most one extra 64 KiB chunk in these tests.
fn evaluate_undrained(script: &str) -> UndrainedRun {
    let cell = CodeCells::new().create("logs", 1024);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let outcome = evaluate(script, sender, Arc::clone(&cell), CancellationToken::new());
    let mut logs = Vec::new();
    while let Ok(request) = receiver.try_recv() {
        match request {
            HostRequest::Log { seq, text } => logs.push((seq, text)),
            _ => panic!("these scripts only submit logs"),
        }
    }
    UndrainedRun {
        outcome,
        logs,
        output: cell.snapshot().output,
    }
}

#[test]
fn log_byte_budget_bounds_the_undrained_native_queue() {
    let chunk_bytes = 64 * 1024;
    let admitted = MAX_LOG_BYTES / chunk_bytes;
    let run = evaluate_undrained(&format!(
        "const chunk = 'é'.repeat({}); for (let i = 0; i < {}; i++) log(chunk);",
        chunk_bytes / 2,
        admitted + 1,
    ));
    let ScriptOutcome::Failed(message) = &run.outcome else {
        panic!("log byte budget did not fail: {:?}", run.outcome);
    };
    assert!(message.contains("log budget exhausted (byte limit)"));
    assert_eq!(run.logs.len(), admitted);
    assert_eq!(
        run.logs.iter().map(|(_, text)| text.len()).sum::<usize>(),
        MAX_LOG_BYTES
    );
    for (index, (seq, text)) in run.logs.iter().enumerate() {
        assert_eq!(*seq, index as u64);
        assert_eq!(text.len(), chunk_bytes);
    }
    assert!(run.output.contains("output truncated at 1024 bytes"));
    assert!(run.output.len() < 1100);
}

#[test]
fn empty_logs_cannot_bypass_the_entry_budget() {
    let run = evaluate_undrained(&format!(
        "for (let i = 0; i < {}; i++) log('');",
        MAX_LOG_ENTRIES + 1,
    ));
    let ScriptOutcome::Failed(message) = &run.outcome else {
        panic!("log entry budget did not fail: {:?}", run.outcome);
    };
    assert!(message.contains("log budget exhausted (entry limit)"));
    assert_eq!(run.logs.len(), MAX_LOG_ENTRIES);
    assert!(run.logs.iter().all(|(_, text)| text.is_empty()));
}

#[test]
fn admitted_logs_keep_their_full_text_and_report_budget_usage() {
    let run = evaluate_undrained(
        r#"
log('hello 🌍');
log({answer: 42});
const b = budget();
if (b.logs_used !== 2 || b.log_bytes_used !== 23 ||
    b.max_logs !== 1024 || b.max_log_bytes !== 4194304) {
    throw new Error('incorrect log budget');
}
void 0;
"#,
    );
    assert_eq!(run.outcome, ScriptOutcome::Completed);
    assert_eq!(
        run.logs,
        vec![(0, "hello 🌍".into()), (1, r#"{"answer":42}"#.into())]
    );
    assert_eq!(run.output, "hello 🌍\n{\"answer\":42}");
}

#[test]
fn reentrant_json_logging_cannot_reset_budget_usage() {
    let run = evaluate_undrained(&format!(
        r#"
const nested = {{toJSON() {{ log('inner'); return 'outer'; }} }};
for (let i = 0; i < {}; i++) log(nested);
"#,
        MAX_LOG_ENTRIES / 2 + 1,
    ));
    let ScriptOutcome::Failed(message) = &run.outcome else {
        panic!("reentrant logging bypassed the budget: {:?}", run.outcome);
    };
    assert!(message.contains("log budget exhausted (entry limit)"));
    assert_eq!(run.logs.len(), MAX_LOG_ENTRIES);
}

#[test]
fn yield_requests_cannot_bypass_native_queue_and_journal_budgets() {
    let cell = CodeCells::new().create("yields", 1024);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let script = format!(
        "try {{ for (let i = 0; i < {}; i++) yield_control(); }} catch (_) {{ log('escaped'); }}",
        super::MAX_YIELD_REQUESTS + 1,
    );
    let outcome = evaluate(&script, sender, cell, CancellationToken::new());
    assert!(
        matches!(outcome, ScriptOutcome::Failed(message) if message.contains("yield budget exhausted"))
    );
    let mut count = 0;
    while let Ok(request) = receiver.try_recv() {
        assert!(matches!(request, HostRequest::Yield { seq } if seq == count));
        count += 1;
    }
    assert_eq!(count as usize, super::MAX_YIELD_REQUESTS);
}
