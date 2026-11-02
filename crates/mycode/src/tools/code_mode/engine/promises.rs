//! Promise settlement stays on the JavaScript thread; tool replies carry only JSON.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use rquickjs::promise::PromiseState;
use rquickjs::{Ctx, Exception, Function, Promise, Value};

use super::super::dispatch::HostReply;

/// Bounds microtask chains that can keep scheduling without making a host call.
pub(super) const MAX_PROMISE_JOBS: usize = 10_000;

/// Ctx::execute_pending_job discards thrown exceptions. Preserve them here so
/// uncatchable host-budget errors keep their cause when raised after an await.
fn execute_pending_job(ctx: &Ctx<'_>) -> rquickjs::Result<bool> {
    let mut job_context = std::ptr::null_mut();
    // SAFETY: this script thread owns the runtime and its sole context. QuickJS
    // writes a live context pointer on error; no value crosses thread boundaries.
    let status = unsafe {
        rquickjs::qjs::JS_ExecutePendingJob(
            rquickjs::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()),
            &mut job_context,
        )
    };
    if status < 0 {
        debug_assert_eq!(job_context, ctx.as_raw().as_ptr());
        Err(rquickjs::Error::Exception)
    } else {
        Ok(status != 0)
    }
}

type Reply = (u64, HostReply);

pub(super) struct PendingCalls<'js> {
    resolvers: RefCell<HashMap<u64, (Function<'js>, Function<'js>)>>,
    replies: Receiver<Reply>,
    sender: Sender<Reply>,
    jobs: Cell<usize>,
}

impl<'js> PendingCalls<'js> {
    pub(super) fn new() -> Self {
        let (sender, replies) = mpsc::channel();
        Self {
            resolvers: RefCell::new(HashMap::new()),
            replies,
            sender,
            jobs: Cell::new(0),
        }
    }

    pub(super) fn sender(&self) -> Sender<Reply> {
        self.sender.clone()
    }

    pub(super) fn insert(&self, seq: u64, resolve: Function<'js>, reject: Function<'js>) {
        self.resolvers.borrow_mut().insert(seq, (resolve, reject));
    }

    /// Release JS values before the context and runtime are destroyed, including
    /// when completion, cancellation or an exception leaves calls outstanding.
    pub(super) fn clear(&self) {
        self.resolvers.borrow_mut().clear();
    }

    fn resolve(&self, ctx: &Ctx<'js>, (seq, reply): Reply) -> rquickjs::Result<()> {
        let callbacks = self.resolvers.borrow_mut().remove(&seq);
        if let Some((resolve, reject)) = callbacks {
            match reply {
                HostReply::Value(value) => {
                    let value = ctx.json_parse(value.to_string())?;
                    resolve.call::<_, ()>((value,))?;
                }
                HostReply::Abort(reason) => {
                    reject.call::<_, ()>((Exception::from_message(ctx.clone(), &reason)?,))?;
                }
            }
        }
        Ok(())
    }

    /// Run ready microtasks and wake immediately on tool replies. Idle waits are
    /// on this script's OS thread, never on the application's Tokio executor.
    pub(super) fn settle(
        &self,
        ctx: &Ctx<'js>,
        script: Promise<'js>,
        stopped: impl Fn() -> bool,
        unhandled: impl Fn() -> Option<String>,
    ) -> rquickjs::Result<Value<'js>> {
        loop {
            if stopped() {
                return Err(super::stop_evaluation(ctx, "script stopped"));
            }
            if script.state() == PromiseState::Rejected {
                return script.result().expect("rejected promise has a result");
            }
            while let Ok(reply) = self.replies.try_recv() {
                self.resolve(ctx, reply)?;
            }
            if execute_pending_job(ctx)? {
                self.jobs.set(self.jobs.get() + 1);
                if self.jobs.get() >= MAX_PROMISE_JOBS {
                    return Err(super::stop_evaluation(
                        ctx,
                        "JavaScript job budget exhausted",
                    ));
                }
                continue;
            }
            if let Some(reason) = unhandled() {
                return Err(Exception::throw_message(
                    ctx,
                    &format!("Unhandled Promise rejection: {reason}"),
                ));
            }
            if self.resolvers.borrow().is_empty() {
                return script.result().unwrap_or_else(|| {
                    Err(Exception::throw_message(
                        ctx,
                        "Promise is pending with no host calls or JavaScript jobs to resolve it",
                    ))
                });
            }
            // Cancellation also covers a dispatcher that failed before sending
            // replies. Successful replies wake recv_timeout without this delay.
            match self.replies.recv_timeout(Duration::from_millis(10)) {
                Ok(reply) => self.resolve(ctx, reply)?,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Exception::throw_message(ctx, "the host dispatcher stopped"));
                }
            }
        }
    }
}
