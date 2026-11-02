use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn promises_support_top_level_await_and_report_rejections() {
    let workspace = workspace("async-values");
    let cells = Arc::new(CodeCells::new());
    for (script, expected) in [
        ("await Promise.resolve(7)", "7"),
        (
            "(async () => { await 0; return {ok: true}; })()",
            r#"{"ok":true}"#,
        ),
        (
            "const r = await callAsync('missing'); complete([r.is_error, typeof r.__error__]);",
            r#"[true,"string"]"#,
        ),
        (
            "const r = await Promise.allSettled([Promise.resolve(1), Promise.reject('bad')]); complete(r);",
            r#"[{"status":"fulfilled","value":1},{"status":"rejected","reason":"bad"}]"#,
        ),
        (
            "const p = Promise.reject('caught'); await Promise.resolve(); p.catch(() => {}); void 0;",
            "(no output)",
        ),
    ] {
        let result = run_script(&cells, &workspace.0, json!({"script": script}))
            .await
            .unwrap();
        assert!(!result.is_error, "{script}: {result:?}");
        assert_eq!(text_of(&result), expected, "{script}");
    }
    for (script, expected) in [
        (
            "await Promise.reject(new Error('awaited failure'));",
            "awaited failure",
        ),
        (
            "Promise.reject(new Error('detached failure')); void 0;",
            "detached failure",
        ),
        ("await new Promise(() => {});", "pending with no host calls"),
        ("complete(Promise.resolve(1));", "await the Promise"),
        (
            "function spin() { Promise.resolve().then(spin); } spin();",
            "budget exhausted",
        ),
        (
            "await 0; try { complete('done'); } finally { log('unreachable'); }",
            "done",
        ),
    ] {
        let result = run_script(&cells, &workspace.0, json!({"script": script}))
            .await
            .unwrap();
        assert_eq!(result.is_error, expected != "done", "{script}: {result:?}");
        assert!(text_of(&result).contains(expected), "{script}: {result:?}");
        assert!(!text_of(&result).contains("unreachable"));
    }
}

#[derive(Default)]
struct Calls {
    active: AtomicUsize,
    peak: AtomicUsize,
    events: Mutex<Vec<String>>,
}

struct ActiveCall<'a> {
    calls: &'a Calls,
    id: String,
}

impl Drop for ActiveCall<'_> {
    fn drop(&mut self) {
        self.calls
            .events
            .lock()
            .unwrap()
            .push(format!("end:{}", self.id));
        self.calls.active.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Probe {
    calls: Arc<Calls>,
    gate: Option<Arc<tokio::sync::Barrier>>,
}

impl ToolDyn for Probe {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "probe".into(),
            description: "Observe overlapping calls and ordered barriers.".into(),
            params_schema: json!({"type": "object"}),
        }
    }

    fn parallel_safe_call(&self, args: &Value) -> bool {
        args["parallel"].as_bool().unwrap_or(true)
    }

    fn execute_dyn<'a>(
        &'a self,
        args: Value,
        _ctx: &'a ToolCtx,
        _out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let id = args["id"].as_str().unwrap().to_owned();
            let active = self.calls.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.calls.peak.fetch_max(active, Ordering::SeqCst);
            self.calls
                .events
                .lock()
                .unwrap()
                .push(format!("start:{id}"));
            let _active = ActiveCall {
                calls: &self.calls,
                id: id.clone(),
            };
            if let Some(gate) = &self.gate {
                gate.wait().await;
            }
            if args["hold"] == true {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(Duration::from_millis(args["delay"].as_u64().unwrap_or(1))).await;
            Ok(ToolResult::text(id))
        })
    }
}

async fn probe_script(
    script: &str,
    calls: Arc<Calls>,
    gate: Option<Arc<tokio::sync::Barrier>>,
) -> ToolResult {
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(Probe { calls, gate }));
    let run = RunCodeTool::new(
        Arc::clone(&registry),
        Arc::new(CodeCells::new()),
        "s".into(),
    );
    tokio::time::timeout(
        Duration::from_secs(3),
        run.execute_dyn(
            json!({"script": script}),
            &ToolCtx::new(std::env::temp_dir()),
            &mut ToolStream::closed(),
        ),
    )
    .await
    .expect("script must settle")
    .unwrap()
}

#[tokio::test]
async fn independent_calls_overlap_and_results_keep_submission_order() {
    let calls = Arc::new(Calls::default());
    let result = probe_script(
        r#"
const results = await Promise.all([
    callAsync('probe', { id: 'first', delay: 20 }),
    callAsync('probe', { id: 'second', delay: 1 }),
    callAsync('probe', { id: 'third', delay: 5 })
]);
complete(results.map(r => r.content[0].Text));
"#,
        Arc::clone(&calls),
        Some(Arc::new(tokio::sync::Barrier::new(3))),
    )
    .await;
    assert!(!result.is_error, "{result:?}");
    assert_eq!(text_of(&result), r#"["first","second","third"]"#);
    assert_eq!(calls.peak.load(Ordering::SeqCst), 3);
    assert_eq!(calls.active.load(Ordering::SeqCst), 0);
    let details = result.details.unwrap();
    for field in ["journal", "inner_calls"] {
        let seqs: Vec<_> = details[field]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["seq"].as_u64().unwrap())
            .collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{field}: {seqs:?}");
    }
    assert_eq!(details["inner_calls"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn exclusive_calls_wait_for_reads_and_block_later_reads() {
    let calls = Arc::new(Calls::default());
    let result = probe_script(
        r#"
await Promise.all([
    callAsync('probe', { id: 'before-a', delay: 10 }),
    callAsync('probe', { id: 'before-b', delay: 20 }),
    callAsync('probe', { id: 'write', parallel: false, delay: 5 }),
    callAsync('probe', { id: 'after-a', delay: 10 }),
    callAsync('probe', { id: 'after-b', delay: 5 })
]);
complete('done');
"#,
        Arc::clone(&calls),
        None,
    )
    .await;
    assert!(!result.is_error, "{result:?}");
    assert_eq!(calls.peak.load(Ordering::SeqCst), 2);
    let events = calls.events.lock().unwrap();
    let at = |event: &str| events.iter().position(|e| e == event).unwrap();
    for id in ["before-a", "before-b"] {
        assert!(at(&format!("end:{id}")) < at("start:write"), "{events:?}");
    }
    for id in ["after-a", "after-b"] {
        assert!(at("end:write") < at(&format!("start:{id}")), "{events:?}");
    }
}

#[tokio::test]
async fn ending_a_script_drops_active_calls_and_never_starts_queued_writes() {
    for (ending, failed) in [
        ("complete('done');", false),
        ("throw new Error('stop');", true),
        (
            "Promise.reject(new Error('detached failure')); void 0;",
            true,
        ),
    ] {
        let calls = Arc::new(Calls::default());
        let script = format!(
            r#"
callAsync('probe', {{ id: 'held-a', hold: true }});
callAsync('probe', {{ id: 'held-b', hold: true }});
await callAsync('probe', {{ id: 'ready' }});
callAsync('probe', {{ id: 'write', parallel: false }});
{ending}
"#
        );
        let result = probe_script(
            &script,
            Arc::clone(&calls),
            Some(Arc::new(tokio::sync::Barrier::new(3))),
        )
        .await;
        assert_eq!(result.is_error, failed, "{}", text_of(&result));
        assert_eq!(calls.active.load(Ordering::SeqCst), 0);
        assert!(
            !calls
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|e| e == "start:write")
        );
        let details = result.details.unwrap();
        let children = details["inner_calls"].as_array().unwrap();
        assert_eq!(children.len(), 3);
        assert_eq!(children[0]["phase"], "cancelled");
        assert_eq!(children[1]["phase"], "cancelled");
        assert_eq!(children[2]["phase"], "completed");
    }
}

#[tokio::test]
async fn cancelling_a_yielded_parallel_script_drops_every_child() {
    for stop_cell in [false, true] {
        let calls = Arc::new(Calls::default());
        let cells = Arc::new(CodeCells::new());
        let registry = Arc::new(ToolRegistry::new());
        registry.register(Arc::new(Probe {
            calls: Arc::clone(&calls),
            gate: None,
        }));
        let run = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&cells), "s".into());
        let ctx = ToolCtx::new(std::env::temp_dir());
        run.execute_dyn(
            json!({"script":
            "await Promise.all(['a','b'].map(id => callAsync('probe', {id, hold: true})));",
            "yield_time_ms": 0}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while calls.active.load(Ordering::SeqCst) != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both children started");
        if stop_cell {
            WaitCodeTool::new(Arc::clone(&cells), "s".into())
                .execute_dyn(
                    json!({"cell_id": "cell-0", "terminate": true}),
                    &ctx,
                    &mut ToolStream::closed(),
                )
                .await
                .unwrap();
        } else {
            ctx.cancel.cancel();
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while calls.active.load(Ordering::SeqCst) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("all child futures were dropped");
        assert_eq!(ctx.cancel.is_cancelled(), !stop_cell);
        assert_eq!(
            cells.get("s", "cell-0").unwrap().phase(),
            super::super::CellPhase::Terminated
        );
    }
}

#[tokio::test]
async fn parallel_reads_and_a_dependent_write_use_the_real_file_tools() {
    let workspace = workspace("parallel-files");
    std::fs::write(workspace.0.join("a.txt"), "alpha").unwrap();
    std::fs::write(workspace.0.join("b.txt"), "beta").unwrap();
    let cells = Arc::new(CodeCells::new());
    let result = run_script(
        &cells,
        &workspace.0,
        json!({"script": r#"
const rows = await Promise.all(['a.txt', 'b.txt'].map(path => callAsync('read', {path})));
if (rows.some(row => row.is_error)) throw new Error('read failed');
const content = rows.map(row => row.content[0].Text.split('\n')[0]).join(',');
const written = await callAsync('write', {path: 'joined.txt', content});
if (written.is_error) throw new Error(written.content[0].Text);
complete(call('read', {path: 'joined.txt'}).content[0].Text.split('\n')[0]);
"#}),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert_eq!(text_of(&result), "alpha,beta");
    assert_eq!(
        std::fs::read_to_string(workspace.0.join("joined.txt")).unwrap(),
        "alpha,beta"
    );
}

#[tokio::test]
async fn async_host_call_budget_cannot_be_caught() {
    let workspace = workspace("async-budget");
    let cells = Arc::new(CodeCells::new());
    let result = run_script(
        &cells,
        &workspace.0,
        json!({"script": r#"
await 0;
try {
    for (let i = 0; i < 300; i++) await callAsync('missing');
} catch (_) { complete('escaped'); }
complete('escaped');
"#}),
    )
    .await
    .unwrap();
    assert!(result.is_error, "{result:?}");
    assert!(
        text_of(&result).contains("host call budget exhausted"),
        "{result:?}"
    );
    assert!(!text_of(&result).contains("escaped"));
}

#[tokio::test]
async fn log_budget_exhaustion_cannot_be_caught_or_keep_exclusive_work_alive() {
    let calls = Arc::new(Calls::default());
    let result = probe_script(
        r#"
const pending = callAsync('probe', {id: 'held', parallel: false, hold: true});
try {
    for (let i = 0; i < 1025; i++) log('');
} catch (_) { complete('escaped'); }
finally { log('escaped finally'); }
await pending;
complete('escaped');
"#,
        Arc::clone(&calls),
        None,
    )
    .await;
    assert!(result.is_error, "{result:?}");
    assert!(text_of(&result).contains("log budget exhausted (entry limit)"));
    assert!(!text_of(&result).contains("escaped"));
    assert_eq!(calls.active.load(Ordering::SeqCst), 0);
}
