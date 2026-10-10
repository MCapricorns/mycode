//! Recovers older authority JSON that is the same document with non-strict
//! punctuation. Callers still validate values and publish the canonical form.
//!
//! Recovery never prints or retains input snippets. A trailing comma outside
//! a string is the only syntax this module repairs.

use serde::de::DeserializeOwned;

use crate::{ConfigError, ConfigErrorKind};

/// A decoded document plus whether the bytes needed syntax recovery.
pub(crate) struct Decoded<T> {
    pub value: T,
    pub migrated: bool,
}

/// Parses strict JSON, then retries after dropping trailing commas.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::NonUtf8`] for non-UTF-8 input and
/// [`ConfigErrorKind::AuthorityValidation`] when recovery still cannot decode
/// a document. The detail names a line and column, never a value.
pub(crate) fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<Decoded<T>, ConfigError> {
    if let Ok(value) = serde_json::from_slice(bytes) {
        return Ok(Decoded {
            value,
            migrated: false,
        });
    }
    let relaxed = relax_trailing_commas(bytes)?;
    if relaxed.as_slice() != bytes
        && let Ok(value) = serde_json::from_slice(&relaxed)
    {
        return Ok(Decoded {
            value,
            migrated: true,
        });
    }
    let location = serde_json::from_slice::<serde_json::Value>(bytes)
        .err()
        .map(|error| format!("line {} column {}", error.line(), error.column()))
        .unwrap_or_else(|| "an unknown position".to_owned());
    Err(ConfigError::authority_rejection()
        .with_detail(format!("JSON could not be read at {location}")))
}

/// Drops commas that sit immediately before `}` or `]` outside strings.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::NonUtf8`] when `bytes` are not UTF-8.
pub(crate) fn relax_trailing_commas(bytes: &[u8]) -> Result<Vec<u8>, ConfigError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| ConfigError::new(ConfigErrorKind::NonUtf8))?;
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escape = false;
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            continue;
        }
        if ch == ',' {
            let mut look = chars.clone();
            if matches!(look.find(|next| !next.is_whitespace()), Some('}' | ']')) {
                continue;
            }
        }
        out.push(ch);
    }
    Ok(out.into_bytes())
}
