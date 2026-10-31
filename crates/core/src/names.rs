// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Validation of user, organization, and repository names.
//!
//! Names match `[A-Za-z0-9][A-Za-z0-9._-]{0,38}`, must not end in `.git`
//! (any case), and namespace names must not be reserved. Restricting names to
//! ASCII rules out Unicode confusables and normalization ambiguities. Names are
//! compared case-insensitively (the database column is `citext`).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Maximum length of a name in bytes (all valid names are ASCII).
pub const MAX_NAME_LEN: usize = 39;

/// First path segments and names that can't be used as namespaces.
///
/// Kept in sync with the `reserved_names` seed migration by a test in
/// `slath-db`. Compare with [`is_reserved`], which ignores case.
pub const RESERVED_NAMES: &[&str] = &[
    "-",
    "about",
    "admin",
    "api",
    "assets",
    "explore",
    "favicon.ico",
    "ghost",
    "git",
    "help",
    "login",
    "logout",
    "new",
    "notifications",
    "org",
    "orgs",
    "robots.txt",
    "root",
    "search",
    "settings",
    "signup",
    "slath",
    "static",
    "system",
    "user",
    "users",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("name must not be empty")]
    Empty,
    #[error("name must be at most {MAX_NAME_LEN} characters")]
    TooLong,
    #[error("name must start with a letter or digit")]
    InvalidStart,
    #[error("name may only contain letters, digits, '.', '_' and '-'")]
    InvalidChar,
    #[error("name must not end in '.git'")]
    GitSuffix,
    #[error("name is reserved")]
    Reserved,
}

/// Returns true if `name` is reserved (case-insensitive).
#[must_use]
pub fn is_reserved(name: &str) -> bool {
    RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(name))
}

fn validate(s: &str) -> Result<(), NameError> {
    let bytes = s.as_bytes();
    let first = *bytes.first().ok_or(NameError::Empty)?;
    if !first.is_ascii_alphanumeric() {
        return Err(if first.is_ascii() {
            NameError::InvalidStart
        } else {
            NameError::InvalidChar
        });
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(NameError::InvalidChar);
    }
    if bytes.len() > MAX_NAME_LEN {
        return Err(NameError::TooLong);
    }
    if bytes.len() >= 4 && bytes[bytes.len() - 4..].eq_ignore_ascii_case(b".git") {
        return Err(NameError::GitSuffix);
    }
    Ok(())
}

/// A validated repository name. Reserved namespace names are allowed here.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoName(String);

/// A validated user or organization name (not reserved).
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NamespaceName(String);

impl RepoName {
    pub fn parse(s: &str) -> Result<Self, NameError> {
        validate(s)?;
        Ok(Self(s.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl NamespaceName {
    pub fn parse(s: &str) -> Result<Self, NameError> {
        validate(s)?;
        if is_reserved(s) {
            return Err(NameError::Reserved);
        }
        Ok(Self(s.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! name_impls {
    ($($t:ident),*) => {$(
        impl TryFrom<String> for $t {
            type Error = NameError;
            fn try_from(s: String) -> Result<Self, NameError> {
                Self::parse(&s)
            }
        }
        impl From<$t> for String {
            fn from(n: $t) -> String {
                n.0
            }
        }
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl fmt::Debug for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($t), "({:?})"), self.0)
            }
        }
        impl AsRef<str> for $t {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    )*};
}
name_impls!(RepoName, NamespaceName);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples() {
        for ok in [
            "a",
            "alice",
            "A1",
            "my-repo",
            "my_repo.rs",
            "x".repeat(39).as_str(),
            "git-tools",
        ] {
            assert!(RepoName::parse(ok).is_ok(), "{ok}");
        }
        let cases: &[(&str, NameError)] = &[
            ("", NameError::Empty),
            ("-x", NameError::InvalidStart),
            (".hidden", NameError::InvalidStart),
            ("_x", NameError::InvalidStart),
            ("a/b", NameError::InvalidChar),
            ("a b", NameError::InvalidChar),
            ("a\0", NameError::InvalidChar),
            ("repo.git", NameError::GitSuffix),
            ("repo.GiT", NameError::GitSuffix),
            ("ålice", NameError::InvalidChar),
            ("аlice", NameError::InvalidChar), // Cyrillic 'а'
            ("alice\u{200b}", NameError::InvalidChar),
        ];
        for (s, err) in cases {
            assert_eq!(RepoName::parse(s), Err(err.clone()), "{s:?}");
        }
        assert_eq!(RepoName::parse(&"x".repeat(40)), Err(NameError::TooLong));
    }

    #[test]
    fn reserved_names_only_block_namespaces() {
        assert_eq!(NamespaceName::parse("Admin"), Err(NameError::Reserved));
        assert_eq!(NamespaceName::parse("API"), Err(NameError::Reserved));
        assert!(RepoName::parse("admin").is_ok());
        assert!(NamespaceName::parse("alice").is_ok());
    }

    #[test]
    fn reserved_list_is_sorted_and_lowercase() {
        let mut sorted = RESERVED_NAMES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, RESERVED_NAMES);
        assert!(RESERVED_NAMES.iter().all(|n| n.to_ascii_lowercase() == *n));
    }
}
