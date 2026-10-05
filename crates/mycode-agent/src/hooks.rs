//! `HookRunner` defines the agent loop's request hook.
//!
//! Production installs [`HookRunner::with_before_request`] so history
//! compaction can rewrite a provider request immediately before it is sent.
//! There is no before-tool observer: turns do not snapshot files.

use mycode_core::Request;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

type BeforeRequestFuture = Pin<Box<dyn Future<Output = Request> + Send>>;
type BeforeRequest = Arc<dyn Fn(Request) -> BeforeRequestFuture + Send + Sync>;

/// Hook runner for the loop's request rewrite.
pub struct HookRunner {
    before_request: Option<BeforeRequest>,
}

impl HookRunner {
    /// An empty runner.
    pub fn new() -> Self {
        Self {
            before_request: None,
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

    /// Runs the before-request rewrite, or returns the request unchanged.
    pub async fn prepare_request(&self, request: Request) -> Request {
        match &self.before_request {
            Some(hook) => hook(request).await,
            None => request,
        }
    }
}

impl Default for HookRunner {
    fn default() -> Self {
        Self::new()
    }
}
