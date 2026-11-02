//! Form parsing and validation shared by every frontend's settings UI.
//!
//! These helpers turn raw text-field contents into config rows — or refuse
//! them — so no settings form drifts into accepting different input.

/// The URL the custom proxy mode starts from: Clash's mixed port.
pub const DEFAULT_PROXY_URL: &str = "http://127.0.0.1:7890";

/// Merges picked choices and free text into the answers one ask card sends.
/// One slot per question preserves arbitrary text, including shell pipes,
/// and does not let an answer to one question occupy another's slot.
/// `Err(index)` names the first required question still unanswered.
pub fn collect_ask_answers(
    rows: &[crate::app::AskPrompt],
    picks: &[String],
    free_text: &[String],
) -> Result<Vec<String>, usize> {
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let text = free_text
                .get(index)
                .map(|text| text.trim())
                .unwrap_or_default();
            let answer = if text.is_empty() {
                picks.get(index).map(|text| text.trim()).unwrap_or_default()
            } else {
                text
            };
            if answer.is_empty() && !row.optional {
                Err(index)
            } else {
                Ok(answer.to_owned())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::collect_ask_answers;

    #[test]
    fn free_text_preserves_pipes_and_question_identity() {
        let question = |optional| crate::app::AskPrompt {
            question: "Question?".to_owned(),
            choices: vec![],
            optional,
            multiple: false,
        };
        let questions = [question(false), question(false)];
        assert_eq!(
            collect_ask_answers(
                &questions,
                &["yes".to_owned()],
                &[String::new(), "a | b".to_owned()]
            ),
            Ok(vec!["yes".to_owned(), "a | b".to_owned()])
        );
        assert_eq!(
            collect_ask_answers(&questions[..1], &[], &["a|b|c".to_owned()]),
            Ok(vec!["a|b|c".to_owned()])
        );
        assert_eq!(
            collect_ask_answers(&questions, &[], &["only first".to_owned()]),
            Err(1)
        );
        assert_eq!(
            collect_ask_answers(&[question(true)], &[], &[]),
            Ok(vec![String::new()])
        );
    }
}
