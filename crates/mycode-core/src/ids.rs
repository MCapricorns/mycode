//! Strongly-typed identifiers for tool calls.
//!
//! Call ids are transparent newtypes over `String`: they serialize as plain
//! JSON strings and `Display` as their inner value. `new()` generates a
//! random UUIDv4-backed id; `From<String>` / `FromStr` accept arbitrary
//! provider-assigned ids (e.g. OpenAI `call_…` tool call ids).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Generate a new random id (UUIDv4).
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4().to_string())
            }

            /// Borrow the inner string.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the id and return the inner string.
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }

        impl FromStr for $name {
            type Err = std::convert::Infallible;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(s.to_owned()))
            }
        }
    };
}

id_type!(
    CallId,
    "Identifier of a tool call (provider-assigned or generated)."
);
