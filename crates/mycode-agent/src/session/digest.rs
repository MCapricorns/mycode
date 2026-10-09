//! Session payload and branch-mutation digests.
//!
//! The branch-mutation digest frames the frozen ASCII domain
//! `mycode-agent-branch-mutation-v1\0` followed by `session-id,
//! reservation-id, kind, source-branch-id, source-head, target-event-id,
//! new-branch-id`; every string is `u32be byte-length || UTF-8`, the kind is a
//! zero-based `u8`, and a head is a zero-based `u8` tag where `event` is
//! followed by the framed event ID. All length conversions are checked.
use std::fmt::Write;

use sha2::{Digest, Sha256};

use super::dto::{BranchMutationKind, HeadStamp};
use super::ids::SessionEventId;

/// Lowercase digest prefix shared by every session digest spelling.
///
/// The spelling is `sha256:` plus 64 lowercase hex digits. The session
/// ledger hashes raw bytes and checks that spelling directly.
pub const DIGEST_PREFIX: &str = "sha256:";
/// ASCII digest payload length: 64 lowercase hexadecimal digits.
pub const DIGEST_HEX_BYTES: usize = 64;

const BRANCH_MUTATION_DOMAIN: &[u8] = b"mycode-agent-branch-mutation-v1\0";

/// Computes the raw SHA-256 digest of one event payload.
#[must_use]
pub fn payload_digest(payload: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    hasher.finalize().into()
}

/// Formats one raw digest as the canonical lowercase `sha256:` spelling.
#[must_use]
pub fn format_digest(raw: &[u8; 32]) -> String {
    let mut spelling = String::with_capacity(DIGEST_PREFIX.len() + DIGEST_HEX_BYTES);
    spelling.push_str(DIGEST_PREFIX);
    for byte in raw {
        write!(spelling, "{byte:02x}").expect("writing to a String cannot fail");
    }
    spelling
}

/// Returns `true` only for the canonical lowercase `sha256:` spelling.
#[must_use]
pub fn is_canonical_digest(value: &str) -> bool {
    let Some(hex) = value.strip_prefix(DIGEST_PREFIX) else {
        return false;
    };
    hex.len() == DIGEST_HEX_BYTES
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Binds the frozen branch-mutation digest input fields.
pub struct BranchMutationDigestInput<'a> {
    /// The session the mutation belongs to.
    pub session_id: &'a str,
    /// The single-use reservation identity.
    pub reservation_id: &'a str,
    /// Fork or rewind.
    pub kind: BranchMutationKind,
    /// The branch whose prefix is copied.
    pub source_branch_id: &'a str,
    /// The source head bound by the reservation.
    pub source_head: &'a HeadStamp,
    /// The prefix boundary event.
    pub target_event_id: &'a SessionEventId,
    /// The branch the mutation creates.
    pub new_branch_id: &'a str,
}

/// Computes the raw branch-mutation digest over the frozen framing.
///
/// Returns `None` only when a string exceeds the checked `u32be` length
/// bound, which the frozen identifier grammars already make impossible.
#[must_use]
pub fn branch_mutation_digest(input: &BranchMutationDigestInput<'_>) -> Option<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(BRANCH_MUTATION_DOMAIN);
    push_framed_string(&mut hasher, input.session_id)?;
    push_framed_string(&mut hasher, input.reservation_id)?;
    hasher.update([input.kind.tag()]);
    push_framed_string(&mut hasher, input.source_branch_id)?;
    match input.source_head {
        HeadStamp::Empty => hasher.update([0]),
        HeadStamp::Event(event) => {
            hasher.update([1]);
            push_framed_string(&mut hasher, event.as_str())?;
        }
    }
    push_framed_string(&mut hasher, input.target_event_id.as_str())?;
    push_framed_string(&mut hasher, input.new_branch_id)?;
    Some(hasher.finalize().into())
}

fn push_framed_string(hasher: &mut Sha256, value: &str) -> Option<()> {
    let length = u32::try_from(value.len()).ok()?;
    hasher.update(length.to_be_bytes());
    hasher.update(value.as_bytes());
    Some(())
}
