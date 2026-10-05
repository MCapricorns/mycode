//! `HookRunner` defines the agent loop's hook dispatch points.
//!
//! Production installs two of them through
//! [`HookRunner::with_before_request`] (history compaction immediately
//! before a provider request) and [`HookRunner::with_before_tool`] (an
//! observer fired after tool-call admission, right before dispatch).

use mycode_core::Request;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Hook runner for the loop's dispatch points.
///
/// The live production paths are [`prepare_request`](HookRunner::prepare_request),
/// which runs the installed before-request rewrite (history compaction), and
/// [`observe_before_tool`](HookRunner::observe_before_tool), which fires the
/// installed before-tool observer immediately before dispatch. An observer
/// error or panic fails the tool call instead of letting the mutation run.
/// The observer clones what it needs while invoked and returns a future;
/// asynchronous observers may offload blocking work (file snapshots) onto
/// `spawn_blocking` instead of stalling the calling executor.
type BeforeToolFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
type BeforeToolObserver = Arc<dyn Fn(&str, &Value) -> BeforeToolFuture + Send + Sync>;
type BeforeRequestFuture = Pin<Box<dyn Future<Output = Request> + Send>>;
type BeforeRequest = Arc<dyn Fn(Request) -> BeforeRequestFuture + Send + Sync>;

pub struct HookRunner {
    before_tool: Option<BeforeToolObserver>,
    before_request: Option<BeforeRequest>,
}

impl HookRunner {
    /// An empty runner.
    pub fn new() -> Self {
        Self {
            before_tool: None,
            before_request: None,
        }
    }

    /// Installs an observer fired after tool-call admission and argument
    /// validation, immediately before dispatch. The observer returns a
    /// future that is awaited before the tool runs. [`Err`] and panics —
    /// during the call or while polling — fail the tool call.
    pub fn with_before_tool<Fut>(
        mut self,
        observer: impl Fn(&str, &Value) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.before_tool = Some(Arc::new(move |tool, args| Box::pin(observer(tool, args))));
        self
    }

    /// Rewrites the provider request immediately before it is sent.
    ///
    /// Used for history compaction: the rewritten `messages` are written
    /// back onto the in-memory agent history after this hook returns.
    pub fn with_before_request<Fut>(
        mut self,
        hook: impl Fn(Request) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        Fut: Future<Output = Request> + Send + 'static,
    {
        self.before_request = Some(Arc::new(move |request| Box::pin(hook(request))));
        self
    }

    /// Runs the before-request rewrite, or returns the request unchanged.
    pub async fn prepare_request(&self, request: Request) -> Request {
        match &self.before_request {
            Some(hook) => hook(request).await,
            None => request,
        }
    }

    /// Fires the before-tool observer, if installed.
    ///
    /// # Errors
    ///
    /// Returns the observer's error, or a visible panic message, so the
    /// dispatcher can refuse to run the tool.
    pub async fn observe_before_tool(&self, tool: &str, args: &Value) -> Result<(), String> {
        let Some(observer) = &self.before_tool else {
            return Ok(());
        };
        let future =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(tool, args))) {
                Ok(future) => future,
                Err(_) => {
                    return Err("checkpoint failed: before_tool observer panicked".to_owned());
                }
            };
        // Polling is isolated through a task so an observer panic unwinds
        // into a JoinError instead of the dispatch path.
        match tokio::spawn(future).await {
            Ok(result) => result,
            Err(_) => Err("checkpoint failed: before_tool observer panicked".to_owned()),
        }
    }
}

impl Default for HookRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::HookRunner;

    #[tokio::test]
    async fn before_tool_error_is_returned() {
        let hooks = HookRunner::new()
            .with_before_tool(|_, _| async { Err("checkpoint failed: oversized".to_owned()) });
        let error = hooks
            .observe_before_tool("write", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(error.contains("oversized"), "{error}");
    }
}
