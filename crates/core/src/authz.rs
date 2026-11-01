// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Authorization policy types and the single policy function.
//!
//! [`check`] is pure: the `service` crate loads a [`ResourceCtx`] (visibility,
//! state, the subject's effective role, ...) and asks for a [`Decision`].
//! The policy itself isn't implemented yet. Until it is, [`check`] denies
//! everything, so nothing can accidentally be allowed.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::ids::{TokenId, UserId};

/// Repository roles, ordered from least to most privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Read,
    Write,
    Maintain,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Private,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoState {
    Active,
    Archived,
    /// Being deleted: invisible to everyone.
    Deleting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserKind {
    Human,
    Bot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserState {
    Active,
    Suspended,
    Deleted,
}

/// Personal access token scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Scope {
    #[serde(rename = "repo:read")]
    RepoRead,
    #[serde(rename = "repo:write")]
    RepoWrite,
    #[serde(rename = "issues:write")]
    IssuesWrite,
    #[serde(rename = "mrs:write")]
    MrsWrite,
    #[serde(rename = "statuses:write")]
    StatusesWrite,
    #[serde(rename = "user:read")]
    UserRead,
    #[serde(rename = "user:write")]
    UserWrite,
    #[serde(rename = "admin")]
    Admin,
}

/// Error for an unknown scope string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown scope")]
pub struct UnknownScope;

impl Scope {
    pub const ALL: &'static [Scope] = &[
        Self::RepoRead,
        Self::RepoWrite,
        Self::IssuesWrite,
        Self::MrsWrite,
        Self::StatusesWrite,
        Self::UserRead,
        Self::UserWrite,
        Self::Admin,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RepoRead => "repo:read",
            Self::RepoWrite => "repo:write",
            Self::IssuesWrite => "issues:write",
            Self::MrsWrite => "mrs:write",
            Self::StatusesWrite => "statuses:write",
            Self::UserRead => "user:read",
            Self::UserWrite => "user:write",
            Self::Admin => "admin",
        }
    }

    /// What the scope allows, for people choosing scopes.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::RepoRead => "Read repositories (clone, fetch, browse, API reads)",
            Self::RepoWrite => "Read and push to repositories, change repository settings",
            Self::IssuesWrite => "Create and edit issues and comments",
            Self::MrsWrite => "Create, review, and merge merge requests",
            Self::StatusesWrite => "Report commit statuses",
            Self::UserRead => "Read your profile, keys, and tokens",
            Self::UserWrite => "Change your profile and SSH keys",
            Self::Admin => "Site administration (site administrators only)",
        }
    }

    /// True if a token with `self` grants `other` (`repo:write` includes `repo:read`).
    #[must_use]
    pub fn implies(self, other: Scope) -> bool {
        self == other
            || matches!(
                (self, other),
                (Self::RepoWrite, Self::RepoRead) | (Self::UserWrite, Self::UserRead)
            )
    }
}

impl FromStr for Scope {
    type Err = UnknownScope;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|x| x.as_str() == s)
            .ok_or(UnknownScope)
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How the subject authenticated: a session (full user rights) or a token
/// (rights capped by its scopes and optional repository list).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    Session,
    Token {
        id: TokenId,
        scopes: Vec<Scope>,
        /// `None` means all repositories the user can access.
        repo_ids: Option<Vec<crate::ids::RepoId>>,
    },
}

/// The authenticated principal of a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Anonymous,
    User {
        id: UserId,
        kind: UserKind,
        state: UserState,
        is_site_admin: bool,
        credential: Credential,
    },
    /// Slath itself (jobs, reconciler, migrations). Never derived from a request.
    System,
}

/// Every action that `check` decides on. The string form (`repo.read`) is used
/// in the matrix test, audit events, and error details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    RepoRead,
    RepoFork,
    RepoPush,
    RepoPushProtected,
    RepoPushMaintainerEdit,
    TagMoveOrDelete,
    IssueCreate,
    CommentCreate,
    IssueEditOwn,
    CommentEditOwn,
    IssueTriage,
    CommentModerate,
    MrCreate,
    MrComment,
    MrReview,
    MrSuggest,
    MrEditOwn,
    MrTriage,
    MrApprove,
    MrMerge,
    MrAutoMerge,
    MrUpdateBranch,
    MrApplySuggestion,
    StatusWrite,
    CiApproveExternalRun,
    CiRerun,
    CiCancel,
    LabelsManage,
    MilestonesManage,
    ProtectionManage,
    WebhooksManage,
    SecretsManage,
    CollaboratorsManage,
    RepoSettings,
    RepoRename,
    RepoArchive,
    RepoDelete,
    RepoTransfer,
    RepoVisibility,
}

impl Action {
    pub const ALL: &'static [Action] = &[
        Self::RepoRead,
        Self::RepoFork,
        Self::RepoPush,
        Self::RepoPushProtected,
        Self::RepoPushMaintainerEdit,
        Self::TagMoveOrDelete,
        Self::IssueCreate,
        Self::CommentCreate,
        Self::IssueEditOwn,
        Self::CommentEditOwn,
        Self::IssueTriage,
        Self::CommentModerate,
        Self::MrCreate,
        Self::MrComment,
        Self::MrReview,
        Self::MrSuggest,
        Self::MrEditOwn,
        Self::MrTriage,
        Self::MrApprove,
        Self::MrMerge,
        Self::MrAutoMerge,
        Self::MrUpdateBranch,
        Self::MrApplySuggestion,
        Self::StatusWrite,
        Self::CiApproveExternalRun,
        Self::CiRerun,
        Self::CiCancel,
        Self::LabelsManage,
        Self::MilestonesManage,
        Self::ProtectionManage,
        Self::WebhooksManage,
        Self::SecretsManage,
        Self::CollaboratorsManage,
        Self::RepoSettings,
        Self::RepoRename,
        Self::RepoArchive,
        Self::RepoDelete,
        Self::RepoTransfer,
        Self::RepoVisibility,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RepoRead => "repo.read",
            Self::RepoFork => "repo.fork",
            Self::RepoPush => "repo.push",
            Self::RepoPushProtected => "repo.push_protected",
            Self::RepoPushMaintainerEdit => "repo.push_maintainer_edit",
            Self::TagMoveOrDelete => "tag.move_or_delete",
            Self::IssueCreate => "issue.create",
            Self::CommentCreate => "comment.create",
            Self::IssueEditOwn => "issue.edit_own",
            Self::CommentEditOwn => "comment.edit_own",
            Self::IssueTriage => "issue.triage",
            Self::CommentModerate => "comment.moderate",
            Self::MrCreate => "mr.create",
            Self::MrComment => "mr.comment",
            Self::MrReview => "mr.review",
            Self::MrSuggest => "mr.suggest",
            Self::MrEditOwn => "mr.edit_own",
            Self::MrTriage => "mr.triage",
            Self::MrApprove => "mr.approve",
            Self::MrMerge => "mr.merge",
            Self::MrAutoMerge => "mr.auto_merge",
            Self::MrUpdateBranch => "mr.update_branch",
            Self::MrApplySuggestion => "mr.apply_suggestion",
            Self::StatusWrite => "status.write",
            Self::CiApproveExternalRun => "ci.approve_external_run",
            Self::CiRerun => "ci.rerun",
            Self::CiCancel => "ci.cancel",
            Self::LabelsManage => "labels.manage",
            Self::MilestonesManage => "milestones.manage",
            Self::ProtectionManage => "protection.manage",
            Self::WebhooksManage => "webhooks.manage",
            Self::SecretsManage => "secrets.manage",
            Self::CollaboratorsManage => "collaborators.manage",
            Self::RepoSettings => "repo.settings",
            Self::RepoRename => "repo.rename",
            Self::RepoArchive => "repo.archive",
            Self::RepoDelete => "repo.delete",
            Self::RepoTransfer => "repo.transfer",
            Self::RepoVisibility => "repo.visibility",
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown action")]
pub struct UnknownAction;

/// Error for an unknown role string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown role")]
pub struct UnknownRole;

impl FromStr for Action {
    type Err = UnknownAction;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|a| a.as_str() == s)
            .ok_or(UnknownAction)
    }
}

/// Facts about the target resource, loaded by `service` before calling [`check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceCtx {
    pub repo_id: crate::ids::RepoId,
    pub visibility: Visibility,
    pub state: RepoState,
    /// The subject's effective role on this repository, if any.
    pub role: Option<Role>,
}

/// Result of a policy check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// `visible: false` must be reported as not found, `true` as forbidden.
    Deny {
        visible: bool,
    },
}

impl Decision {
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// The authorization choke point: every access to repository-scoped data
/// goes through it.
///
/// Stub: denies every action and hides the resource. `tests/matrix.rs`
/// checks the policy cell by cell against `tests/matrix.csv`.
#[must_use]
pub fn check(_subject: &Subject, _action: Action, _res: &ResourceCtx) -> Decision {
    Decision::Deny { visible: false }
}

impl FromStr for Role {
    type Err = UnknownRole;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "maintain" => Ok(Self::Maintain),
            "admin" => Ok(Self::Admin),
            _ => Err(UnknownRole),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_strings_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for a in Action::ALL {
            assert!(seen.insert(a.as_str()), "duplicate {a}");
            assert_eq!(a.as_str().parse::<Action>(), Ok(*a));
        }
        assert_eq!("repo.nope".parse::<Action>(), Err(UnknownAction));
    }

    #[test]
    fn scope_strings_round_trip() {
        for s in Scope::ALL {
            assert_eq!(s.as_str().parse::<Scope>(), Ok(*s));
        }
        assert!(Scope::RepoWrite.implies(Scope::RepoRead));
        assert!(!Scope::RepoRead.implies(Scope::RepoWrite));
        assert!(Scope::UserWrite.implies(Scope::UserRead));
        assert_eq!("repo".parse::<Scope>(), Err(UnknownScope));
    }

    #[test]
    fn roles_are_ordered() {
        assert!(
            Role::Read < Role::Write
                && Role::Write < Role::Maintain
                && Role::Maintain < Role::Admin
        );
    }
}
