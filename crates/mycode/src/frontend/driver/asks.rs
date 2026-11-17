//! Pending questions belong to their session, including while it is off screen.
use crate::app::AskPrompt;
use crate::frontend::view_model::FrontendAction;

use super::{Effect, FrontendDriver};

/// Identity captured when a frontend renders one question card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskIdentity {
    session: String,
    id: u64,
}

pub(super) struct PendingAsk {
    id: u64,
    questions: Vec<AskPrompt>,
    answers: Vec<String>,
    texts: Vec<String>,
    index: usize,
}

impl FrontendDriver {
    /// Captures the visible card so a delayed UI callback cannot answer its successor.
    pub fn open_ask_identity(&self) -> Option<AskIdentity> {
        self.vm.pending_ask.as_ref()?;
        let session = self.open_session_id()?;
        let id = self.pending_asks.get(&session)?.id;
        Some(AskIdentity { session, id })
    }

    /// Applies a rendered card's input only while that same issuance is visible.
    pub fn with_open_ask(
        &mut self,
        identity: &AskIdentity,
        out: &mut Vec<Effect>,
        update: impl FnOnce(&mut Self, &mut Vec<Effect>),
    ) {
        if self.vm.pending_ask.is_some()
            && self.open_session_id().as_deref() == Some(identity.session.as_str())
            && self
                .pending_asks
                .get(&identity.session)
                .is_some_and(|ask| ask.id == identity.id)
        {
            update(self, out);
        }
    }

    pub(super) fn receive_ask(
        &mut self,
        session: String,
        id: u64,
        questions: Vec<AskPrompt>,
        out: &mut Vec<Effect>,
    ) {
        if questions.is_empty()
            || self
                .pending_asks
                .get(&session)
                .is_some_and(|ask| ask.id >= id)
        {
            return;
        }
        let visible = self.open_session_id().as_deref() == Some(session.as_str());
        self.pending_asks.insert(
            session,
            PendingAsk {
                id,
                answers: vec![String::new(); questions.len()],
                questions: questions.clone(),
                texts: Vec::new(),
                index: 0,
            },
        );
        if visible {
            self.act(FrontendAction::AskRequested(questions), out);
        }
    }

    pub(super) fn resolve_ask(&mut self, session: &str, id: u64, out: &mut Vec<Effect>) {
        if !self
            .pending_asks
            .get(session)
            .is_some_and(|ask| ask.id == id)
        {
            return;
        }
        self.pending_asks.remove(session);
        if self.open_session_id().as_deref() == Some(session) {
            self.act(FrontendAction::AskAnswered, out);
        }
    }

    /// Save before navigation resets the visible form and its native inputs.
    pub(super) fn park_open_ask(&mut self) {
        let Some(session) = self.open_session_id() else {
            return;
        };
        if self.vm.pending_ask.is_some()
            && let Some(ask) = self.pending_asks.get_mut(&session)
        {
            ask.answers.clone_from(&self.vm.ask_answers);
            ask.texts.clone_from(&self.ask_texts);
            ask.index = self.ask_index;
        }
    }

    /// Called after the conversation snapshot replaced its view state.
    pub(super) fn restore_open_ask(&mut self) {
        let Some(session) = self.open_session_id() else {
            return;
        };
        let Some(ask) = self.pending_asks.get(&session) else {
            return;
        };
        self.vm.pending_ask = Some(ask.questions.clone());
        self.vm.ask_answers.clone_from(&ask.answers);
        self.ask_texts.clone_from(&ask.texts);
        self.ask_index = ask.index;
    }

    pub(super) fn take_open_ask(&mut self) -> Option<(String, u64)> {
        let session = self.open_session_id()?;
        let ask = self.pending_asks.remove(&session)?;
        Some((session, ask.id))
    }
}
