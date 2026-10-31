// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Validated user input for Git arguments.
//!
//! Values of these types are safe to pass to Git in their documented
//! position: after `--end-of-options` (revisions, refs) or after `--` (paths).
//! None of them can start with `-`.

use crate::object::ObjectId;

/// Maximum length of a repository path in bytes.
pub const MAX_PATH_LEN: usize = 4096;
/// Maximum length of a ref name in bytes.
pub const MAX_REF_LEN: usize = 1024;

/// A full ref name (`refs/...`) that satisfies `git check-ref-format`.
/// Ref names are bytes: they may be non-UTF-8.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RefName(Vec<u8>);

impl RefName {
    /// Validates a full ref name using the rules of `git check-ref-format`
    /// (without `--allow-onelevel` or `--refspec-pattern`).
    pub fn parse(name: impl Into<Vec<u8>>) -> Result<Self, &'static str> {
        let name = name.into();
        check_ref_format(&name)?;
        if !name.starts_with(b"refs/") {
            return Err("ref name must start with refs/");
        }
        Ok(Self(name))
    }

    /// `refs/heads/<branch>` for a short branch name.
    pub fn branch(short: &[u8]) -> Result<Self, &'static str> {
        Self::under(b"refs/heads/", short)
    }

    /// `refs/tags/<tag>` for a short tag name.
    pub fn tag(short: &[u8]) -> Result<Self, &'static str> {
        Self::under(b"refs/tags/", short)
    }

    fn under(prefix: &[u8], short: &[u8]) -> Result<Self, &'static str> {
        if short.first() == Some(&b'-') {
            return Err("name must not start with '-'");
        }
        let mut full = prefix.to_vec();
        full.extend_from_slice(short);
        Self::parse(full)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// True if the ref is in a namespace clients must never write.
    #[must_use]
    pub fn is_reserved(&self) -> bool {
        self.0.starts_with(b"refs/slath/") || self.0.starts_with(b"refs/merge-requests/")
    }
}

impl std::fmt::Debug for RefName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RefName({:?})", String::from_utf8_lossy(&self.0))
    }
}

/// Implements the rules from `git help check-ref-format`.
fn check_ref_format(name: &[u8]) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > MAX_REF_LEN {
        return Err("ref name has invalid length");
    }
    if name == b"@" {
        return Err("ref name must not be '@'");
    }
    if name.ends_with(b"/") || name.ends_with(b".") {
        return Err("ref name must not end with '/' or '.'");
    }
    if name
        .windows(2)
        .any(|w| w == b".." || w == b"@{" || w == b"//")
    {
        return Err("ref name must not contain '..', '@{', or '//'");
    }
    if name
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b" ~^:?*[\\".contains(&b))
    {
        return Err("ref name contains a forbidden character");
    }
    if !name.contains(&b'/') {
        return Err("ref name must have at least two components");
    }
    for component in name.split(|&b| b == b'/') {
        if component.starts_with(b".") {
            return Err("ref name component must not start with '.'");
        }
        if component.ends_with(b".lock") {
            return Err("ref name component must not end with '.lock'");
        }
    }
    Ok(())
}

/// A revision supplied by a user: a full object ID or a branch/tag name.
///
/// Short names are never passed to Git. The caller resolves a [`Revision`]
/// with [`crate::Runner::resolve_commit`], which builds the full ref itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Revision {
    Oid(ObjectId),
    Branch(RefName),
    Tag(RefName),
}

impl Revision {
    /// Parses a user revision. 40 lowercase hex digits are an object ID;
    /// anything else must be a valid branch name.
    pub fn parse_branch_or_oid(s: &[u8]) -> Result<Self, &'static str> {
        if let Ok(oid) = ObjectId::parse(s) {
            return Ok(Self::Oid(oid));
        }
        RefName::branch(s).map(Self::Branch)
    }

    pub fn parse_tag(s: &[u8]) -> Result<Self, &'static str> {
        RefName::tag(s).map(Self::Tag)
    }

    /// The argument to place after `--end-of-options`, peeled to a commit.
    #[must_use]
    pub fn to_commit_arg(&self) -> Vec<u8> {
        let mut arg = match self {
            Self::Oid(oid) => oid.as_str().as_bytes().to_vec(),
            Self::Branch(r) | Self::Tag(r) => r.as_bytes().to_vec(),
        };
        arg.extend_from_slice(b"^{commit}");
        arg
    }
}

/// A path inside a repository tree, as supplied by a user.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RepoPath(Vec<u8>);

impl RepoPath {
    /// Rejects NUL bytes, absolute paths, `..` components, and paths longer
    /// than [`MAX_PATH_LEN`]. The empty path is the tree root.
    pub fn parse(path: impl Into<Vec<u8>>) -> Result<Self, &'static str> {
        let path = path.into();
        if path.len() > MAX_PATH_LEN {
            return Err("path too long");
        }
        if path.contains(&0) {
            return Err("path contains NUL");
        }
        if path.starts_with(b"/") {
            return Err("path must be relative");
        }
        if path.split(|&b| b == b'/').any(|c| c == b"..") {
            return Err("path must not contain '..'");
        }
        Ok(Self(path))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for RepoPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RepoPath({:?})", String::from_utf8_lossy(&self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_names() {
        for ok in [
            &b"refs/heads/main"[..],
            b"refs/heads/feature/x",
            b"refs/tags/v1.0",
            b"refs/heads/\xff\xfe",
            b"refs/heads/-x",
        ] {
            assert!(
                RefName::parse(ok).is_ok(),
                "{:?}",
                String::from_utf8_lossy(ok)
            );
        }
        for bad in [
            &b""[..],
            b"main",
            b"refs/heads/",
            b"refs/heads/a..b",
            b"refs/heads/a b",
            b"refs/heads/a~1",
            b"refs/heads/a^",
            b"refs/heads/a:b",
            b"refs/heads/a?",
            b"refs/heads/a*",
            b"refs/heads/a[",
            b"refs/heads/a\\b",
            b"refs/heads/a\x01",
            b"refs/heads/a\x7f",
            b"refs/heads/.hidden",
            b"refs/heads/x.lock",
            b"refs/heads/a@{1}",
            b"refs/heads//a",
            b"refs/heads/a.",
            b"HEAD",
        ] {
            assert!(
                RefName::parse(bad).is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn option_like_revisions_are_rejected() {
        assert!(Revision::parse_branch_or_oid(b"--output=/tmp/x").is_err());
        assert!(Revision::parse_branch_or_oid(b"-n").is_err());
        assert!(Revision::parse_tag(b"--all").is_err());
        assert!(Revision::parse_branch_or_oid(b"HEAD~1").is_err());
        assert!(Revision::parse_branch_or_oid(b"main@{yesterday}").is_err());
        assert_eq!(
            Revision::parse_branch_or_oid(b"main").map(|r| r.to_commit_arg()),
            Ok(b"refs/heads/main^{commit}".to_vec())
        );
        let oid = "0123456789abcdef0123456789abcdef01234567";
        assert!(matches!(
            Revision::parse_branch_or_oid(oid.as_bytes()),
            Ok(Revision::Oid(_))
        ));
    }

    #[test]
    fn repo_paths() {
        for ok in [
            &b""[..],
            b"README.md",
            b"a/b/c",
            b"a/..b/c..",
            b"\xff.bin",
            b"-rf",
        ] {
            assert!(RepoPath::parse(ok).is_ok());
        }
        for bad in [&b"/etc/passwd"[..], b"..", b"a/../b", b"a/..", b"a\0b"] {
            assert!(RepoPath::parse(bad).is_err());
        }
        assert!(RepoPath::parse(vec![b'a'; MAX_PATH_LEN + 1]).is_err());
    }

    #[test]
    fn reserved_refs() {
        assert!(RefName::parse(&b"refs/slath/mr/1/r1"[..]).is_ok_and(|r| r.is_reserved()));
        assert!(RefName::parse(&b"refs/merge-requests/1/head"[..]).is_ok_and(|r| r.is_reserved()));
        assert!(!RefName::branch(b"slath").is_ok_and(|r| r.is_reserved()));
    }
}
