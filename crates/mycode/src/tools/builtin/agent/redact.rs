//! Credentials never belong in retained delegation artifacts.
use regex::Regex;
use std::sync::OnceLock;
pub(super) fn redact_text(text: &str) -> String {
    static SECRET: OnceLock<Regex> = OnceLock::new();
    let pattern = SECRET.get_or_init(|| Regex::new(concat!(
        r#"(?im)(?P<credential>\b(?:[a-z][a-z0-9_-]*[_-])?(?:api[_-]?key|token|password|secret)["']?\s*[=:]\s*["']?)[^\s"'\r\n,;}]+"#,
        r#"|(?P<authorization>\bauthorization["']?\s*[=:]\s*["']?)(?:(?:Bearer|Basic)\s+)?[^\s"'\r\n,;}]+"#,
        r#"|\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9_]{16,})\b|Bearer\s+[A-Za-z0-9._~+/-]+=*"#,
    )).expect("constant redaction regex"));
    pattern
        .replace_all(text, |caps: &regex::Captures<'_>| {
            match caps
                .name("credential")
                .or_else(|| caps.name("authorization"))
            {
                Some(prefix) => format!("{}[REDACTED]", prefix.as_str()),
                None => "[REDACTED]".into(),
            }
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn authorization_headers_remove_the_token_after_the_scheme() {
        for text in [
            "Authorization: Bearer private-credential",
            "authorization = bearer private-credential",
            "Authorization: Basic private-credential",
            r#"{"Authorization": "Bearer private-credential"}"#,
        ] {
            let redacted = super::redact_text(text);
            assert!(!redacted.contains("private-credential"), "{redacted}");
            assert!(redacted.contains("[REDACTED]"), "{redacted}");
        }
    }

    #[test]
    fn quoted_credential_keys_are_redacted() {
        for text in [
            r#"{"api_key": "private-credential"}"#,
            "'password': 'private-credential'",
            "access_token=private-credential",
        ] {
            let redacted = super::redact_text(text);
            assert!(!redacted.contains("private-credential"), "{redacted}");
        }
    }

    #[test]
    fn ordinary_password_values_do_not_consume_following_text() {
        for value in ["Basic", "Bearer"] {
            assert_eq!(
                super::redact_text(&format!("password={value} ordinary prose")),
                "password=[REDACTED] ordinary prose"
            );
        }
    }
}
