//! Destructive conversation operations. Resolve ledger identities before a
//! rewind, and ask for explicit confirmation before deleting durable data.
use gpui_kit::{Context, Window};
use mycode_app::{
    ActiveConversation, BranchId, BridgeCommand, BridgeReply, SessionEventId, SessionId,
};

use super::Workspace;
use crate::view_model::{ConversationEntry, DesktopAction, EntryKind};

fn previous_event(entries: &[ConversationEntry], index: usize) -> Option<&str> {
    entries
        .get(..index)?
        .iter()
        .rev()
        .find_map(|entry| SessionEventId::parse(&entry.event_id).map(|_| entry.event_id.as_str()))
}

fn can_rewind(conversation: &ActiveConversation, index: usize) -> bool {
    conversation.entries.get(index).is_some_and(|entry| {
        entry.kind == EntryKind::UserMessage
            && (previous_event(&conversation.entries, index).is_some()
                || conversation.older_before.is_some())
    })
}

struct RecallRequest {
    session: SessionId,
    branch: BranchId,
    head: String,
    edit: Option<String>,
    generation: u64,
}

impl Workspace {
    pub(crate) fn can_rewind_message(&self, index: usize) -> bool {
        !self.vm.sending
            && self
                .vm
                .active
                .as_ref()
                .is_some_and(|active| can_rewind(active, index))
    }

    /// Rewinds to before this message and restores its text for editing.
    pub(crate) fn on_edit_message(&mut self, index: usize, cx: &mut Context<Self>) {
        self.recall_at(index, true, cx);
    }

    /// Rewinds the conversation only; working-tree files are never restored.
    pub(crate) fn on_recall_message(&mut self, index: usize, cx: &mut Context<Self>) {
        self.recall_at(index, false, cx);
    }

    fn recall_at(&mut self, index: usize, edit: bool, cx: &mut Context<Self>) {
        if !self.can_rewind_message(index) {
            return;
        }
        let active = self.vm.active.as_ref().expect("rewindable conversation");
        let (Some(session), Some(branch)) = (
            SessionId::parse(&active.session_id),
            BranchId::parse(&active.branch_id),
        ) else {
            return;
        };
        self.recall_generation = self.recall_generation.wrapping_add(1);
        let request = RecallRequest {
            session,
            branch,
            head: active.head.clone(),
            edit: edit.then(|| active.entries[index].text.to_string()),
            generation: self.recall_generation,
        };
        if let Some(target) = previous_event(&active.entries, index).map(str::to_owned) {
            self.finish_recall(request, target, cx);
        } else if let Some(before) = active.older_before.clone() {
            self.resolve_recall_page(request, before, cx);
        }
    }

    /// A resident-window boundary is not a branch boundary. Resolve the
    /// preceding committed event without prepending or disturbing the viewport.
    fn resolve_recall_page(
        &mut self,
        request: RecallRequest,
        before: String,
        cx: &mut Context<Self>,
    ) {
        let reply = self.bridge.request(BridgeCommand::LoadOlder {
            session: request.session.clone(),
            branch: request.branch.clone(),
            expected_head: super::parse_head(&request.head),
            before: before.clone(),
        });
        cx.spawn(async move |this, cx| {
            let reply = reply.await;
            let _ = this.update(cx, |workspace, cx| {
                let current = workspace.recall_generation == request.generation
                    && !workspace.vm.sending
                    && workspace.vm.active.as_ref().is_some_and(|active| {
                        active.session_id == request.session.as_str()
                            && active.branch_id == request.branch.as_str()
                            && active.head == request.head
                    });
                if !current {
                    return;
                }
                match reply {
                    BridgeReply::Older(Ok(page)) => {
                        if let Some(target) = previous_event(&page.entries, page.entries.len()) {
                            workspace.finish_recall(request, target.to_owned(), cx);
                        } else if let Some(older) = page.older.filter(|older| older != &before) {
                            workspace.resolve_recall_page(request, older, cx);
                        } else {
                            workspace.push_toast(
                                "The first message has no earlier event to recall to.",
                                super::ToastKind::Info,
                                cx,
                            );
                        }
                    }
                    BridgeReply::Older(Err(message)) => {
                        workspace.apply_action(DesktopAction::Failed(message), cx)
                    }
                    _ => {}
                }
            });
        })
        .detach();
    }

    fn finish_recall(&mut self, request: RecallRequest, to_event: String, cx: &mut Context<Self>) {
        self.dispatch(
            BridgeCommand::RecallMessage {
                session: request.session,
                branch: request.branch,
                expected_head: super::parse_head(&request.head),
                to_event,
                edit: request.edit,
            },
            cx,
        );
    }

    /// The cancel button is first so dismissing the dialog never deletes data.
    pub(crate) fn on_delete_session(
        &mut self,
        session_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = window.prompt(
            gpui_kit::PromptLevel::Warning,
            crate::i18n::t("Delete this session?", "删除此会话？"),
            Some(crate::i18n::t(
                "Its conversation history will be permanently deleted. Workspace files are not affected. This cannot be undone.",
                "会话历史将被永久删除，无法撤销。工作区文件不会受到影响。",
            )),
            &[crate::i18n::t("Cancel", "取消"), crate::i18n::t("Delete session", "删除会话")], cx,
        );
        let session_id = session_id.to_owned();
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(1) {
                return;
            }
            let _ = this.update(cx, |workspace, cx| {
                workspace.delete_confirmed(session_id, cx)
            });
        })
        .detach();
    }

    fn delete_confirmed(&mut self, session_id: String, cx: &mut Context<Self>) {
        let request = self.bridge.request(BridgeCommand::DeleteSession {
            session_id: session_id.clone(),
        });
        cx.spawn(async move |this, cx| {
            let reply = request.await;
            let _ = this.update(cx, |workspace, cx| {
                match reply {
                    BridgeReply::SessionDeleted(Ok(())) => {
                        if workspace
                            .vm
                            .active
                            .as_ref()
                            .is_some_and(|active| active.session_id == session_id)
                        {
                            workspace.apply_action(DesktopAction::SessionDeleted, cx);
                        }
                        // Forget bindings only after the core confirms deletion;
                        // failure must leave both the transcript and routing intact.
                        workspace
                            .apply_action(DesktopAction::SessionBindingsForgotten(session_id), cx);
                        workspace.dispatch(BridgeCommand::ListSessions, cx);
                    }
                    BridgeReply::SessionDeleted(Err(message)) => {
                        workspace.apply_action(DesktopAction::Failed(message), cx)
                    }
                    _ => {}
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kind: EntryKind) -> ConversationEntry {
        ConversationEntry {
            event_id: id.to_owned(),
            kind,
            text: "text".into(),
            call_id: None,
            thinking: String::new(),
        }
    }

    #[test]
    fn rewind_skips_cancelled_live_tool_placeholders() {
        let entries = [
            entry(
                "evt1-00000000000000000000000000000001",
                EntryKind::AssistantMessage,
            ),
            entry("call-live", EntryKind::ToolCall),
            entry(
                "evt1-00000000000000000000000000000002",
                EntryKind::UserMessage,
            ),
        ];
        assert_eq!(
            previous_event(&entries, 2),
            Some(entries[0].event_id.as_str())
        );
        assert_eq!(previous_event(&entries, 0), None);
        assert_eq!(previous_event(&entries, 8), None);
    }

    #[test]
    fn first_resident_user_message_can_resolve_older_history() {
        let mut active = ActiveConversation {
            session_id: "session".to_owned(),
            branch_id: "branch".to_owned(),
            head: "empty".to_owned(),
            entries: vec![entry(
                "evt1-00000000000000000000000000000002",
                EntryKind::UserMessage,
            )],
            older_before: Some("evt1-00000000000000000000000000000002".to_owned()),
            streaming: None,
            context_used: 0,
            context_cache: 0,
        };
        assert!(can_rewind(&active, 0));
        active.older_before = None;
        assert!(!can_rewind(&active, 0));
        assert!(!can_rewind(&active, 1));
    }
}
