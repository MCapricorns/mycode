use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::evaluate;
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
