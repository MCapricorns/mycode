#[test]
fn collapsed_thinking_headline_follows_the_newest_line() {
    let trace = "reasoning step 0 weighs the next edit\nsecond line\n\n";
    assert_eq!(
        super::thinking_headline("思考", false, trace),
        "思考 · second line ▸"
    );
    assert_eq!(
        super::thinking_headline("Thinking", true, trace),
        "Thinking ▾"
    );
    assert_eq!(super::thinking_headline("思考", false, "   "), "思考 ▸");
    let long = "a streaming line that keeps growing with the newest words at the end";
    let headline = super::thinking_headline("思考", false, long);
    assert!(
        headline.ends_with("newest words at the end ▸"),
        "{headline}"
    );
    assert!(headline.contains('\u{2026}'), "{headline}");
}
