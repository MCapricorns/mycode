//! Session-family typed identifiers with frozen persistent grammars.
//!
//! Every identifier carries 128 operating-system CSPRNG bits in one canonical
//! spelling: a fixed ASCII prefix followed by exactly 32 lowercase hexadecimal
//! digits. Parsing accepts only that spelling, so an accepted identifier can
//! never contain a path separator, traversal, or non-portable byte.
use std::fmt::{self, Display, Formatter};

const RANDOM_BYTES: usize = 16;
const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

macro_rules! session_id_type {
    ($name:ident, $prefix:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Generates a fresh identifier from the operating-system CSPRNG.
            ///
            /// Returns `None` when the operating-system random source cannot
            /// fill the required 16-byte buffer. Callers must treat failure as
            /// fail-closed and must not fall back to a weaker source.
            pub fn generate() -> Option<Self> {
                let mut random = [0_u8; RANDOM_BYTES];
                getrandom::fill(&mut random).ok()?;
                let mut spelling = String::with_capacity($prefix.len() + RANDOM_BYTES * 2);
                spelling.push_str($prefix);
                for byte in random {
                    spelling.push(char::from(LOWER_HEX[usize::from(byte >> 4)]));
                    spelling.push(char::from(LOWER_HEX[usize::from(byte & 0x0f)]));
                }
                Some(Self(spelling))
            }

            /// Parses the one canonical persistent spelling.
            #[must_use]
            pub fn parse(value: &str) -> Option<Self> {
                let suffix = value.strip_prefix($prefix)?;
                if suffix.len() != RANDOM_BYTES * 2
                    || !suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

session_id_type!(
    SessionId,
    "ses1-",
    "Identifies one durable session ledger (`ses1-[0-9a-f]{32}`)."
);
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
