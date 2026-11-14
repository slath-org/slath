// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Origin labels: who wrote a piece of user-generated text, relative to the
//! repository it belongs to and as they stood when writing it. API responses
//! and webhooks carry it next to `untrusted_content`, so agent harnesses can
//! treat the text as data and weigh it by its author.

use serde::{Deserialize, Serialize};

use crate::authz::Role;

/// Ordered from least to most trusted; an edit by someone else keeps the
/// lesser of the two ([`Origin::least`]), as the database does
/// (`slath_origin_least`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// No account, such as text whose author was gone before origins were
    /// recorded.
    Anonymous,
    /// A signed-in person without a role on the repository.
    OutsideContributor,
    /// Any bot account, whatever its role.
    Bot,
    /// A person with the read role, or a member of the owning organization.
    Member,
    /// A person with the write role or higher.
    Writer,
}

impl Origin {
    pub const ALL: [Self; 5] = [
        Self::Anonymous,
        Self::OutsideContributor,
        Self::Bot,
        Self::Member,
        Self::Writer,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Anonymous => "anonymous",
            Self::OutsideContributor => "outside_contributor",
            Self::Bot => "bot",
            Self::Member => "member",
            Self::Writer => "writer",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.as_str() == s)
    }

    /// A person's origin from their effective role on the repository and
    /// whether they belong to its organization.
    #[must_use]
    pub fn of_person(role: Option<Role>, org_member: bool) -> Self {
        match role {
            Some(Role::Write | Role::Maintain | Role::Admin) => Self::Writer,
            Some(Role::Read) => Self::Member,
            None if org_member => Self::Member,
            None => Self::OutsideContributor,
        }
    }

    /// The less trusted of two origins.
    #[must_use]
    pub fn least(self, other: Self) -> Self {
        self.min(other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_and_names_round_trip() {
        for (i, o) in Origin::ALL.iter().enumerate() {
            assert_eq!(Origin::parse(o.as_str()), Some(*o));
            if i > 0 {
                assert!(Origin::ALL[i - 1] < *o);
            }
        }
        assert_eq!(Origin::parse("owner"), None);
    }

    #[test]
    fn people_by_role() {
        assert_eq!(Origin::of_person(Some(Role::Admin), false), Origin::Writer);
        assert_eq!(Origin::of_person(Some(Role::Write), false), Origin::Writer);
        assert_eq!(Origin::of_person(Some(Role::Read), false), Origin::Member);
        assert_eq!(Origin::of_person(None, true), Origin::Member);
        assert_eq!(Origin::of_person(None, false), Origin::OutsideContributor);
    }

    #[test]
    fn edits_keep_the_lesser() {
        assert_eq!(Origin::Writer.least(Origin::Bot), Origin::Bot);
        assert_eq!(
            Origin::OutsideContributor.least(Origin::Member),
            Origin::OutsideContributor
        );
    }
}
