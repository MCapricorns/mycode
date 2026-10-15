//! Host-backed tools bridged into the agent's registry: `ask_user` and the
//! settings-configured web search backend.

use std::future::Future;
use std::pin::Pin;

use mycode_config::{AppSettings, HomeLayout, read_app_settings, read_provider_secrets};
use tokio_util::sync::CancellationToken;

use crate::BridgeEvent;
use crate::settings_io::render_config_error;

/// Environment variable carrying the Querit API key, checked before the vault
/// (the pi agent's querit plugin resolves this variable before its own config
/// file, so a machine already set up for pi keeps working).
const QUERIT_KEY_ENV: &str = "QUERIT_API_KEY";
/// Environment variable carrying the AnySearch API key. Anonymous traffic is
/// allowed when this and the vault entry are both absent.
const ANYSEARCH_KEY_ENV: &str = "ANYSEARCH_API_KEY";

/// Routes user answers from the UI to the pending `ask_user` tool.
#[derive(Default)]
struct AskRouter {
    next: std::sync::atomic::AtomicU64,
    pending: std::sync::Mutex<std::collections::HashMap<String, PendingAsk>>,
}

/// One registered answer channel with its issuance generation.
struct PendingAsk {
    generation: u64,
    answer: tokio::sync::oneshot::Sender<Vec<String>>,
}

impl AskRouter {
    fn register(&self, session_id: &str) -> (tokio::sync::oneshot::Receiver<Vec<String>>, AskSlot) {
        let generation = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().expect("ask router").insert(
            session_id.to_owned(),
            PendingAsk {
                generation,
                answer: tx,
            },
        );
        (
            rx,
            AskSlot {
                session_id: session_id.to_owned(),
                generation,
            },
        )
    }

    fn deliver(&self, session_id: &str, answers: Vec<String>) -> bool {
        self.pending
            .lock()
            .expect("ask router")
            .remove(session_id)
            .is_some_and(|ask| ask.answer.send(answers).is_ok())
    }

    fn dismiss(&self, session_id: &str) -> bool {
        self.pending
            .lock()
            .expect("ask router")
            .remove(session_id)
            .is_some()
    }

    /// Removes one registration, but only when it still maps to that slot's
    /// issuance. A follow-up turn for the same session re-registers with a
    /// newer generation and must not lose its channel to the old turn's exit.
    fn retire(&self, slot: &AskSlot) {
        let mut pending = self.pending.lock().expect("ask router");
        if pending
            .get(&slot.session_id)
            .is_some_and(|ask| ask.generation == slot.generation)
        {
            pending.remove(&slot.session_id);
        }
    }
}

/// Removes one session's registered ask channel when the ask that registered
/// it ends: answered, dismissed, cancelled, or dropped with its turn.
///
/// Without this, every session that ever asked left one dead sender in the
/// process-wide router, including sessions the user later deleted.
pub(crate) struct AskSlot {
    session_id: String,
    generation: u64,
}

impl Drop for AskSlot {
    fn drop(&mut self) {
        if let Some(router) = ASK_ROUTER.get() {
            router.retire(self);
        }
    }
}

/// Process-wide ask router; one pending ask per session.
static ASK_ROUTER: std::sync::OnceLock<AskRouter> = std::sync::OnceLock::new();

/// Registers a new answer channel for this session, replacing any previous
/// one. The returned slot removes the registration when the ask ends.
pub(crate) fn register_ask(
    session_id: &str,
) -> (tokio::sync::oneshot::Receiver<Vec<String>>, AskSlot) {
    ASK_ROUTER
        .get_or_init(AskRouter::default)
        .register(session_id)
}

pub(crate) fn deliver_ask_answer(session_id: &str, answers: Vec<String>) -> Result<(), String> {
    let router = ASK_ROUTER.get_or_init(AskRouter::default);
    if router.deliver(session_id, answers) {
        Ok(())
    } else {
        Err("no pending question for this session".to_owned())
    }
}

/// Drops a pending ask without answering. The tool returns a catchable error
/// and the program keeps running. Stopping the turn is a separate cancel.
pub(crate) fn dismiss_ask(session_id: &str) -> Result<(), String> {
    let router = ASK_ROUTER.get_or_init(AskRouter::default);
    if router.dismiss(session_id) {
        Ok(())
    } else {
        Err("no pending question for this session".to_owned())
    }
}

/// Host channel forwarding `ask_user` waits through the UI event stream.
pub(crate) struct BridgeAskChannel {
    pub(crate) session_id: String,
    pub(crate) events: crate::BridgeEventTx,
}

impl mycode_tools::builtin::AskChannel for BridgeAskChannel {
    fn ask<'a>(
        &'a self,
        questions: &'a [mycode_tools::builtin::AskQuestion],
        cancel: &'a CancellationToken,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Vec<mycode_tools::builtin::AskAnswer>, mycode_tools::ToolError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            // One answer channel per question set. A turn-wide channel was
            // spent by the first ask, so a second ask in the same turn showed
            // its card yet failed at once as dismissed. Registering before
            // the card goes out means an answer can never beat its channel,
            // and the slot retires the entry however this wait ends.
            let (answer, _slot) = register_ask(&self.session_id);
            let rows = questions
                .iter()
                .map(|question| crate::protocol::AskPrompt {
                    question: question.question.clone(),
                    choices: question.choices.clone(),
                    optional: question.optional,
                    multiple: question.multiple,
                })
                .collect();
            let _ = self.events.send(BridgeEvent::AskRequested {
                session_id: self.session_id.clone(),
                questions: rows,
            });
            let answers = tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    return Err(mycode_tools::ToolError::Execution("cancelled".into()));
                }
                answers = answer => answers.map_err(|_| mycode_tools::builtin::user_dismissed())?,
            };
            Ok(questions
                .iter()
                .enumerate()
                .map(|(index, question)| mycode_tools::builtin::AskAnswer {
                    question: question.question.clone(),
                    answer: answers.get(index).cloned().unwrap_or_default(),
                })
                .collect())
        })
    }
}

/// Builds the web client for the enabled backend, or the first builtin that
/// already has a key (env or vault).
fn web_client(home: &HomeLayout) -> Result<crate::web_client::WebClient, String> {
    let settings = read_app_settings(home).map_err(|error| render_config_error(&error))?;
    let secrets = read_provider_secrets(home).ok();
    let backend = resolve_web_backend(&settings, secrets.as_ref())?;
    let kind = crate::web_client::SearchKind::parse(&backend.kind)
        .ok_or_else(|| format!("unknown search backend kind '{}'", backend.kind))?;
    let key = web_backend_key(&backend, secrets.as_ref());
    if key.is_none() && kind == crate::web_client::SearchKind::Querit {
        return Err(format!(
            "no API key for the '{}' search backend — paste one in Settings or set {QUERIT_KEY_ENV}",
            backend.id
        ));
    }
    let transport = crate::web_client::transport::ReqwestWebTransport::new()
        .map_err(|error| error.to_string())?;
    crate::web_client::WebClient::new(&backend.endpoint, key, std::sync::Arc::new(transport))
        .map_err(|_| "the search backend endpoint violates the URL policy".to_owned())
        .map(|client| client.with_kind(kind))
}

fn web_backend_key(
    backend: &mycode_config::WebBackendSettings,
    secrets: Option<&mycode_config::ProviderSecrets>,
) -> Option<String> {
    let env_name = match backend.kind.as_str() {
        "querit" => Some(QUERIT_KEY_ENV),
        "anysearch" => Some(ANYSEARCH_KEY_ENV),
        _ => None,
    };
    env_name
        .and_then(|name| std::env::var(name).ok())
        .map(|value| mycode_config::normalize_api_key(&value))
        .filter(|value| !value.is_empty())
        .or_else(|| {
            secrets.and_then(|secrets| {
                secrets
                    .key(&format!("web-{}", backend.id))
                    .map(mycode_config::normalize_api_key)
                    .filter(|value| !value.is_empty())
            })
        })
}

fn resolve_web_backend(
    settings: &AppSettings,
    secrets: Option<&mycode_config::ProviderSecrets>,
) -> Result<mycode_config::WebBackendSettings, String> {
    if let Some(backend) = settings.web.backends.iter().find(|backend| backend.enabled) {
        return Ok(backend.clone());
    }
    let mut candidates = mycode_config::builtin_web_backends();
    for backend in &settings.web.backends {
        if let Some(slot) = candidates.iter_mut().find(|item| item.id == backend.id) {
            *slot = backend.clone();
        } else {
            candidates.push(backend.clone());
        }
    }
    if let Some(backend) = candidates
        .iter()
        .find(|backend| web_backend_key(backend, secrets).is_some())
    {
        return Ok(backend.clone());
    }
    Err("no search backend is ready — paste a Querit or AnySearch API key in Settings".to_owned())
}

/// Host channel for the model's web tools: the same bounded client the
/// settings page configures.
pub(crate) struct BridgeWebHost {
    pub(crate) home: HomeLayout,
}

impl mycode_tools::builtin::WebHost for BridgeWebHost {
    fn search<'a>(
        &'a self,
        query: &'a str,
        max_results: usize,
        cancel: &'a CancellationToken,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<mycode_tools::builtin::WebHit>, mycode_tools::ToolError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let fail = |message: String| mycode_tools::ToolError::Execution(message);
            let client = web_client(&self.home).map_err(fail)?;
            let results = client
                .search(query, max_results, cancel.clone())
                .await
                .map_err(|error| fail(crate::web_client::tool_failure("search", &error)))?;
            Ok(results
                .into_iter()
                .map(|result| mycode_tools::builtin::WebHit {
                    url: result.url,
                    title: result.title,
                    snippet: result.snippet,
                })
                .collect())
        })
    }

    fn fetch_content<'a>(
        &'a self,
        urls: &'a [String],
        cancel: &'a CancellationToken,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Vec<mycode_tools::builtin::WebPage>, mycode_tools::ToolError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let fail = |message: String| mycode_tools::ToolError::Execution(message);
            let client = web_client(&self.home).map_err(fail)?;
            let pages = client
                .contents(urls, cancel.clone())
                .await
                .map_err(|error| fail(crate::web_client::tool_failure("fetch", &error)))?;
            Ok(pages
                .into_iter()
                .map(|page| mycode_tools::builtin::WebPage {
                    url: page.url,
                    content: page.content,
                    truncated: page.truncated,
                })
                .collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{deliver_ask_answer, dismiss_ask, register_ask};

    #[test]
    fn a_turns_exit_retires_only_its_own_registration() {
        let (_first_rx, first_slot) = register_ask("ses1-ask-generations");
        let (_second_rx, _second_slot) = register_ask("ses1-ask-generations");
        // The first turn's registry drops; the newer registration must stay.
        drop(first_slot);
        assert!(
            deliver_ask_answer("ses1-ask-generations", vec!["yes".to_owned()]).is_ok(),
            "the successor turn's registration was retired by the old turn"
        );
    }

    #[test]
    fn dropping_the_slot_removes_the_pending_entry() {
        let (_rx, slot) = register_ask("ses2-ask-retire");
        drop(_rx);
        drop(slot);
        assert!(deliver_ask_answer("ses2-ask-retire", Vec::new()).is_err());
        assert!(dismiss_ask("ses2-ask-retire").is_err());
    }

    #[tokio::test]
    async fn one_turn_can_ask_more_than_once() {
        use mycode_tools::builtin::{AskChannel as _, AskQuestion};

        let (events, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let channel = super::BridgeAskChannel {
            session_id: "ses4-ask-twice".to_owned(),
            events,
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let questions = vec![AskQuestion {
            question: "which one?".to_owned(),
            choices: Vec::new(),
            optional: false,
            multiple: false,
        }];
        for reply in ["first", "second"] {
            let user = async {
                let request = requests.recv().await.expect("question card");
                assert!(matches!(request, crate::BridgeEvent::AskRequested { .. }));
                deliver_ask_answer("ses4-ask-twice", vec![reply.to_owned()])
                    .expect("the ask must be waiting for this answer");
            };
            let (answers, ()) = tokio::join!(channel.ask(&questions, &cancel), user);
            let answers = answers.expect("answered ask");
            assert_eq!(answers[0].answer, reply);
        }
        // An answer with no question on screen is refused, not buffered for
        // the next question.
        assert!(deliver_ask_answer("ses4-ask-twice", vec!["stale".to_owned()]).is_err());
    }

    #[test]
    fn an_answered_ask_leaves_no_entry_behind() {
        let (rx, slot) = register_ask("ses3-ask-answer");
        assert!(deliver_ask_answer("ses3-ask-answer", vec!["a".to_owned()]).is_ok());
        let answers = rx.blocking_recv().expect("delivered answers");
        assert_eq!(answers, vec!["a".to_owned()]);
        drop(slot);
        assert!(dismiss_ask("ses3-ask-answer").is_err());
    }
}
