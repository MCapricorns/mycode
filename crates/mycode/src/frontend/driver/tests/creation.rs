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
        ReplyScope::Open {
            session: selected.session_id.clone(),
            generation: driver.open_generation,
        },
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

#[test]
fn first_prompt_sends_its_attachments_and_preserves_edits_made_while_opening() {
    let (mut driver, root) = scratch_driver("welcome-first-prompt");
    let created = summary();
    let mut out = Vec::new();
    let text = "Review my editor".to_owned();
    driver.pending_create = Some(1);
    driver.welcome_create = Some(1);
    driver.act(
        crate::frontend::view_model::FrontendAction::WelcomeSendHeld(text.clone()),
        &mut out,
    );
    driver.vm.composer_draft = "A follow-up draft".into();

    driver.on_reply(
        ReplyScope::Create {
            request: 1,
            project: None,
            welcome: Some(super::super::sends::FailedSendDraft {
                text: text.clone(),
                attachments: super::code_attachment(),
            }),
        },
        BridgeReply::Created(Ok(created.clone())),
        &mut out,
    );
    assert_eq!(
        driver.vm.pending_welcome_send.as_deref(),
        Some(text.as_str())
    );
    driver.on_reply(
        ReplyScope::Open {
            session: created.session_id.clone(),
            generation: driver.open_generation,
        },
        BridgeReply::Conversation(Ok(conversation(
            &created.session_id,
            &created.root_branch_id,
        ))),
        &mut out,
    );
    assert!(driver.submitted_turns.contains_key(&created.session_id));
    assert!(!driver.pending_send_attachments[&created.session_id].is_empty());
    assert!(driver.vm.pending_welcome_send.is_none());
    assert_eq!(driver.vm.composer_draft, "A follow-up draft");
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}
