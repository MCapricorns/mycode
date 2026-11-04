use super::{ReplyScope, conversation, scratch_driver};
use crate::app::{BranchId, BridgeReply, SessionId, SessionSummary};

fn summary() -> SessionSummary {
    SessionSummary {
        session_id: SessionId::generate().unwrap().to_string(),
        root_branch_id: BranchId::generate().unwrap().to_string(),
        title: "New chat".into(),
        updated_at_ms: None,
        event_count: 0,
        active: false,
        corrupt: false,
    }
}

#[test]
fn overlapping_creates_keep_their_own_project_and_only_the_newest_opens() {
    for newest_first in [false, true] {
        let (mut driver, root) = scratch_driver("overlapping-creates");
        let default_chat = summary();
        let project_chat = summary();
        let project = root.to_string_lossy().into_owned();
        let mut out = Vec::new();
        driver.pending_create = Some(2);
        driver.vm.project_dir = Some(project.clone());
        let replies = if newest_first {
            vec![
                (2, Some(project.clone()), project_chat.clone()),
                (1, None, default_chat.clone()),
            ]
        } else {
            vec![
                (1, None, default_chat.clone()),
                (2, Some(project.clone()), project_chat.clone()),
            ]
        };
        for (request, project, summary) in replies {
            driver.on_reply(
                ReplyScope::Create {
                    request,
                    project,
                    welcome: None,
                },
                BridgeReply::Created(Ok(summary)),
                &mut out,
            );
        }
        assert_eq!(
            driver.open_session_id().as_deref(),
            Some(project_chat.session_id.as_str())
        );
        assert_eq!(
            driver.pending_open.as_deref(),
            Some(project_chat.session_id.as_str())
        );
        assert_eq!(
            driver.vm.session_projects,
            [(project_chat.session_id, project)]
        );
        assert!(
            !driver
                .vm
                .session_projects
                .iter()
                .any(|(session, _)| session == &default_chat.session_id)
        );
        driver.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn a_create_reply_cannot_replace_an_explicitly_opened_conversation() {
    let (mut driver, root) = scratch_driver("create-after-open");
    let created = summary();
    let selected = summary();
    let project = root.to_string_lossy().into_owned();
    let mut out = Vec::new();
    driver.pending_create = Some(1);
    driver.request_open_session(&selected.session_id);
    driver.on_reply(
        ReplyScope::Open(selected.session_id.clone()),
        BridgeReply::Conversation(Ok(conversation(
            &selected.session_id,
            &selected.root_branch_id,
        ))),
        &mut out,
    );
    driver.vm.composer_draft = "keep this draft".into();
    driver.on_reply(
        ReplyScope::Create {
            request: 1,
            project: Some(project.clone()),
            welcome: None,
        },
        BridgeReply::Created(Ok(created.clone())),
        &mut out,
    );
    assert_eq!(
        driver.open_session_id().as_deref(),
        Some(selected.session_id.as_str())
    );
    assert_eq!(driver.vm.composer_draft, "keep this draft");
    assert!(driver.vm.project_dir.is_none());
    assert_eq!(driver.vm.session_projects, [(created.session_id, project)]);
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}
