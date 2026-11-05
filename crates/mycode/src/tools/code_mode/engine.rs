//! Embedded QuickJS and the code-mode host-function whitelist.
//!
//! The runtime lives entirely on the script thread. Tools, permissions and
//! async work remain in the shared dispatcher. Both blocking calls and Promise
//! callbacks use the same registry and call budget.

mod promises;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use rquickjs::context::intrinsic;
use rquickjs::function::Opt;
use rquickjs::{Context, Ctx, Exception, Function, Persistent, Promise, Runtime, Value as JsValue};

use self::promises::PendingCalls;
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
/// Cumulative UTF-8 log payload admitted outside the JavaScript heap.
/// The dispatcher may be unable to consume logs while an exclusive tool runs.
pub const MAX_LOG_BYTES: usize = 4 * 1024 * 1024;
/// Bounds queued empty/tiny logs and their retained journal entries as well.
pub const MAX_LOG_ENTRIES: usize = 1024;
/// Yield requests also occupy the native queue and retained journal.
pub const MAX_YIELD_REQUESTS: usize = 1024;

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
        let rejections = Rc::new(RefCell::new(HashMap::new()));
        let tracked = Rc::clone(&rejections);
        runtime.set_host_promise_rejection_tracker(Some(Box::new(
            move |ctx, promise, reason, handled| {
                let promise = Persistent::save(&ctx, promise);
                if handled {
                    tracked.borrow_mut().remove(&promise);
                } else {
                    tracked
                        .borrow_mut()
                        .insert(promise, exception_message(reason));
                }
            },
        )));
        // Promise jobs run on this thread; no module loader, OS or network APIs
        // are installed in the JavaScript context.
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
        let result = context.with(|ctx| {
            let pending = Rc::new(PendingCalls::new());
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
delete Math.random;
"#,
                )?;
                register_host_functions(
                    &ctx,
                    host_tx,
                    Arc::clone(&cell),
                    Rc::clone(&completed),
                    Rc::clone(&pending),
                )?;
                let script = ctx.eval_promise(script)?;
                let stopped = || {
                    completed.get() || exhausted.get() || cell.terminated() || cancel.is_cancelled()
                };
                // QuickJS async global evaluation resolves to { value: result }.
                let unhandled = || rejections.borrow().values().next().cloned();
                let evaluated = pending.settle(&ctx, script, stopped, unhandled)?;
                let mut value: JsValue = evaluated
                    .into_object()
                    .expect("async evaluation result")
                    .get("value")?;
                while let Some(promise) = value.clone().into_promise() {
                    value = pending.settle(&ctx, promise, stopped, unhandled)?;
                }
                if !value.is_undefined() {
                    cell.push_output(&render_value(&ctx, &value)?);
                }
                Ok(())
            };
            let result = execute().map_err(|error| {
                if error.is_exception() {
                    exception_message(ctx.catch())
                } else {
                    error.to_string()
                }
            });
            pending.clear();
            result
        });
        let unhandled = rejections.borrow().values().next().cloned();
        // Persistent rejection identities must not outlive the runtime.
        runtime.set_host_promise_rejection_tracker(None);
        rejections.borrow_mut().clear();
        result?;
        if let Some(reason) = unhandled {
            return Err(format!("Unhandled Promise rejection: {reason}"));
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
    pending: Rc<PendingCalls<'js>>,
) -> rquickjs::Result<()> {
    let call_tx = host_tx.clone();
    let call_cell = Arc::clone(&cell);
    ctx.globals().set(
        "call",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, name: String, args: Opt<JsValue<'js>>| {
                let (reply_tx, reply_rx) = std::sync::mpsc::channel();
                send_call(&ctx, &call_tx, &call_cell, name, args, reply_tx)?;
                match reply_rx.recv() {
                    Ok((_, HostReply::Value(value))) => ctx.json_parse(value.to_string()),
                    Ok((_, HostReply::Abort(reason))) => {
                        Err(Exception::throw_message(&ctx, &reason))
                    }
                    Err(_) => Err(Exception::throw_message(
                        &ctx,
                        "the host dispatcher dropped the call",
                    )),
                }
            },
        )?,
    )?;

    let call_tx = host_tx.clone();
    let call_cell = Arc::clone(&cell);
    ctx.globals().set(
        "callAsync",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, name: String, args: Opt<JsValue<'js>>| {
                let (promise, resolve, reject) = Promise::new(&ctx)?;
                let seq = send_call(&ctx, &call_tx, &call_cell, name, args, pending.sender())?;
                pending.insert(seq, resolve, reject);
                Ok::<_, rquickjs::Error>(promise)
            },
        )?,
    )?;

    let log_tx = host_tx.clone();
    let log_cell = Arc::clone(&cell);
    let log_budget = Rc::new(Cell::new((0_usize, 0_usize)));
    let log_usage = Rc::clone(&log_budget);
    ctx.globals().set(
        "log",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, value: JsValue<'js>| -> rquickjs::Result<()> {
                // Admission is cumulative, not reset when the dispatcher drains:
                // it bounds both the native queue and the retained journal. Check
                // the entry count before converting another potentially large value.
                if log_usage.get().0 >= MAX_LOG_ENTRIES {
                    return Err(stop_evaluation(&ctx, "log budget exhausted (entry limit)"));
                }
                let text = render_value(&ctx, &value)?;
                // JSON conversion may re-enter log through a toJSON method or
                // getter. Never overwrite budget usage charged by that inner log.
                let (entries, bytes) = log_usage.get();
                if entries >= MAX_LOG_ENTRIES {
                    return Err(stop_evaluation(&ctx, "log budget exhausted (entry limit)"));
                }
                if text.len() > MAX_LOG_BYTES.saturating_sub(bytes) {
                    return Err(stop_evaluation(&ctx, "log budget exhausted (byte limit)"));
                }
                log_usage.set((entries + 1, bytes + text.len()));
                log_cell.push_output(&text);
                let seq = log_cell.next_seq();
                let _ = log_tx.send(HostRequest::Log { seq, text });
                Ok(())
            },
        )?,
    )?;

    let yields = Rc::new(Cell::new(0_usize));
    let yield_usage = Rc::clone(&yields);
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
                    "logs_used": log_budget.get().0,
                    "max_logs": MAX_LOG_ENTRIES,
                    "log_bytes_used": log_budget.get().1,
                    "max_log_bytes": MAX_LOG_BYTES,
                    "yields_used": yield_usage.get(),
                    "max_yields": MAX_YIELD_REQUESTS,
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
        Function::new(ctx.clone(), move |ctx: Ctx<'js>| -> rquickjs::Result<()> {
            if yields.get() >= MAX_YIELD_REQUESTS {
                return Err(stop_evaluation(&ctx, "yield budget exhausted"));
            }
            yields.set(yields.get() + 1);
            let seq = cell.next_seq();
            let _ = host_tx.send(HostRequest::Yield { seq });
            Ok(())
        })?,
    )?;
    Ok(())
}

fn send_call<'js>(
    ctx: &Ctx<'js>,
    host_tx: &UnboundedSender<HostRequest>,
    cell: &ScriptCell,
    name: String,
    args: Opt<JsValue<'js>>,
    reply: std::sync::mpsc::Sender<(u64, HostReply)>,
) -> rquickjs::Result<u64> {
    if name == "run_code" || name == "wait_code" {
        return Err(Exception::throw_message(
            ctx,
            &format!("call(\"{name}\") is not allowed: code mode cannot invoke itself"),
        ));
    }
    if !cell.begin_call() {
        return Err(stop_evaluation(
            ctx,
            &format!(
                "host call budget exhausted ({} calls); split the work into several scripts",
                super::cell::MAX_HOST_CALLS
            ),
        ));
    }
    let args = match args.0 {
        Some(value) => to_json(ctx, &value)?,
        None => Value::Null,
    };
    let args = if args.is_null() { json!({}) } else { args };
    let seq = cell.next_seq();
    host_tx
        .send(HostRequest::Call {
            seq,
            name,
            args,
            reply,
        })
        .map_err(|_| Exception::throw_message(ctx, "the host dispatcher stopped"))?;
    Ok(seq)
}

fn exception_message(value: JsValue<'_>) -> String {
    if let Some(exception) = value.clone().into_object().and_then(Exception::from_object) {
        exception.to_string()
    } else if let Some(text) = value.as_string() {
        text.to_string()
            .unwrap_or_else(|_| "JavaScript exception".to_owned())
    } else {
        format!("{value:?}")
    }
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
            "await the Promise before passing its value to a host function",
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
