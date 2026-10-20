use super::{AssistantPart, assistant_parts};

#[test]
fn assistant_parts_split_error_lines_out_of_prose() {
    let text = "before\n\n[error] the response was interrupted: transport failed";
    assert_eq!(
        assistant_parts(text),
        vec![
            AssistantPart::Prose("before".into()),
            AssistantPart::Error(
                "[error] the response was interrupted: transport failed".into()
            ),
        ]
    );
}

#[test]
fn assistant_parts_merge_consecutive_errors_and_keep_plain_prose() {
    let errors = "[error] first\n[error] second";
    assert_eq!(
        assistant_parts(errors),
        vec![AssistantPart::Error(
            "[error] first\n[error] second".into()
        )]
    );
    assert_eq!(
        assistant_parts("just an answer\n\n"),
        vec![AssistantPart::Prose("just an answer".into())]
    );
}

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
