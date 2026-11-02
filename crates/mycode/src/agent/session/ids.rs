//! Session-family typed identifiers with frozen persistent grammars.
//!
//! A session id names its directory, so it reads as its creation time:
//! `sess-YYYYMMDD-HHMMSS-` in local time plus 8 random lowercase hex
//! digits. Every other identifier carries 128 operating-system CSPRNG bits:
//! a fixed ASCII prefix followed by exactly 32 lowercase hexadecimal digits.
//! Parsing accepts only those spellings, so an accepted identifier can never
//! contain a path separator, traversal, or non-portable byte.
use std::fmt::{self, Display, Formatter};

const RANDOM_BYTES: usize = 16;
/// Session id prefix.
const SESSION_PREFIX: &str = "sess-";
/// Random hex digits after a session id's timestamp.
const SESSION_RANDOM_DIGITS: usize = 8;

fn is_lower_hex(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

macro_rules! session_id_type {
    ($name:ident, $prefix:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Generates a fresh identifier from the operating-system CSPRNG.
            ///
            /// `Uuid::new_v4` fills 16 bytes from that source and its simple
            /// spelling is already the canonical 32 lowercase hex digits.
            /// `Option` stays in the signature so callers keep their
            /// fail-closed handling if the random source ever fails.
            pub fn generate() -> Option<Self> {
                Some(Self(format!(
                    "{}{}",
                    $prefix,
                    uuid::Uuid::new_v4().simple()
                )))
            }

            /// Parses the one canonical persistent spelling.
            #[must_use]
            pub fn parse(value: &str) -> Option<Self> {
                let suffix = value.strip_prefix($prefix)?;
                if suffix.len() != RANDOM_BYTES * 2 || !is_lower_hex(suffix) {
                    return None;
                }
                Some(Self(value.to_owned()))
            }

            /// Returns the canonical persistent spelling.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

/// Identifies one durable session ledger
/// (`sess-YYYYMMDD-HHMMSS-[0-9a-f]{8}`), and names its directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(String);

impl SessionId {
    /// Generates a fresh identifier stamped with the local time now.
    ///
    /// Two sessions created in the same second differ in their random
    /// digits, and the store refuses an id it already holds. `Option` keeps
    /// callers' fail-closed handling.
    pub fn generate() -> Option<Self> {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let random = uuid::Uuid::new_v4().simple().to_string();
        Some(Self(format!(
            "{SESSION_PREFIX}{stamp}-{}",
            &random[..SESSION_RANDOM_DIGITS]
        )))
    }

    /// Parses the one canonical persistent spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let rest = value.strip_prefix(SESSION_PREFIX)?;
        let mut parts = rest.split('-');
        let (Some(date), Some(time), Some(random), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        let digits = |text: &str, len: usize| {
            text.len() == len && text.bytes().all(|byte| byte.is_ascii_digit())
        };
        if !digits(date, 8)
            || !digits(time, 6)
            || random.len() != SESSION_RANDOM_DIGITS
            || !is_lower_hex(random)
        {
            return None;
        }
        Some(Self(value.to_owned()))
    }

    /// Returns the canonical persistent spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for SessionId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

session_id_type!(
    BranchId,
    "br1-",
    "Identifies one branch of a session ledger (`br1-[0-9a-f]{32}`)."
);
session_id_type!(
    SessionEventId,
    "evt1-",
    "Identifies one appended session event (`evt1-[0-9a-f]{32}`)."
);
session_id_type!(
    SessionCallId,
    "call1-",
    "Binds one tool-call/tool-result event pair (`call1-[0-9a-f]{32}`)."
);
session_id_type!(
    BranchReservationId,
    "sbr1-",
    "Identifies one single-use branch mutation reservation (`sbr1-[0-9a-f]{32}`)."
);

#[cfg(test)]
mod tests {
    use super::SessionId;

    #[test]
    fn a_session_id_reads_as_its_creation_time() {
        let fresh = SessionId::generate().expect("id");
        assert_eq!(SessionId::parse(fresh.as_str()), Some(fresh));
        assert!(SessionId::parse("sess-20261019-153012-0123abcd").is_some());
        for bad in [
            "ses1-0123456789abcdef0123456789abcdef",
            "sess-20261019-153012-0123ABCD",
            "sess-2026101-153012-0123abcd",
            "sess-20261019-153012-0123abc",
            "sess-20261019-153012-0123abcd-",
            "sess-20261019/153012-0123abcd",
        ] {
            assert!(SessionId::parse(bad).is_none(), "{bad}");
        }
    }
}
