use super::{thinking_chip_label, used_percent};

#[test]
fn thinking_chip_labels_fit_the_chip() {
    for level in [
        "default", "off", "on", "minimal", "low", "medium", "high", "xhigh", "max",
    ] {
        let label = thinking_chip_label(level);
        assert!(label.chars().count() <= 12, "{level}: {label}");
    }
}

#[test]
fn busy_enter_opens_choice_only_for_a_text_draft_on_a_busy_turn() {
    use super::busy_enter_opens_choice;
    assert!(busy_enter_opens_choice(true, "steer me", false));
    assert!(!busy_enter_opens_choice(false, "hello", false));
    assert!(!busy_enter_opens_choice(true, "  ", false));
    assert!(!busy_enter_opens_choice(true, "", false));
    // Attachments cannot steer: their submit keeps the waiting path.
    assert!(!busy_enter_opens_choice(true, "with files", true));
}

#[test]
fn context_progress_uses_the_used_share() {
    assert_eq!(used_percent(100), 0);
    assert_eq!(used_percent(99), 1);
    assert_eq!(used_percent(80), 20);
    assert_eq!(used_percent(50), 50);
    assert_eq!(used_percent(0), 100);
    assert_eq!(used_percent(150), 0);
}
