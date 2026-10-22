//! `HookRunner` defines the agent loop's request hooks.
//!
//! Production installs [`HookRunner::with_before_request`] so history
//! compaction can rewrite a provider request immediately before it is sent,
//! and [`HookRunner::with_context_overflow`] so a request the provider
//! refused as too large is compacted and sent once more.
//! There is no before-tool observer: turns do not snapshot files.

use mycode_tools::Request;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

type BeforeRequestFuture = Pin<Box<dyn Future<Output = Request> + Send>>;
type BeforeRequest = Arc<dyn Fn(Request) -> BeforeRequestFuture + Send + Sync>;
type OverflowFuture = Pin<Box<dyn Future<Output = Option<Request>> + Send>>;
type ContextOverflow = Arc<dyn Fn(Request) -> OverflowFuture + Send + Sync>;

/// Hook runner for the loop's request rewrites.
pub struct HookRunner {
    before_request: Option<BeforeRequest>,
    context_overflow: Option<ContextOverflow>,
}

impl HookRunner {
    /// An empty runner.
    pub fn new() -> Self {
        Self {
            before_request: None,
            context_overflow: None,
        }
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

    /// Shrinks a request the provider refused as over the context window.
    ///
    /// The hook gets the refused request with the history as the agent
    /// holds it and returns a smaller one, or `None` when it cannot shrink
    /// it. Like the before-request rewrite, the returned `messages` are
    /// written back onto the in-memory history. A turn retries this way
    /// once per response, and only before any output arrived.
    pub fn with_context_overflow<Fut>(
        mut self,
        hook: impl Fn(Request) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        Fut: Future<Output = Option<Request>> + Send + 'static,
    {
        self.context_overflow = Some(Arc::new(move |request| Box::pin(hook(request))));
        self
    }

    /// Runs the before-request rewrite, or returns the request unchanged.
    pub async fn prepare_request(&self, request: Request) -> Request {
        match &self.before_request {
            Some(hook) => hook(request).await,
            None => request,
        }
    }

    /// Whether a context overflow hook is installed.
    pub fn recovers_overflow(&self) -> bool {
        self.context_overflow.is_some()
    }

    /// Runs the context overflow hook; `None` when there is none or it
    /// could not shrink the request.
    pub async fn shrink_overflowed(&self, request: Request) -> Option<Request> {
        match &self.context_overflow {
            Some(hook) => hook(request).await,
            None => None,
        }
    }
}

impl Default for HookRunner {
    fn default() -> Self {
        Self::new()
    }
}
