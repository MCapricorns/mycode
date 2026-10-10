//! Shared strict authority value types reused by every persisted document.
//!
//! Each type owns one frozen grammar and fails closed on any noncanonical
//! spelling. Documents built on these values keep their own exact schemas;
//! this module defines no file format.

use crate::{ConfigError, ConfigErrorKind};

const MAX_AUTHORITY_REVISION: u64 = i64::MAX as u64;

/// Identifies a logical or persisted authority document revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorityRevision(u64);

impl AuthorityRevision {
    /// Logical revision used when the authority document is absent.
    pub const ABSENT: Self = Self(0);

    /// Creates a bounded authority revision, including logical absence.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] above `i64::MAX`.
    pub fn new(value: u64) -> Result<Self, ConfigError> {
        if value > MAX_AUTHORITY_REVISION {
            return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
        }
        Ok(Self(value))
    }

    /// Returns the numeric revision.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    pub(crate) fn checked_next(self) -> Result<Self, ConfigError> {
        if self.0 >= MAX_AUTHORITY_REVISION {
            return Err(ConfigError::new(ConfigErrorKind::RevisionExhausted));
        }
        Ok(Self(self.0 + 1))
    }
}
