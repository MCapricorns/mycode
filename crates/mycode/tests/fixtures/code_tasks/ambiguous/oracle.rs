#[path = "workspace/ranges.rs"]
mod candidate;

#[test]
fn previous_decrements_without_changing_next() {
    for value in [-50, -1, 0, 1, 50] {
        assert_eq!(candidate::previous(value), value - 1);
        assert_eq!(candidate::next(value), value + 1);
    }
}
