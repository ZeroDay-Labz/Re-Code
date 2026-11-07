//! Strongly typed identifiers.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Creates a new identifier. Identifiers sort by creation time.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Parses an identifier from its text form.
            pub fn parse(text: &str) -> Result<Self, uuid::Error> {
                Uuid::parse_str(text).map(Self)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id_type!(
    /// Identifies a session.
    SessionId
);
id_type!(
    /// Identifies one user request and everything done to answer it.
    TurnId
);
id_type!(
    /// Identifies a pending permission request.
    ApprovalId
);

/// Milliseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixMillis(pub i64);

impl UnixMillis {
    /// The current time.
    #[must_use]
    pub fn now() -> Self {
        let elapsed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        Self(i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
    }
}
