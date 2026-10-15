use super::{ReplyScope, failed_start};
use crate::view_model::{DesktopAction, WorkspaceState, reduce};
use mycode_app::ActiveConversation;

fn conversation(session: &str, branch: &str) -> ActiveConversation {
    ActiveConversation {
        session_id: session.to_owned(),
        branch_id: branch.to_owned(),
        head: "empty".to_owned(),
        entries: Vec::new(),
        older_before: None,
        streaming: None,
        context_used: 0,
        context_cache: 0,
    }
}

#[test]
fn rejected_start_releases_sending_and_removes_the_waiting_bubble() {
    let mut state = WorkspaceState {
        active: Some(conversation("a", "branch")),
        ..WorkspaceState::default()
    };
    reduce(&mut state, DesktopAction::TurnArmed);
    assert!(state.sending);
    assert!(state.active.as_ref().unwrap().streaming.is_some());
    reduce(&mut state, failed_start("start refused".to_owned()));
    assert!(!state.sending);
    assert!(state.active.as_ref().unwrap().streaming.is_none());
    assert_eq!(state.error.as_deref(), Some("start refused"));
    assert!(crate::view_model::open_session_is_empty(&state));
}

#[test]
fn late_turn_and_recall_replies_cannot_change_another_session_or_branch() {
    let mut state = WorkspaceState {
        active: Some(conversation("a", "branch")),
        ..WorkspaceState::default()
    };
    let scope = ReplyScope::Session {
        session: "a".to_owned(),
        branch: Some("branch".to_owned()),
    };
    assert!(scope.applies(&state, None));
    state.active = Some(conversation("b", "branch"));
    assert!(!scope.applies(&state, None));
    state.active = Some(conversation("a", "rewound"));
    assert!(!scope.applies(&state, None));
}

#[test]
fn superseded_open_and_file_search_replies_are_ignored() {
    let mut state = WorkspaceState {
        active: Some(conversation("a", "branch")),
        ..WorkspaceState::default()
    };
    assert!(!ReplyScope::Open("old".to_owned()).applies(&state, Some("new")));
    assert!(!ReplyScope::Open("old".to_owned()).applies(&state, None));
    assert!(ReplyScope::Open("new".to_owned()).applies(&state, Some("new")));
    reduce(
        &mut state,
        DesktopAction::ComposerChanged("@second".to_owned()),
    );
    assert!(
        !ReplyScope::Mention {
            session: "a".to_owned(),
            query: "first".to_owned()
        }
        .applies(&state, None)
    );
    assert!(
        ReplyScope::Mention {
            session: "a".to_owned(),
            query: "second".to_owned()
        }
        .applies(&state, None)
    );
}
