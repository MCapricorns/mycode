#[path = "workspace/clamp.rs"]
mod candidate;

#[test]
fn percentage_bounds_and_unrelated_behavior() {
    for value in [0, 1, 42, 99, 100, 101, 250, u32::MAX] {
        let expected = if value > 100 { 100 } else { value };
        assert_eq!(candidate::clamp_percent(value), expected, "value={value}");
    }
    assert_eq!(candidate::percent_label(), "percent");
}
