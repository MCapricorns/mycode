//! Open replies must not revive work whose terminal event already arrived.
use crate::app::{
    ActiveConversation, BranchId, BridgeEvent, BridgeReply, ConversationEntry, EntryKind, SessionId,
};

use super::{ReplyScope, conversation, scratch_driver};
use crate::frontend::driver::{Effect, FrontendDriver};

fn open_scope(driver: &FrontendDriver) -> ReplyScope {
    ReplyScope::Open {
        session: driver.pending_open.clone().unwrap(),
        generation: driver.open_generation,
    }
}

fn deliver_snapshot(
    driver: &mut FrontendDriver,
    scope: ReplyScope,
    snapshot: ActiveConversation,
    out: &mut Vec<Effect>,
) -> bool {
    if !scope.applies(
        &driver.vm,
        driver.pending_open.as_deref(),
        driver.open_generation,
    ) {
        return false;
    }
    driver.on_reply(scope, BridgeReply::Conversation(Ok(snapshot)), out);
    true
}

fn final_entry() -> ConversationEntry {
    ConversationEntry {
        event_id: "evt1-00000000000000000000000000000002".into(),
        kind: EntryKind::AssistantMessage,
        agent_states: None,
        text: "committed result".into(),
        call_id: None,
        thinking: String::new(),
    }
}

#[test]
fn a_terminal_before_the_open_reply_discards_the_busy_snapshot() {
    for offscreen in [false, true] {
        for terminal in ["done", "failed", "compact"] {
            let (mut driver, root) = scratch_driver("terminal-before-open");
            let session = SessionId::generate().unwrap().to_string();
            let branch = BranchId::generate().unwrap().to_string();
            let mut stale = conversation(&session, &branch);
            stale.running = terminal != "compact";
            stale.compacting = terminal == "compact";
            driver.vm.active = Some(if offscreen {
                conversation("other-session", "other-branch")
            } else {
                stale.clone()
            });
            driver.vm.sending = !offscreen && stale.running;
            driver.vm.compacting = !offscreen && stale.compacting;
            driver.pending_open = Some(session.clone());
            let old_scope = open_scope(&driver);
            let entry = final_entry();
            let head = entry.event_id.clone();
            let event = match terminal {
                "done" => BridgeEvent::ChatDone {
                    session_id: session.clone(),
                    head: head.clone(),
                    entry: entry.clone(),
                },
                "failed" => BridgeEvent::ChatFailed {
                    session_id: session.clone(),
                    message: "provider failed".into(),
                    head: head.clone(),
                },
                _ => BridgeEvent::CompactFinished {
                    session_id: session.clone(),
                    message: "compacted".into(),
                    ok: true,
                    head: head.clone(),
                },
            };
            let mut out = Vec::new();
            driver.on_event(event, &mut out);
            assert_eq!(driver.pending_open.as_deref(), Some(session.as_str()));
            assert!(!deliver_snapshot(&mut driver, old_scope, stale, &mut out));

            let fresh_scope = open_scope(&driver);
            let mut fresh = conversation(&session, &branch);
            fresh.head = head.clone();
            fresh.entries.push(entry);
            assert!(deliver_snapshot(&mut driver, fresh_scope, fresh, &mut out));
            assert!(!driver.vm.sending);
            assert!(!driver.vm.compacting);
            assert_eq!(driver.vm.active.as_ref().unwrap().head, head);
            assert_eq!(driver.vm.active.as_ref().unwrap().entries.len(), 1);
            assert!(driver.pending_open.is_none());
            assert!(driver.resync_on_turn_end.is_none());
            driver.shutdown();
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

#[test]
fn returning_to_the_same_chat_does_not_accept_its_earlier_open_reply() {
    let (mut driver, root) = scratch_driver("open-generation");
    let session = SessionId::generate().unwrap().to_string();
    let other = SessionId::generate().unwrap().to_string();
    let branch = BranchId::generate().unwrap().to_string();
    driver.request_open_session(&session);
    let stale_scope = open_scope(&driver);
    driver.request_open_session(&other);
    driver.request_open_session(&session);
    let current_scope = open_scope(&driver);
    let mut out = Vec::new();
    assert!(!deliver_snapshot(
        &mut driver,
        stale_scope,
        conversation(&session, &branch),
        &mut out,
    ));
    let mut current = conversation(&session, &branch);
    current.entries.push(final_entry());
    assert!(deliver_snapshot(
        &mut driver,
        current_scope,
        current,
        &mut out
    ));
    assert_eq!(driver.vm.active.as_ref().unwrap().entries.len(), 1);
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn another_chats_terminal_does_not_invalidate_the_requested_snapshot() {
    let (mut driver, root) = scratch_driver("unrelated-terminal");
    let session = SessionId::generate().unwrap().to_string();
    driver.pending_open = Some(session.clone());
    let scope = open_scope(&driver);
    let mut out = Vec::new();
    driver.on_event(
        BridgeEvent::ChatFailed {
            session_id: "other-session".into(),
            message: "failed".into(),
            head: String::new(),
        },
        &mut out,
    );
    assert!(deliver_snapshot(
        &mut driver,
        scope,
        conversation(&session, "branch"),
        &mut out,
    ));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleting_a_chat_invalidates_only_its_own_pending_open() {
    for (deleted_target, succeeded) in [(false, true), (true, true), (true, false)] {
        let (mut driver, root) = scratch_driver("deleted-before-open");
        let session = SessionId::generate().unwrap().to_string();
        driver.vm.active = Some(conversation("visible-session", "branch"));
        driver.pending_open = Some(session.clone());
        let scope = open_scope(&driver);
        let deleted = if deleted_target {
            &session
        } else {
            "another-session"
        };
        let mut out = Vec::new();
        driver.on_reply(
            ReplyScope::DeletedSession(deleted.to_owned()),
            BridgeReply::SessionDeleted(if succeeded {
                Ok(())
            } else {
                Err("delete failed".into())
            }),
            &mut out,
        );
        assert_eq!(
            deliver_snapshot(
                &mut driver,
                scope,
                conversation(&session, "branch"),
                &mut out
            ),
            !deleted_target || !succeeded,
        );
        assert_eq!(
            driver.open_session_id().as_deref(),
            Some(if deleted_target && succeeded {
                "visible-session"
            } else {
                &session
            }),
        );
        driver.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }
}
