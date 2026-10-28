use std::future::{Future, pending};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use mycode_tools::{
    EventStream, Message, Provider, ProviderError, ProviderErrorKind, Request, ToolRegistry,
    UserMessage,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::{Agent, AgentConfig, HookRunner, TurnEnv};

struct WaitingProvider {
    entered: Arc<Notify>,
    overflow: bool,
}

impl Provider for WaitingProvider {
    fn stream<'a>(
        &'a self,
        _request: &'a Request,
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            if self.overflow {
                return Err(ProviderError::with_message(
                    ProviderErrorKind::Rejected,
                    "context_length_exceeded",
                ));
            }
            self.entered.notify_one();
            pending().await
        })
    }
}

#[tokio::test]
async fn stop_interrupts_provider_setup_and_request_hooks() {
    for phase in ["provider", "prepare", "overflow"] {
        let entered = Arc::new(Notify::new());
        let provider = WaitingProvider {
            entered: Arc::clone(&entered),
            overflow: phase == "overflow",
        };
        let hook_entered = Arc::clone(&entered);
        let hooks = match phase {
            "prepare" => HookRunner::new().with_before_request(move |_| {
                let entered = Arc::clone(&hook_entered);
                async move {
                    entered.notify_one();
                    pending().await
                }
            }),
            "overflow" => HookRunner::new().with_context_overflow(move |_| {
                let entered = Arc::clone(&hook_entered);
                async move {
                    entered.notify_one();
                    pending().await
                }
            }),
            _ => HookRunner::new(),
        };
        let tools = ToolRegistry::new();
        let env = TurnEnv::new(&provider, &tools, &hooks);
        let mut agent = Agent::new(AgentConfig::new());
        let stop = async {
            entered.notified().await;
            env.cancel.cancel();
        };
        let work = agent.prompt(
            Message::User(UserMessage::text("inspect this project")),
            &env,
        );
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(work, stop) })
                .await
                .unwrap_or_else(|_| panic!("stop remained blocked in {phase}"));
        assert!(matches!(
            result.unwrap(),
            mycode_tools::events::TurnOutcome::Aborted
        ));
        if phase == "prepare" {
            // The turn never started, so the pending user entry is rolled back.
            assert!(agent.state().messages().is_empty());
        } else {
            assert!(agent.state().messages().iter().any(|message| {
                message.as_ref() == &Message::User(UserMessage::text("inspect this project"))
            }));
        }
    }
}
