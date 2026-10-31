// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Property tests for name validation.

use proptest::prelude::*;
use slath_core::names::{NameError, NamespaceName, RepoName, is_reserved};

proptest! {
    #[test]
    fn valid_names_are_accepted(s in "[A-Za-z0-9][A-Za-z0-9._-]{0,38}") {
        prop_assume!(!s.to_ascii_lowercase().ends_with(".git"));
        prop_assert_eq!(RepoName::parse(&s).map(|n| n.to_string()), Ok(s.clone()));
        prop_assert_eq!(NamespaceName::parse(&s).is_ok(), !is_reserved(&s));
    }

    #[test]
    fn git_suffix_is_rejected(stem in "[A-Za-z0-9][A-Za-z0-9._-]{0,34}", suffix in "\\.[gG][iI][tT]") {
        let s = format!("{stem}{suffix}");
        prop_assert_eq!(RepoName::parse(&s), Err(NameError::GitSuffix));
    }

    #[test]
    fn bad_first_char_is_rejected(first in "[-._]", rest in "[A-Za-z0-9]{0,10}") {
        prop_assert_eq!(RepoName::parse(&format!("{first}{rest}")), Err(NameError::InvalidStart));
    }

    /// Any non-ASCII character (confusables, zero-width, combining marks, ...) is rejected.
    #[test]
    fn non_ascii_is_rejected(prefix in "[a-z]{0,5}", c in any::<char>().prop_filter("non-ascii", |c| !c.is_ascii()), suffix in "[a-z]{0,5}") {
        let s = format!("{prefix}{c}{suffix}");
        prop_assert_eq!(RepoName::parse(&s), Err(NameError::InvalidChar));
    }

    #[test]
    fn path_and_option_characters_are_rejected(prefix in "[a-z]{1,5}", c in "[/\\\\ :~^?*\\[@{}\0\n\t]", suffix in "[a-z]{0,5}") {
        let s = format!("{prefix}{c}{suffix}");
        prop_assert_eq!(RepoName::parse(&s), Err(NameError::InvalidChar));
    }

    #[test]
    fn too_long_is_rejected(s in "[a-z]{40,80}") {
        prop_assert_eq!(RepoName::parse(&s), Err(NameError::TooLong));
    }

    #[test]
    fn parse_never_panics(s in any::<String>()) {
        let _ = RepoName::parse(&s);
        let _ = NamespaceName::parse(&s);
    }
}
