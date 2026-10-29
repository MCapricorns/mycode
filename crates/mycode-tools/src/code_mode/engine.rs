//! Embedded QuickJS and the code-mode host-function whitelist.
//!
//! The runtime lives entirely on the script thread. Tools, permissions and
//! async work remain in the shared dispatcher; JavaScript calls it synchronously.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use rquickjs::context::intrinsic;
use rquickjs::function::Opt;
use rquickjs::{Context, Ctx, Exception, Function, Runtime, Value as JsValue};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use super::cell::{ScriptCell, ScriptOutcome};
use super::dispatch::{HostReply, HostRequest};

/// QuickJS interrupt polls per script (not a JavaScript instruction count).
/// QuickJS-NG polls after approximately 10,000 loop/function checks, and in
/// selected builtins. These counters intentionally describe engine checkpoints.
pub const MAX_OPERATIONS: u64 = 100;
/// Total JavaScript heap, including strings, containers and compiled code.
pub const MAX_MEMORY_BYTES: usize = 32 * 1024 * 1024;
/// Native stack consumed by JavaScript evaluation.
pub const MAX_STACK_BYTES: usize = 256 * 1024;
/// Source limit, checked before parsing.
pub const MAX_SCRIPT_BYTES: usize = 4 * 1024 * 1024;

/// Runs one script; no QuickJS value or runtime crosses a thread boundary.
pub(crate) fn evaluate(
    script: &str,
    host_tx: UnboundedSender<HostRequest>,
    cell: Arc<ScriptCell>,
    cancel: CancellationToken,
) -> ScriptOutcome {
    if script.len() > MAX_SCRIPT_BYTES {
        return ScriptOutcome::Failed(format!("script exceeds {MAX_SCRIPT_BYTES} bytes"));
    }
    let completed = Rc::new(Cell::new(false));
    let exhausted = Rc::new(Cell::new(false));
    let run = || -> Result<(), String> {
        let runtime = Runtime::new().map_err(|error| error.to_string())?;
        runtime.set_memory_limit(MAX_MEMORY_BYTES);
        runtime.set_max_stack_size(MAX_STACK_BYTES);
        let probe = Arc::clone(&cell);
        let stop = cancel.clone();
        let budget_exhausted = Rc::clone(&exhausted);
        let mut operations = 0;
        runtime.set_interrupt_handler(Some(Box::new(move || {
            operations += 1;
            probe.set_operations(operations);
            if probe.terminated() || stop.is_cancelled() {
                return true;
            }
            if operations >= MAX_OPERATIONS {
                budget_exhausted.set(true);
                return true;
            }
            false
        })));
        // RegExpCompiler and Promise initialize the regex and async-function
        // prototypes used by the parser. Promise is hidden below; no job queue
        // is driven. No module loader, OS or network APIs are installed.
        let context = Context::custom::<(
            intrinsic::Eval,
            intrinsic::Json,
            intrinsic::RegExpCompiler,
            intrinsic::RegExp,
            intrinsic::Promise,
            intrinsic::MapSet,
            intrinsic::TypedArrays,
        )>(&runtime)
        .map_err(|error| error.to_string())?;
        context.with(|ctx| {
            let execute = || -> rquickjs::Result<()> {
                // Host evaluation needs the Eval intrinsic. Remove script entry
                // points, including constructors reachable through prototypes.
                ctx.eval::<(), _>(
                    r#"
for (const fn of [function(){}, function*(){}, async function(){}, async function*(){}]) {
    Object.defineProperty(Object.getPrototypeOf(fn), "constructor", {
        value: undefined, writable: false, configurable: false
    });
}
delete globalThis.eval;
delete globalThis.Function;
delete globalThis.Promise;
delete Math.random;
"#,
                )?;
                register_host_functions(&ctx, host_tx, Arc::clone(&cell), Rc::clone(&completed))?;
                let value: JsValue = ctx.eval(script)?;
                if !value.is_undefined() {
                    cell.push_output(&render_value(&ctx, &value)?);
                }
                Ok(())
            };
            execute().map_err(|error| {
                if error.is_exception() {
                    let exception = ctx.catch();
                    if let Some(exception) = exception
                        .clone()
                        .into_object()
                        .and_then(Exception::from_object)
                    {
                        return exception.to_string();
                    }
                    return render_value(&ctx, &exception)
                        .unwrap_or_else(|_| "JavaScript exception".to_owned());
                }
                error.to_string()
            })
        })?;
        if runtime.is_job_pending() {
            return Err(
                "Async JavaScript is not supported; use synchronous call(name, args)".to_owned(),
            );
        }
        Ok(())
    };
    let result = run();
    if cell.terminated() || cancel.is_cancelled() {
        ScriptOutcome::Terminated
    } else if completed.get() {
        ScriptOutcome::Completed
    } else if exhausted.get() {
        ScriptOutcome::Failed("JavaScript operation budget exhausted".to_owned())
    } else {
        match result {
            Ok(()) => ScriptOutcome::Completed,
            Err(error) => ScriptOutcome::Failed(error),
        }
    }
}

fn register_host_functions<'js>(
    ctx: &Ctx<'js>,
    host_tx: UnboundedSender<HostRequest>,
    cell: Arc<ScriptCell>,
    completed: Rc<Cell<bool>>,
) -> rquickjs::Result<()> {
    let call_tx = host_tx.clone();
    let call_cell = Arc::clone(&cell);
    ctx.globals().set("call", Function::new(ctx.clone(), move |ctx: Ctx<'js>, name: String, args: Opt<JsValue<'js>>| {
        if name == "run_code" || name == "wait_code" {
            return Err(Exception::throw_message(&ctx, &format!(
                "call(\"{name}\") is not allowed: code mode cannot invoke itself"
            )));
        }
        if !call_cell.begin_call() {
            return Err(stop_evaluation(&ctx, &format!(
                "host call budget exhausted ({} calls); split the work into several scripts",
                super::cell::MAX_HOST_CALLS
            )));
        }
        let arguments = match args.0 {
            Some(value) => to_json(&ctx, &value)?,
            None => Value::Null,
        };
        let arguments = if arguments.is_null() { json!({}) } else { arguments };
        let seq = call_cell.next_seq();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        call_tx.send(HostRequest::Call { seq, name, args: arguments, reply: reply_tx })
            .map_err(|_| Exception::throw_message(&ctx, "the host dispatcher stopped"))?;
        match reply_rx.recv() {
            Ok(HostReply::Value(value)) => ctx.json_parse(value.to_string()),
            Ok(HostReply::Abort(reason)) => Err(Exception::throw_message(&ctx, &reason)),
            Err(_) => Err(Exception::throw_message(&ctx, "the host dispatcher dropped the call")),
        }
    })?)?;

    let log_tx = host_tx.clone();
    let log_cell = Arc::clone(&cell);
    ctx.globals().set(
        "log",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, value: JsValue<'js>| -> rquickjs::Result<()> {
                let text = render_value(&ctx, &value)?;
                log_cell.push_output(&text);
                let seq = log_cell.next_seq();
                let _ = log_tx.send(HostRequest::Log { seq, text });
                Ok(())
            },
        )?,
    )?;

    let budget_cell = Arc::clone(&cell);
    ctx.globals().set(
        "budget",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
            ctx.json_parse(
                json!({
                    "calls_used": budget_cell.calls(),
                    "max_calls": super::cell::MAX_HOST_CALLS,
                    "operations": budget_cell.operations(),
                    "max_operations": MAX_OPERATIONS,
                    "max_output_bytes": budget_cell.max_output_bytes(),
                    "max_memory_bytes": MAX_MEMORY_BYTES,
                })
                .to_string(),
            )
        })?,
    )?;

    let complete_tx = host_tx.clone();
    let complete_cell = Arc::clone(&cell);
    ctx.globals().set(
        "complete",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, value: Opt<JsValue<'js>>| -> rquickjs::Result<()> {
                let value = value
                    .0
                    .unwrap_or_else(|| JsValue::new_undefined(ctx.clone()));
                let json = to_json(&ctx, &value)?;
                let text = if let Some(text) = value.as_string() {
                    text.to_string()?
                } else {
                    json.to_string()
                };
                complete_cell.push_output(&text);
                let seq = complete_cell.next_seq();
                let _ = complete_tx.send(HostRequest::Complete { seq, value: json });
                completed.set(true);
                Err(stop_evaluation(&ctx, "script completed"))
            },
        )?,
    )?;

    ctx.globals().set(
        "yield_control",
        Function::new(ctx.clone(), move || {
            let seq = cell.next_seq();
            let _ = host_tx.send(HostRequest::Yield { seq });
        })?,
    )?;
    Ok(())
}

/// Completion and exhausted host budgets must bypass JavaScript catch/finally,
/// just like QuickJS's own interrupt errors. The exception belongs to this ctx.
fn stop_evaluation(ctx: &Ctx<'_>, message: &str) -> rquickjs::Error {
    match Exception::from_message(ctx.clone(), message) {
        Ok(exception) => {
            // SAFETY: both handles are live, belong to this thread/context and
            // QuickJS only marks the existing Error object; ownership is retained.
            unsafe {
                rquickjs::qjs::JS_SetUncatchableError(
                    ctx.as_raw().as_ptr(),
                    exception.as_value().as_raw(),
                );
            }
            exception.throw()
        }
        Err(error) => error,
    }
}

/// JSON semantics at the bridge: undefined becomes null; cyclic objects and
/// BigInt fail explicitly. Strings remain unquoted in model-visible output.
fn to_json<'js>(ctx: &Ctx<'js>, value: &JsValue<'js>) -> rquickjs::Result<Value> {
    if value.is_promise() {
        return Err(Exception::throw_type(
            ctx,
            "Async JavaScript is not supported; use synchronous call(name, args)",
        ));
    }
    let Some(text) = ctx.json_stringify(value.clone())? else {
        return Ok(Value::Null);
    };
    serde_json::from_str(&text.to_string()?).map_err(|error| {
        Exception::throw_message(
            ctx,
            &format!("cannot convert JavaScript value to JSON: {error}"),
        )
    })
}

fn render_value<'js>(ctx: &Ctx<'js>, value: &JsValue<'js>) -> rquickjs::Result<String> {
    if let Some(text) = value.as_string() {
        text.to_string()
    } else {
        Ok(to_json(ctx, value)?.to_string())
    }
}
