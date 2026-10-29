use super::{conversation, scratch_driver};
use crate::driver::Effect;
use crate::view_model::FrontendAction;

#[test]
fn follow_intent_survives_updates_and_resets_only_for_a_new_conversation() {
    let (mut driver, root) = scratch_driver("scroll-follow");
    driver.vm.active = Some(conversation("session-a", "branch-a"));
    let mut effects = Vec::new();
    driver.act(FrontendAction::TurnArmed, &mut effects);
    assert!(driver.conversation_at_tail());

    effects.clear();
    driver.act(
        FrontendAction::ComposerChanged("a steer draft".into()),
        &mut effects,
    );
    driver.act(FrontendAction::ChatDelta("new output".into()), &mut effects);
    assert!(driver.conversation_at_tail());
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::ScrollToTail))
    );

    driver.set_conversation_at_tail(false);
    effects.clear();
    driver.act(
        FrontendAction::ChatDelta("while reading history".into()),
        &mut effects,
    );
    driver.act(
        FrontendAction::ConversationOpened(conversation("session-a", "branch-a")),
        &mut effects,
    );
    assert!(
        !driver.conversation_at_tail(),
        "refreshing the same branch preserves history position"
    );
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::ScrollToTail))
    );

    effects.clear();
    driver.act(
        FrontendAction::ConversationOpened(conversation("session-b", "branch-b")),
        &mut effects,
    );
    assert!(
        driver.conversation_at_tail(),
        "another conversation starts at its tail"
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::ScrollToTail))
    );

    driver.set_conversation_at_tail(false);
    driver.act(
        FrontendAction::ConversationOpened(conversation("session-b", "rewound")),
        &mut effects,
    );
    assert!(
        driver.conversation_at_tail(),
        "recall opens the new branch at its tail"
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}
