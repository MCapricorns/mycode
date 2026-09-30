//! Compaction checkpoints: durable summaries that let a turn send the model
//! a compact prefix instead of the whole conversation. The session ledger is
//! never rewritten; a checkpoint only narrows what a provider request
//! carries, so replay and history stay complete.

use serde::{Deserialize, Serialize};

use crate::secure_fs::owned_file::{locked_update_owned_file, read_owned_file};
use crate::{ConfigError, HomeLayout, MAX_AUTHORITY_DOCUMENT_BYTES};

/// Current checkpoint document version.
pub const COMPACTION_FORMAT_VERSION: u32 = 1;
/// Document kind marker; rejects foreign files at the same path.
pub const COMPACTION_KIND: &str = "compaction";
/// Largest accepted summary text.
pub const MAX_SUMMARY_CHARS: usize = 24_000;
/// Largest accepted id fields.
const MAX_ID_CHARS: usize = 128;

/// One session's durable compaction checkpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompactionCheckpoint {
    /// Document schema version ([`COMPACTION_FORMAT_VERSION`]).
    pub format_version: u32,
    /// Document kind marker ([`COMPACTION_KIND`]).
    pub kind: String,
    /// Session the checkpoint belongs to.
    pub session_id: String,
    /// Branch the summary covers.
    pub branch_id: String,
    /// Ledger head stamp the summary covers.
    pub covered_head: String,
    /// Leading history messages the summary replaces.
    pub covered_messages: usize,
    /// The summary text itself.
    pub summary: String,
    /// Provider model that produced the summary.
    pub model: String,
    /// Creation time in seconds since the Unix epoch.
    pub created_at_unix: u64,
}

impl CompactionCheckpoint {
    /// Strict field validation.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::authority_rejection`] for wrong version, kind,
    /// malformed ids, or oversized summary.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let valid_id = |value: &str| {
            !value.is_empty()
                && value.chars().count() <= MAX_ID_CHARS
                && !value.chars().any(char::is_control)
        };
        if self.format_version != COMPACTION_FORMAT_VERSION
            || self.kind != COMPACTION_KIND
            || !valid_id(&self.session_id)
            || !valid_id(&self.branch_id)
            || !valid_id(&self.model)
            || self.summary.chars().count() > MAX_SUMMARY_CHARS
            || self.covered_head.chars().count() > MAX_ID_CHARS
        {
            return Err(ConfigError::authority_rejection());
        }
        Ok(())
    }
}

/// Rough token estimate: four characters per token plus a small constant.
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    estimate_token_count(text.chars().count())
}

/// Token estimate from an already-counted character length.
///
/// Callers that already walk message text use this so compaction does not
/// allocate a second copy of the transcript just to measure it.
#[must_use]
pub fn estimate_token_count(chars: usize) -> usize {
    chars / 4 + 8
}

fn compaction_path(session_id: &str) -> Result<String, ConfigError> {
    crate::session_relative(session_id, "compaction.json")
}

/// Reads one session's checkpoint; a missing file yields `None`.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security, IO failure, or a
/// document that fails strict validation.
pub fn read_compaction(
    home: &HomeLayout,
    session_id: &str,
) -> Result<Option<CompactionCheckpoint>, ConfigError> {
    let path = compaction_path(session_id)?;
    let Some(bytes) = read_owned_file(home, &path, MAX_AUTHORITY_DOCUMENT_BYTES)? else {
        return Ok(None);
    };
    let checkpoint: CompactionCheckpoint =
        serde_json::from_slice(&bytes).map_err(|_| ConfigError::authority_rejection())?;
    checkpoint.validate()?;
    Ok(Some(checkpoint))
}

/// Atomically replaces one session's checkpoint.
///
/// # Errors
///
/// Returns [`ConfigError`] for validation or owned-file failures; a failed
/// write leaves any previous checkpoint untouched.
pub fn write_compaction(
    home: &HomeLayout,
    session_id: &str,
    checkpoint: &CompactionCheckpoint,
) -> Result<(), ConfigError> {
    checkpoint.validate()?;
    if checkpoint.session_id != session_id {
        return Err(ConfigError::authority_rejection());
    }
    let path = compaction_path(session_id)?;
    let mut wire = serde_json::to_vec_pretty(checkpoint)
        .map_err(|_| ConfigError::new(crate::ConfigErrorKind::Serialization))?;
    wire.push(b'\n');
    locked_update_owned_file(home, &path, MAX_AUTHORITY_DOCUMENT_BYTES, |_| Ok(wire))
}
