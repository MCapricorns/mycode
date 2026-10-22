//! Host-backed tools bridged into the agent's registry: `ask_user`.

use std::future::Future;
use std::pin::Pin;

use tokio_util::sync::CancellationToken;

use crate::BridgeEvent;

/// Routes user answers from the UI to the pending `ask_user` tool.
#[derive(Default)]
struct AskRouter {
    next: std::sync::atomic::AtomicU64,
    pending: parking_lot::Mutex<std::collections::HashMap<String, PendingAsk>>,
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
        self.pending.lock().insert(
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
            .remove(session_id)
            .is_some_and(|ask| ask.answer.send(answers).is_ok())
    }

    fn dismiss(&self, session_id: &str) -> bool {
        self.pending.lock().remove(session_id).is_some()
    }

    /// Removes one registration, but only when it still maps to that slot's
    /// issuance. A follow-up turn for the same session re-registers with a
    /// newer generation and must not lose its channel to the old turn's exit.
    fn retire(&self, slot: &AskSlot) {
        let mut pending = self.pending.lock();
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

impl mycode_agent::builtin::AskChannel for BridgeAskChannel {
    fn ask<'a>(
        &'a self,
        questions: &'a [mycode_agent::builtin::AskQuestion],
        cancel: &'a CancellationToken,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Vec<mycode_agent::builtin::AskAnswer>, mycode_agent::ToolError>,
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
                    return Err(mycode_agent::ToolError::Execution(format!(
                        "cancelled{}",
                        mycode_agent::tool::INTERRUPTED_NOTE
                    )));
                }
                answers = answer => answers.map_err(|_| mycode_agent::builtin::user_dismissed())?,
            };
            Ok(questions
                .iter()
                .enumerate()
                .map(|(index, question)| mycode_agent::builtin::AskAnswer {
                    question: question.question.clone(),
                    answer: answers.get(index).cloned().unwrap_or_default(),
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
        use mycode_agent::builtin::{AskChannel as _, AskQuestion};

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
