// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Change sources: branches that exist only to carry a merge request, such
//! as the bookmarks `jj git push --change` creates (`push-<change ID>`).
//! A repository names them by a prefix; pushing one opens a merge request
//! without naming a branch, and branch lists leave them out.

/// The prefix of jj's default bookmark template, `"push-" ++
/// change_id.short()`.
pub const DEFAULT_PREFIX: &str = "push-";

/// The longest prefix a repository can set, in bytes.
pub const MAX_PREFIX: usize = 32;

/// Checks a repository's prefix: empty (no change sources), or up to
/// [`MAX_PREFIX`] letters, digits, `-`, `_`, `.`, and `/` that start with
/// a letter or digit and end with `-`, `_`, or `/`, so the prefix can't
/// run into a branch's own name. `mr/` is the server's.
pub fn check_prefix(prefix: &str) -> Result<(), &'static str> {
    if prefix.is_empty() {
        return Ok(());
    }
    if prefix.len() > MAX_PREFIX {
        return Err("Use at most 32 characters.");
    }
    let ok_char = |c: char| c.is_ascii_alphanumeric() || "-_./".contains(c);
    if !prefix.chars().all(ok_char)
        || !prefix.starts_with(|c: char| c.is_ascii_alphanumeric())
        || prefix.contains("..")
        || prefix.contains("//")
        || prefix.contains("/.")
    {
        return Err("Use letters, digits, and - _ . / only, starting with a letter or digit.");
    }
    if !prefix.ends_with(['-', '_', '/']) {
        return Err("End the prefix with -, _, or /.");
    }
    if prefix.starts_with("mr/") {
        return Err("Branches under mr/ are reserved for merge requests from forks.");
    }
    Ok(())
}

/// Whether `branch` (without `refs/heads/`) is a change source under
/// `prefix`: the prefix and at least one more character.
#[must_use]
pub fn is_source(prefix: &str, branch: &str) -> bool {
    !prefix.is_empty() && branch.len() > prefix.len() && branch.starts_with(prefix)
}

/// The change ID a change source is named by (the name after the prefix,
/// when it's 8 to 32 characters of jj's change ID alphabet `k`–`z`, as
/// jj's `push-<change ID>` bookmarks are), else `None`.
#[must_use]
pub fn change_id<'a>(prefix: &str, branch: &'a str) -> Option<&'a str> {
    if !is_source(prefix, branch) {
        return None;
    }
    let rest = &branch[prefix.len()..];
    ((8..=32).contains(&rest.len()) && rest.bytes().all(|b| (b'k'..=b'z').contains(&b)))
        .then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        for ok in ["", "push-", "review/", "jj_", "a.b-"] {
            assert_eq!(check_prefix(ok), Ok(()), "{ok}");
        }
        for bad in [
            "push", "-push-", "/x-", "a..b-", "a//b/", "a/.b-", "ä-", "a b-", "mr/", "mr/x-",
        ] {
            assert!(check_prefix(bad).is_err(), "{bad}");
        }
        assert!(check_prefix(&"a".repeat(33)).is_err());
        assert!(check_prefix(&format!("{}-", "a".repeat(31))).is_ok());
    }

    #[test]
    fn sources() {
        assert!(is_source("push-", "push-kxqzlmno"));
        assert!(!is_source("push-", "push-"));
        assert!(!is_source("push-", "main"));
        assert!(!is_source("", "push-x"));
    }

    #[test]
    fn change_ids() {
        assert_eq!(
            change_id("push-", "push-kxqzlmnopqrs"),
            Some("kxqzlmnopqrs")
        );
        assert_eq!(change_id("push-", "push-kxqz"), None);
        assert_eq!(change_id("push-", "push-fix-login"), None);
        assert_eq!(change_id("push-", "topic-kxqzlmnopqrs"), None);
        assert_eq!(change_id("", "kxqzlmnopqrs"), None);
    }
}
