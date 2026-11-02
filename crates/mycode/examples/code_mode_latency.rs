//! Compare blocking and concurrent CodeMode calls with identical simulated I/O.
//! Run with: cargo run -p mycode --example code_mode_latency --locked

use std::sync::Arc;
use std::time::{Duration, Instant};

use mycode::tools::{
    CodeCells, RunCodeTool, Tool, ToolCtx, ToolError, ToolRegistry, ToolResult, ToolStream,
};
use serde_json::{Value, json};

struct DelayedRead;

impl Tool for DelayedRead {
    type Args = Value;
    type Output = ();

    fn name(&self) -> &str {
        "delayed_read"
    }
    fn description(&self) -> &str {
        "Simulated independent I/O taking 50 ms."
    }
    fn parallel_safe(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        _: Value,
        _: &ToolCtx,
        _: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(ToolResult::text("ready"))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(DelayedRead));
    let cells = Arc::new(CodeCells::new());
    let run = RunCodeTool::new(Arc::clone(&registry), cells, "latency".into());
    let ctx = ToolCtx::new(std::env::current_dir().unwrap());
    let scripts = [
        (
            "serial",
            "for (let i = 0; i < 8; i++) call('delayed_read'); complete(8);",
        ),
        (
            "parallel",
            "const r = await Promise.all(Array.from({length: 8}, () => callAsync('delayed_read'))); complete(r.length);",
        ),
    ];
    let mut medians = Vec::new();
    for (name, script) in scripts {
        let mut samples = Vec::new();
        // One warmup followed by five samples of the same eight tool calls.
        for iteration in 0..6 {
            let started = Instant::now();
            let result = run
                .execute(
                    serde_json::from_value(json!({"script": script})).unwrap(),
                    &ctx,
                    &mut ToolStream::closed(),
                )
                .await
                .unwrap();
            let elapsed = started.elapsed().as_secs_f64() * 1_000.;
            assert!(!result.is_error, "{result:?}");
            assert_eq!(result.details.as_ref().unwrap()["calls"], 8);
            if iteration > 0 {
                samples.push(elapsed);
            }
        }
        samples.sort_by(f64::total_cmp);
        let median = samples[samples.len() / 2];
        println!("{name}: median {median:.2} ms; samples {samples:.2?}");
        medians.push(median);
    }
    println!(
        "serial / parallel: {:.2}x (8 independent 50 ms waits; excludes model and network latency)",
        medians[0] / medians[1]
    );
}
