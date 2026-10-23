//! The hardened Rhai engine and its host-function whitelist.
//!
//! Every limit in this module is fixed; a model-written script cannot raise
//! one. The whitelist is exactly `call`, `log`, `budget`, `complete` and
//! `yield_control`: `agent` and the MCP meta tools are ordinary registry
//! tools reached through `call`, so code mode never opens a second
//! orchestration or filesystem channel. See
//! `wiki/architecture/code-mode-engine.md` §3.2.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rhai::{Dynamic, Engine, EvalAltResult, Position};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use super::cell::ScriptCell;
use super::dispatch::{HostReply, HostRequest};

/// Operation budget for one script.
pub const MAX_OPERATIONS: u64 = 1_000_000;
/// Call-stack ceiling.
pub const MAX_CALL_LEVELS: usize = 64;
/// Expression depth ceiling: (expressions, function bodies).
pub const MAX_EXPR_DEPTHS: (usize, usize) = (64, 32);
/// Largest script string, in bytes.
pub const MAX_STRING_SIZE: usize = 4 * 1024 * 1024;
/// Largest array the script may build.
pub const MAX_ARRAY_SIZE: usize = 65_536;
/// Largest map the script may build.
pub const MAX_MAP_SIZE: usize = 65_536;
/// Rhai invokes `on_progress` once per operation; the callback samples every
/// this many operations, so a 1M-operation script does not take the cell
/// lock a million times.
pub const PROGRESS_CHECK_EVERY: u64 = 4096;

/// Marker `complete(value)` terminates evaluation with, so stopping the
/// script is a control-flow fact rather than a race on the host channel.
pub const COMPLETE_MARKER: &str = "__mycode_complete__";

/// Knobs a caller cannot change from inside the script.
pub struct EngineLimits {
    /// Turn cancellation; a long evaluation stops at the next operation.
    pub cancel: CancellationToken,
}

/// Builds one engine for one script.
///
/// The engine is `Send` (the `sync` feature is a hard requirement) so it can
/// be moved onto the script's `std::thread`.
#[must_use]
pub fn hardened_engine(
    host_tx: UnboundedSender<HostRequest>,
    cell: Arc<ScriptCell>,
    limits: EngineLimits,
) -> Engine {
    let mut engine = Engine::new();
    engine
        .set_max_operations(MAX_OPERATIONS)
        .set_max_call_levels(MAX_CALL_LEVELS)
        .set_max_expr_depths(MAX_EXPR_DEPTHS.0, MAX_EXPR_DEPTHS.1)
        .set_max_string_size(MAX_STRING_SIZE)
        .set_max_array_size(MAX_ARRAY_SIZE)
        .set_max_map_size(MAX_MAP_SIZE);
    // No `eval` and no module resolver: a script cannot build code at run
    // time or import one. `print`/`debug` are disabled at the parser so a
    // silent side channel cannot desynchronise the journal; anything
    // observable must go through `log`.
    engine.disable_symbol("eval");
    engine.disable_symbol("print");
    engine.disable_symbol("debug");
    engine.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());
    // Belt and braces: should any remaining path reach a print, keep it off
    // the host's stdout.
    engine.on_print(|_| {});
    engine.on_debug(|_, _, _| {});

    let cancel = limits.cancel;
    let probe = Arc::clone(&cell);
    let ticks = AtomicU64::new(0);
    engine.on_progress(move |operations: u64| {
        let tick = ticks.fetch_add(1, Ordering::Relaxed) + 1;
        if !tick.is_multiple_of(PROGRESS_CHECK_EVERY) {
            return None;
        }
        probe.set_operations(operations);
        if probe.terminated() || cancel.is_cancelled() {
            Some(Dynamic::from("interrupted"))
        } else {
            None
        }
    });

    register_host_functions(&mut engine, host_tx, cell);
    engine
}

fn register_host_functions(
    engine: &mut Engine,
    host_tx: UnboundedSender<HostRequest>,
    cell: Arc<ScriptCell>,
) {
    // `call(name, args)`. Rhai reserves the name `call` for its own FnPtr
    // invocation (`Op::CallFnPtr`), lowered by the compiler before any
    // function lookup, so a registered `call` would never be reached. Custom
    // syntax is matched before that lowering, which is what keeps the
    // model-facing contract exactly `call(name, args)`.
    let call_tx = host_tx.clone();
    let call_cell = Arc::clone(&cell);
    engine
        .register_custom_syntax(
            ["call", "(", "$expr$", ",", "$expr$", ")"],
            false,
            move |context, inputs| -> Result<Dynamic, Box<EvalAltResult>> {
                let name = match context.eval_expression_tree(&inputs[0])?.into_string() {
                    Ok(name) => name,
                    Err(kind) => {
                        return Err(runtime_error(format!(
                            "call() takes a tool name string, got {kind}"
                        )));
                    }
                };
                let args = context.eval_expression_tree(&inputs[1])?;
                let seq = call_cell.next_seq();
                if name == "run_code" || name == "wait_code" {
                    return Err(runtime_error(format!(
                        "call(\"{name}\") is not allowed: code mode cannot invoke itself"
                    )));
                }
                if !call_cell.begin_call() {
                    return Err(runtime_error(format!(
                        "host call budget exhausted ({} calls); split the work into several \
                         scripts",
                        super::cell::MAX_HOST_CALLS
                    )));
                }
                let arguments = match dynamic_to_json(&args) {
                    Ok(Value::Null) => Value::Object(serde_json::Map::new()),
                    Ok(value) => value,
                    Err(error) => return Err(error),
                };
                let (reply_tx, reply_rx) = std::sync::mpsc::channel();
                if call_tx
                    .send(HostRequest::Call {
                        seq,
                        name,
                        args: arguments,
                        reply: reply_tx,
                    })
                    .is_err()
                {
                    return Err(runtime_error("the host dispatcher stopped"));
                }
                match reply_rx.recv() {
                    Ok(HostReply::Value(value)) => json_to_dynamic(&value),
                    Ok(HostReply::Abort(reason)) => Err(runtime_error(reason)),
                    Err(_) => Err(runtime_error("the host dispatcher dropped the call")),
                }
            },
        )
        .expect("the `call` custom syntax is a valid pattern");

    // log(text) → record it as output and as progress.
    let log_tx = host_tx.clone();
    let log_cell = Arc::clone(&cell);
    engine.register_fn("log", move |value: Dynamic| {
        let text = render_value(&value);
        log_cell.push_output(&text);
        let seq = log_cell.next_seq();
        let _ = log_tx.send(HostRequest::Log { seq, text });
    });

    // budget() → counters the script may branch on.
    let budget_cell = Arc::clone(&cell);
    engine.register_fn("budget", move || -> rhai::Map {
        let mut map = rhai::Map::new();
        map.insert("calls_used".into(), Dynamic::from(budget_cell.calls() as i64));
        map.insert(
            "max_calls".into(),
            Dynamic::from(super::cell::MAX_HOST_CALLS as i64),
        );
        map.insert(
            "operations".into(),
            Dynamic::from(budget_cell.operations() as i64),
        );
        map.insert("max_operations".into(), Dynamic::from(MAX_OPERATIONS as i64));
        map.insert(
            "max_output_bytes".into(),
            Dynamic::from(budget_cell.max_output_bytes() as i64),
        );
        map
    });

    // complete(value) → the final answer; evaluation stops right after.
    let complete_tx = host_tx.clone();
    let complete_cell = Arc::clone(&cell);
    engine.register_fn(
        "complete",
        move |value: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let text = render_value(&value);
            let json = dynamic_to_json(&value).unwrap_or_else(|_| Value::String(text.clone()));
            complete_cell.push_output(&text);
            let seq = complete_cell.next_seq();
            let _ = complete_tx.send(HostRequest::Complete { seq, value: json });
            Err(Box::new(EvalAltResult::ErrorTerminated(
                Dynamic::from(COMPLETE_MARKER),
                Position::NONE,
            )))
        },
    );

    // yield_control() → hand control back to the model without stopping.
    let yield_cell = Arc::clone(&cell);
    engine.register_fn("yield_control", move || {
        let seq = yield_cell.next_seq();
        yield_cell.record(
            seq,
            super::journal::ScriptEventKind::Yield,
            &Value::Null,
            None,
        );
        yield_cell.mark_yielded();
    });

    // Anything that would leave the journal's view of the run incomplete.
    // `sleep` is registered per numeric type: Rhai prefers an exact
    // signature over a `Dynamic` one, and the core package already has
    // `sleep(INT)` / `sleep(FLOAT)`.
    engine.register_fn("sleep", |_: i64| -> Result<(), Box<EvalAltResult>> {
        Err(runtime_error("sleep is not available in code mode"))
    });
    engine.register_fn("sleep", |_: f64| -> Result<(), Box<EvalAltResult>> {
        Err(runtime_error("sleep is not available in code mode"))
    });
    engine.register_fn("exit", || -> Result<(), Box<EvalAltResult>> {
        Err(runtime_error("exit is not available in code mode; call complete(value)"))
    });
    engine.register_fn("exit", |_: Dynamic| -> Result<(), Box<EvalAltResult>> {
        Err(runtime_error("exit is not available in code mode; call complete(value)"))
    });
    engine.register_fn("timestamp", || -> Result<(), Box<EvalAltResult>> {
        Err(runtime_error("timestamp is not available in code mode"))
    });
}

fn runtime_error(message: impl Into<String>) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(
        Dynamic::from(message.into()),
        Position::NONE,
    ))
}

/// Converts a script value to JSON.
pub(crate) fn dynamic_to_json(value: &Dynamic) -> Result<Value, Box<EvalAltResult>> {
    if value.is_unit() {
        return Ok(Value::Null);
    }
    serde_json::to_value(value).map_err(|error| {
        runtime_error(format!("cannot convert a script value to JSON: {error}"))
    })
}

/// Converts a JSON value back to a script value.
pub(crate) fn json_to_dynamic(value: &Value) -> Result<Dynamic, Box<EvalAltResult>> {
    rhai::serde::to_dynamic(value)
        .map_err(|error| runtime_error(format!("cannot convert a tool result: {error}")))
}

/// The text form used for `log` and `complete`.
pub(crate) fn render_value(value: &Dynamic) -> String {
    if let Some(text) = value.clone().try_cast::<String>() {
        return text;
    }
    dynamic_to_json(value)
        .map(|json| json.to_string())
        .unwrap_or_else(|_| value.to_string())
}
