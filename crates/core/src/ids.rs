// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed UUIDv7 identifiers.
//!
//! Each entity gets its own newtype so IDs of different kinds can't be mixed
//! up. The string form is the lowercase hyphenated UUID, which is also what
//! repository storage paths are built from.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Error returned when parsing an ID from a string fails.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid id")]
pub struct ParseIdError;

macro_rules! id_type {
    ($($(#[$meta:meta])* $name:ident),* $(,)?) => {$(
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a new time-ordered (v7) ID.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Wraps an existing UUID, e.g. one loaded from the database.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0.hyphenated(), f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0.hyphenated())
            }
        }

        impl FromStr for $name {
            type Err = ParseIdError;

            /// Accepts only the canonical lowercase hyphenated form, so every
            /// ID has exactly one string representation.
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let uuid = Uuid::try_parse(s).map_err(|_| ParseIdError)?;
                if uuid.hyphenated().to_string() != s {
                    return Err(ParseIdError);
                }
                Ok(Self(uuid))
            }
        }
    )*};
}

id_type!(
    /// A user or organization namespace.
    NamespaceId,
    /// A human or bot account.
    UserId,
    /// An organization.
    OrgId,
    /// A repository. Also names its directory on disk.
    RepoId,
    /// A personal access token.
    TokenId,
    /// An SSH public key.
    SshKeyId,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_v7_and_ordered() {
        let a = RepoId::new();
        let b = RepoId::new();
        assert_eq!(a.as_uuid().get_version_num(), 7);
        assert!(a < b);
    }

    #[test]
    fn round_trips_canonical_form_only() {
        let id = UserId::new();
        let s = id.to_string();
        assert_eq!(s.parse::<UserId>(), Ok(id));
        assert_eq!(s.to_uppercase().parse::<UserId>(), Err(ParseIdError));
        assert_eq!(s.replace('-', "").parse::<UserId>(), Err(ParseIdError));
        assert_eq!("../etc".parse::<UserId>(), Err(ParseIdError));
    }
}
