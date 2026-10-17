//! Session-family typed identifiers with frozen persistent grammars.
//!
//! Every identifier carries 128 operating-system CSPRNG bits in one canonical
//! spelling: a fixed ASCII prefix followed by exactly 32 lowercase hexadecimal
//! digits. Parsing accepts only that spelling, so an accepted identifier can
//! never contain a path separator, traversal, or non-portable byte.
use std::fmt::{self, Display, Formatter};

const RANDOM_BYTES: usize = 16;

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
