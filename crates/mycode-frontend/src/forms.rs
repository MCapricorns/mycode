//! Form parsing and validation shared by every frontend's settings UI.
//!
//! These helpers turn raw text-field contents into config rows — or refuse
//! them — so no settings form drifts into accepting different input.

/// The URL the custom proxy mode starts from: Clash's mixed port.
pub const DEFAULT_PROXY_URL: &str = "http://127.0.0.1:7890";

/// Trims an API key candidate for UI button enablement and save dispatch.
///
/// Saving an empty or whitespace-only key field is a no-op so the replace-key
/// flow cannot delete a stored credential.
pub fn normalized_key_input(raw: &str) -> Option<String> {
    let key = mycode_app::normalize_api_key(raw);
    (!key.is_empty() && !key.eq_ignore_ascii_case("bearer")).then_some(key)
}

/// Merges picked choices and free text into the answers one ask card sends.
/// One slot per question preserves arbitrary text, including shell pipes,
/// and does not let an answer to one question occupy another's slot.
/// `Err(index)` names the first required question still unanswered.
pub fn collect_ask_answers(
    rows: &[mycode_app::AskPrompt],
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
    use super::{collect_ask_answers, normalized_key_input};

    #[test]
    fn blank_key_inputs_are_refused_and_bearer_prefixes_are_stripped() {
        assert_eq!(normalized_key_input(""), None);
        assert_eq!(normalized_key_input("   \t\n  "), None);
        assert_eq!(normalized_key_input("Bearer   "), None);
        assert_eq!(
            normalized_key_input("  sk-test-123  "),
            Some("sk-test-123".to_owned())
        );
        assert_eq!(
            normalized_key_input("Bearer sk-test-123"),
            Some("sk-test-123".to_owned())
        );
    }

    #[test]
    fn free_text_preserves_pipes_and_question_identity() {
        let question = |optional| mycode_app::AskPrompt {
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
