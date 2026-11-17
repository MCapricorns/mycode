use super::{conversation, scratch_driver};
use crate::app::{AskPrompt, BridgeEvent};
use crate::frontend::view_model::FrontendAction;

fn questions() -> Vec<AskPrompt> {
    ["First question", "Second question"]
        .into_iter()
        .map(|question| AskPrompt {
            question: question.to_owned(),
            choices: vec!["Yes".to_owned(), "No".to_owned()],
            optional: false,
            multiple: false,
        })
        .collect()
}

#[test]
fn a_background_ask_and_its_draft_survive_navigation_and_snapshot_refresh() {
    let (mut driver, root) = scratch_driver("park-ask");
    let mut out = Vec::new();
    driver.act(
        FrontendAction::ConversationOpened(conversation("b", "branch")),
        &mut out,
    );
    driver.on_event(
        BridgeEvent::AskRequested {
            session_id: "a".to_owned(),
            ask_id: 12,
            questions: questions(),
        },
        &mut out,
    );
    assert!(driver.vm.pending_ask.is_none());
    let mut running = conversation("a", "branch");
    running.running = true;
    driver.act(
        FrontendAction::ConversationOpened(running.clone()),
        &mut out,
    );
    assert_eq!(driver.vm.pending_ask, Some(questions()));
    driver.pick_ask_choice(0, "Yes".to_owned(), false, &mut out);
    driver.ask_next(&mut out);
    driver.ask_text_typed(1, "Keep this draft".to_owned(), &mut out);
    for navigation in [
        FrontendAction::ConversationParked,
        FrontendAction::ConversationOpened(conversation("b", "branch")),
        FrontendAction::ConversationOpened(running.clone()),
    ] {
        driver.act(navigation, &mut out);
        driver.act(
            FrontendAction::ConversationOpened(running.clone()),
            &mut out,
        );
        assert_eq!(driver.vm.ask_answers[0], "Yes");
        assert_eq!(driver.ask_texts[1], "Keep this draft");
        assert_eq!(driver.ask_index, 1);
    }
    driver.on_event(
        BridgeEvent::AskRequested {
            session_id: "a".to_owned(),
            ask_id: 13,
            questions: questions(),
        },
        &mut out,
    );
    driver.on_event(
        BridgeEvent::AskResolved {
            session_id: "a".to_owned(),
            ask_id: 12,
        },
        &mut out,
    );
    assert!(
        driver.vm.pending_ask.is_some(),
        "old completion cannot close the new card"
    );
    assert!(driver.ask_texts.is_empty());
    driver.on_event(
        BridgeEvent::AskResolved {
            session_id: "a".to_owned(),
            ask_id: 13,
        },
        &mut out,
    );
    assert!(driver.vm.pending_ask.is_none());
    assert!(driver.pending_asks.is_empty());
    drop(driver);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_background_cancel_does_not_restore_a_dead_question() {
    let (mut driver, root) = scratch_driver("cancel-ask");
    let mut out = Vec::new();
    driver.on_event(
        BridgeEvent::AskRequested {
            session_id: "a".to_owned(),
            ask_id: 20,
            questions: questions(),
        },
        &mut out,
    );
    driver.on_event(
        BridgeEvent::AskResolved {
            session_id: "a".to_owned(),
            ask_id: 20,
        },
        &mut out,
    );
    driver.act(
        FrontendAction::ConversationOpened(conversation("a", "branch")),
        &mut out,
    );
    assert!(driver.vm.pending_ask.is_none());
    assert!(driver.pending_asks.is_empty());
    drop(driver);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn callbacks_from_a_replaced_or_hidden_card_cannot_change_the_current_ask() {
    let (mut driver, root) = scratch_driver("ask-callback-identity");
    let mut out = Vec::new();
    let mut running = conversation("a", "branch");
    running.running = true;
    driver.act(
        FrontendAction::ConversationOpened(running.clone()),
        &mut out,
    );
    driver.on_event(
        BridgeEvent::AskRequested {
            session_id: "a".to_owned(),
            ask_id: 30,
            questions: questions(),
        },
        &mut out,
    );
    let previous = driver.open_ask_identity().unwrap();
    driver.on_event(
        BridgeEvent::AskRequested {
            session_id: "a".to_owned(),
            ask_id: 31,
            questions: questions(),
        },
        &mut out,
    );
    let current = driver.open_ask_identity().unwrap();
    driver.with_open_ask(&previous, &mut out, |driver, out| {
        driver.ask_text_typed(0, "stale input".to_owned(), out);
        driver.pick_ask_choice(0, "Yes".to_owned(), true, out);
        driver.dismiss_ask(out);
    });
    assert!(driver.ask_texts.is_empty());
    assert_eq!(driver.vm.ask_answers, ["", ""]);
    assert_eq!(driver.open_ask_identity(), Some(current.clone()));

    driver.act(FrontendAction::ConversationParked, &mut out);
    driver.with_open_ask(&current, &mut out, |driver, out| {
        driver.ask_text_typed(0, "hidden card input".to_owned(), out);
    });
    driver.act(FrontendAction::ConversationOpened(running), &mut out);
    assert!(driver.ask_texts.is_empty());
    driver.with_open_ask(&current, &mut out, |driver, out| {
        driver.ask_text_typed(0, "current input".to_owned(), out);
    });
    assert_eq!(driver.ask_texts, ["current input"]);
    drop(driver);
    let _ = std::fs::remove_dir_all(root);
}
