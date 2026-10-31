// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Git version parsing and the minimum version check.

use std::fmt;

/// Minimum supported Git version on the server.
pub const MIN_GIT_VERSION: GitVersion = GitVersion {
    major: 2,
    minor: 43,
    patch: 0,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl GitVersion {
    /// Parses `git version 2.43.0` (also with vendor suffixes such as
    /// `2.39.3 (Apple Git-146)` or `2.45.0.windows.1`).
    #[must_use]
    pub fn parse(output: &str) -> Option<Self> {
        let rest = output.trim().strip_prefix("git version ")?;
        let mut nums = rest
            .split(|c: char| !c.is_ascii_digit())
            .take(3)
            .map(|n| n.parse::<u32>().ok());
        let major = nums.next()??;
        let minor = nums.next()??;
        let patch = nums.next().flatten().unwrap_or(0);
        Some(Self {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for GitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        let v = |s| GitVersion::parse(s);
        assert_eq!(
            v("git version 2.43.0\n"),
            Some(GitVersion {
                major: 2,
                minor: 43,
                patch: 0
            })
        );
        assert_eq!(
            v("git version 2.39.3 (Apple Git-146)").map(|v| v.minor),
            Some(39)
        );
        assert_eq!(v("git version 2.45.0.windows.1").map(|v| v.patch), Some(0));
        assert_eq!(
            v("git version 3.0"),
            Some(GitVersion {
                major: 3,
                minor: 0,
                patch: 0
            })
        );
        assert_eq!(v("hg 6.0"), None);
        assert!(v("git version 2.42.9").is_some_and(|v| v < MIN_GIT_VERSION));
        assert!(v("git version 2.43.0").is_some_and(|v| v >= MIN_GIT_VERSION));
    }
}
