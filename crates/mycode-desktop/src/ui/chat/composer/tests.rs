use super::{COMPOSER_PLACEHOLDER, QUEUE_PLACEHOLDER, thinking_chip_label, used_percent};

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
fn composer_placeholder_asks_the_agent_without_a_product_name() {
    assert_eq!(COMPOSER_PLACEHOLDER.0, "Ask the agent");
    assert_eq!(COMPOSER_PLACEHOLDER.1, "向代理提问");
    assert!(!COMPOSER_PLACEHOLDER.0.contains("MYCode"));
    assert!(!COMPOSER_PLACEHOLDER.1.contains("MYCode"));
    assert!(!COMPOSER_PLACEHOLDER.0.to_ascii_lowercase().contains("chat"));
    assert!(!COMPOSER_PLACEHOLDER.1.contains("对话"));
    assert_eq!(
        QUEUE_PLACEHOLDER.0,
        "Queue; Ctrl+Enter interrupts; Ctrl+G steers"
    );
    assert_eq!(
        QUEUE_PLACEHOLDER.1,
        "回车排队 · Ctrl+Enter 打断发送 · Ctrl+G 引导"
    );
}

/// 200k of a 1M window is 20% used, so the ring must fill about a fifth.
#[test]
fn cache_ring_fills_with_the_used_share() {
    assert_eq!(used_percent(80), 20);
    assert_eq!(used_percent(100), 0);
    assert_eq!(used_percent(0), 100);
    assert_eq!(used_percent(150), 0);
}
