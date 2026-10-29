//! Credentials never belong in retained delegation artifacts.
use regex::Regex;
use std::sync::OnceLock;
pub(super) fn redact_text(text: &str) -> String {
    static SECRET: OnceLock<Regex> = OnceLock::new();
    let pattern = SECRET.get_or_init(|| Regex::new(r#"(?im)(\b(?:(?:[a-z][a-z0-9_-]*[_-])?(?:api[_-]?key|token|password|secret)|authorization)\s*[=:]\s*["']?)([^\s"'\r\n,;}]+)|\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9_]{16,})\b|(?i:Bearer\s+)[A-Za-z0-9._~+/-]+=*"#).expect("constant redaction regex"));
    pattern
        .replace_all(text, |caps: &regex::Captures<'_>| match caps.get(1) {
            Some(prefix) => format!("{}[REDACTED]", prefix.as_str()),
            None => "[REDACTED]".into(),
        })
        .into_owned()
}
