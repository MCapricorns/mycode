//! Self-update driving (check, download, install), the ask card's answer
//! flow, and the configuration export/import pipeline.
use std::path::PathBuf;

use mycode_app::BridgeCommand;

use super::{DriverMsg, Effect, FrontendDriver, ToastKind, UPDATE_RECHECK_AFTER};
use crate::forms;
use crate::view_model::{FrontendAction, UpdateState};

impl FrontendDriver {
    // ---- self-update ----

    /// Arms the daily re-check. Re-armed from the tick so it keeps firing
    /// for the process's whole uptime, the way the desktop's loop did.
    pub(crate) fn spawn_update_recheck(&self) {
        self.schedule(UPDATE_RECHECK_AFTER, || DriverMsg::UpdateRecheckTick);
    }

    /// Reads and clears the "this check was user-started" flag.
    pub fn take_manual_update_check(&mut self) -> bool {
        std::mem::take(&mut self.manual_update_check)
    }

    /// Runs one update check. `manual` marks a user click: its result may
    /// toast and, when a download is already staged, reopens the dialog.
    pub fn check_update(&mut self, manual: bool, out: &mut Vec<Effect>) {
        // Checking replaces the update state with `Checking`. Doing that
        // mid-download makes the next offer look idle and starts a second
        // download of the same package. A manual click instead opens the
        // dialog, which already renders the in-flight or staged state.
        if matches!(
            self.vm.update,
            UpdateState::Downloading { .. } | UpdateState::Ready { .. }
        ) {
            if manual {
                self.act(FrontendAction::UpdateDialogToggled(true), out);
            }
            return;
        }
        if manual {
            self.manual_update_check = true;
        }
        self.act(
            FrontendAction::UpdateStateChanged(UpdateState::Checking),
            out,
        );
        self.dispatch(BridgeCommand::CheckUpdate);
    }

    /// Downloads the newest offer into staging. One download at a time: a
    /// re-offer mid-download must not restart it.
    pub fn download_update(&mut self, out: &mut Vec<Effect>) {
        if matches!(
            self.vm.update,
            UpdateState::Downloading { .. } | UpdateState::Ready { .. }
        ) {
            return;
        }
        let Some(offer) = self.vm.last_offer.clone() else {
            return;
        };
        self.act(
            FrontendAction::UpdateStateChanged(UpdateState::Downloading {
                version: offer.version.clone(),
            }),
            out,
        );
        self.dispatch(BridgeCommand::DownloadUpdate { offer });
    }

    /// Installs the staged update: the swap helper is armed to relaunch,
    /// then the frontend quits via [`Effect::Quit`] so the swap can replace
    /// the binary and start the new version.
    pub fn install_update(&mut self, out: &mut Vec<Effect>) {
        let Some(prepared) = self.vm.prepared_update.clone() else {
            return;
        };
        if let Err(message) = mycode_app::arm_staged_update(&prepared, true) {
            self.act(FrontendAction::Failed(message), out);
            return;
        }
        self.update_armed = true;
        out.push(Effect::Quit);
    }

    /// The auto-update preference: persisted with the durable UI state; a
    /// check follows when it is turned on.
    pub fn toggle_auto_update(&mut self, enabled: bool, out: &mut Vec<Effect>) {
        self.act(FrontendAction::AutoUpdateToggled(enabled), out);
        self.persist_ui_state();
        if enabled {
            self.check_update(true, out);
        }
    }

    // ---- ask card ----

    /// One choice picked on an ask row: clears the row's free text, toggles
    /// the choice (multi-select aware), and submits when `submit` is set.
    pub fn pick_ask_choice(
        &mut self,
        index: usize,
        answer: String,
        submit: bool,
        out: &mut Vec<Effect>,
    ) {
        // Typing and a picked choice are mutually exclusive: the same way a
        // pick clears the text, fresh text clears the pick.
        if index < self.ask_texts.len() {
            self.ask_texts[index].clear();
        }
        out.push(Effect::AskTextCleared { index });
        let current = self.vm.ask_answers.get(index).cloned().unwrap_or_default();
        let multiple = self
            .vm
            .pending_ask
            .as_ref()
            .and_then(|rows| rows.get(index))
            .is_some_and(|row| row.multiple);
        let answer = mycode_app::builtin::toggle_ask_choice(&current, &answer, multiple);
        self.act(FrontendAction::AskChoicePicked { index, answer }, out);
        if submit {
            self.answer_ask(self.vm.ask_answers.clone(), out);
        }
    }

    /// Free text typed on one row clears any picked choice for it.
    pub fn ask_text_typed(&mut self, index: usize, text: String, out: &mut Vec<Effect>) {
        while self.ask_texts.len() <= index {
            self.ask_texts.push(String::new());
        }
        self.ask_texts[index] = text;
        if !self.ask_texts[index].trim().is_empty()
            && self
                .vm
                .ask_answers
                .get(index)
                .is_some_and(|answer| !answer.is_empty())
        {
            self.act(
                FrontendAction::AskChoicePicked {
                    index,
                    answer: String::new(),
                },
                out,
            );
        }
    }

    /// Advances the ask card one question. A required question needs a pick
    /// or free text before it moves on; the last question submits.
    pub fn ask_next(&mut self, out: &mut Vec<Effect>) {
        let Some(rows) = self.vm.pending_ask.clone() else {
            return;
        };
        let index = self.ask_index.min(rows.len().saturating_sub(1));
        let answered = self
            .vm
            .ask_answers
            .get(index)
            .is_some_and(|answer| !answer.trim().is_empty())
            || self
                .ask_texts
                .get(index)
                .is_some_and(|text| !text.trim().is_empty());
        if !answered && !rows[index].optional {
            self.toast(
                crate::i18n::t("Answer this question first", "先回答这一题"),
                ToastKind::Info,
                out,
            );
            return;
        }
        if index + 1 < rows.len() {
            self.ask_index = index + 1;
        } else {
            self.submit_ask(out);
        }
    }

    /// Steps the ask card back one question; earlier answers stay put.
    pub fn ask_back(&mut self, out: &mut Vec<Effect>) {
        if self.vm.pending_ask.is_none() {
            return;
        }
        self.ask_index = self.ask_index.saturating_sub(1);
        let _ = out;
    }

    /// Submits the card: merges picks with free text, or jumps back to the
    /// first required question still missing an answer.
    pub fn submit_ask(&mut self, out: &mut Vec<Effect>) {
        let Some(rows) = self.vm.pending_ask.as_ref() else {
            return;
        };
        match forms::collect_ask_answers(rows, &self.vm.ask_answers, &self.ask_texts) {
            Ok(answers) => self.answer_ask(answers, out),
            Err(index) => {
                // Jump back to the first required question still missing an
                // answer instead of losing the whole form's progress.
                self.ask_index = index;
                self.toast(
                    format!(
                        "{} {} {}",
                        crate::i18n::t("Answer question", "请先回答第"),
                        index + 1,
                        crate::i18n::t("before submitting.", "题后再提交"),
                    ),
                    ToastKind::Info,
                    out,
                );
            }
        }
    }

    /// Dismisses the pending ask without answering. The tool sees a
    /// catchable error; this does not cancel the turn.
    pub fn dismiss_ask(&mut self, out: &mut Vec<Effect>) {
        let Some(session_id) = self.open_session_id() else {
            return;
        };
        self.ask_texts.clear();
        self.ask_index = 0;
        self.act(FrontendAction::AskAnswered, out);
        self.dispatch(BridgeCommand::AskDismiss { session_id });
    }

    fn answer_ask(&mut self, answers: Vec<String>, out: &mut Vec<Effect>) {
        let Some(session_id) = self.open_session_id() else {
            return;
        };
        // Each question owns its text; the next request starts with empty fields.
        self.ask_texts.clear();
        self.ask_index = 0;
        self.act(FrontendAction::AskAnswered, out);
        self.dispatch(BridgeCommand::AskAnswer {
            session_id,
            answers,
        });
    }

    // ---- data export / import ----

    /// Exports the product configuration (settings, UI state, provider API
    /// keys) to `path`. Conversations never travel. The destination picker
    /// is the frontend's; the driver only takes the chosen file.
    pub fn export_data(&mut self, path: PathBuf) {
        self.dispatch(BridgeCommand::ExportData { path });
    }

    /// Applies one export bundle chosen in an open dialog: settings, UI
    /// state, and provider keys. Conversations are never imported.
    pub fn import_data(&mut self, path: PathBuf) {
        self.import_request = Some(path.clone());
        self.dispatch(BridgeCommand::ImportData { path });
    }
}
