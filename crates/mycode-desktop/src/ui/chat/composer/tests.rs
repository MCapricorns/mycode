use super::{COMPOSER_PLACEHOLDER, QUEUE_PLACEHOLDER, cache_ring_svg, thinking_chip_label};

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
    assert_eq!(QUEUE_PLACEHOLDER.0, "Queue; Ctrl+Enter interrupts");
    assert_eq!(QUEUE_PLACEHOLDER.1, "回车排队 · Ctrl+Enter 打断发送");
}

#[test]
fn cache_ring_svg_preserves_track_stroke_opacity() {
    let track = gpui_kit::hsla(0.6, 0.2, 0.7, 0.35);
    let ink = gpui_kit::hsla(0.6, 0.8, 0.6, 1.0);
    let svg = cache_ring_svg(50, track, ink);
    assert!(
        svg.contains(r#"stroke-opacity="0.35""#),
        "cache ring track must emit stroke-opacity for translucent Hsla: {svg}"
    );
    assert!(
        svg.contains(r#"stroke-opacity="1.00""#),
        "cache ring ink must emit full stroke-opacity: {svg}"
    );
}
