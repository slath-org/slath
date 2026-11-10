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
    /// Everyone, including anonymous visitors, can read it.
    Public,
    /// Every signed-in user can read it; anonymous visitors can't see it.
    Internal,
    /// Only people with a role can see it.
    Private,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoState {
    Active,
    Archived,
    /// Being deleted: invisible to everyone.
    Deleting,
    /// A fork whose owner lost access to its private parent: invisible to
    /// everyone but site administrators, read-only, and deleted after the
    /// retention period unless access returns.
    Locked,
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
    /// An SSH key (Git over SSH): the user's full rights, like a session.
    SshKey {
        id: crate::ids::SshKeyId,
    },
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
    /// A repository's deploy key (Git over SSH): reads that one repository
    /// and, with `write`, pushes to it. Nothing else, anywhere.
    DeployKey {
        id: crate::ids::SshKeyId,
        repo_id: crate::ids::RepoId,
        write: bool,
    },
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
    /// Opening a merge request from a fork in the same network: signed in
    /// and `read` on the target (push on the source is checked there).
    MrCreateFork,
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
        Self::MrCreateFork,
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
            Self::MrCreateFork => "mr.create_fork",
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
    /// The subject's effective role on this repository, if any: the highest
    /// of collaborator role, organization ownership/membership, namespace
    /// ownership, and site administration.
    pub role: Option<Role>,
    /// The repository allows forks.
    pub allow_forking: bool,
}

impl ResourceCtx {
    /// A context with forking allowed.
    #[must_use]
    pub const fn new(
        repo_id: crate::ids::RepoId,
        visibility: Visibility,
        state: RepoState,
        role: Option<Role>,
    ) -> Self {
        Self {
            repo_id,
            visibility,
            state,
            role,
            allow_forking: true,
        }
    }
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

/// Who may perform an action, at minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Needs {
    /// Anyone who can read, including anonymous visitors of public repositories.
    Read,
    /// A signed-in user who can read.
    SignedIn,
    Role(Role),
}

impl Action {
    /// The minimum role (or sign-in) the action needs.
    const fn needs(self) -> Needs {
        use Action as A;
        match self {
            A::RepoRead => Needs::Read,
            A::RepoFork
            | A::IssueCreate
            | A::CommentCreate
            | A::IssueEditOwn
            | A::CommentEditOwn
            | A::MrCreateFork
            | A::MrComment
            | A::MrReview
            | A::MrSuggest
            | A::MrEditOwn => Needs::SignedIn,
            A::RepoPush
            | A::RepoPushProtected
            | A::RepoPushMaintainerEdit
            | A::IssueTriage
            | A::MrCreate
            | A::MrTriage
            | A::MrApprove
            | A::MrMerge
            | A::MrAutoMerge
            | A::MrUpdateBranch
            | A::MrApplySuggestion
            | A::StatusWrite
            | A::CiApproveExternalRun
            | A::CiRerun
            | A::CiCancel => Needs::Role(Role::Write),
            A::TagMoveOrDelete | A::CommentModerate | A::LabelsManage | A::MilestonesManage => {
                Needs::Role(Role::Maintain)
            }
            A::ProtectionManage
            | A::WebhooksManage
            | A::SecretsManage
            | A::CollaboratorsManage
            | A::RepoSettings
            | A::RepoRename
            | A::RepoArchive
            | A::RepoDelete
            | A::RepoTransfer
            | A::RepoVisibility => Needs::Role(Role::Admin),
        }
    }

    /// The token scope that covers the action.
    #[must_use]
    pub const fn scope(self) -> Scope {
        use Action as A;
        match self {
            A::RepoRead => Scope::RepoRead,
            A::IssueCreate
            | A::CommentCreate
            | A::IssueEditOwn
            | A::CommentEditOwn
            | A::IssueTriage
            | A::CommentModerate
            | A::LabelsManage
            | A::MilestonesManage => Scope::IssuesWrite,
            A::MrCreate
            | A::MrCreateFork
            | A::MrComment
            | A::MrReview
            | A::MrSuggest
            | A::MrEditOwn
            | A::MrTriage
            | A::MrApprove
            | A::MrMerge
            | A::MrAutoMerge
            | A::MrUpdateBranch
            | A::MrApplySuggestion => Scope::MrsWrite,
            A::StatusWrite => Scope::StatusesWrite,
            _ => Scope::RepoWrite,
        }
    }

    /// True for actions that change something (denied on archived repositories).
    #[must_use]
    pub const fn is_write(self) -> bool {
        !matches!(self, Self::RepoRead | Self::RepoFork)
    }
}

/// The authorization choke point: every access to repository-scoped data
/// goes through it. `tests/matrix.rs` checks it cell by cell against
/// `tests/matrix.csv`.
///
/// - Repositories being deleted don't exist for anyone.
/// - Suspended and deleted users can do nothing.
/// - Public repositories give everyone `read`.
/// - A token restricted to other repositories acts like an anonymous
///   visitor here; otherwise a token's scopes cap what its role allows.
/// - Archived repositories refuse every write except unarchiving.
/// - A denial is "visible" (403) only if the subject can read the
///   repository; otherwise it's "not found" (404).
#[must_use]
pub fn check(subject: &Subject, action: Action, res: &ResourceCtx) -> Decision {
    const HIDDEN: Decision = Decision::Deny { visible: false };
    if res.state == RepoState::Deleting {
        return HIDDEN;
    }
    if res.state == RepoState::Locked {
        let admin = matches!(
            subject,
            Subject::System
                | Subject::User {
                    is_site_admin: true,
                    state: UserState::Active,
                    ..
                }
        );
        if !admin || action != Action::RepoRead {
            return HIDDEN;
        }
    }
    let (role, signed_in, scopes) = match subject {
        Subject::System => return Decision::Allow,
        Subject::Anonymous => (None, false, None),
        Subject::DeployKey { repo_id, write, .. } => {
            // Other repositories, forks included, don't exist for it.
            if *repo_id != res.repo_id {
                return HIDDEN;
            }
            if !matches!(action, Action::RepoRead | Action::RepoPush) {
                return Decision::Deny { visible: true };
            }
            let role = if *write { Role::Write } else { Role::Read };
            (Some(role), true, None)
        }
        Subject::User {
            state, credential, ..
        } => {
            if *state != UserState::Active {
                return HIDDEN;
            }
            match credential {
                Credential::Session | Credential::SshKey { .. } => (res.role, true, None),
                Credential::Token {
                    scopes, repo_ids, ..
                } => {
                    if repo_ids
                        .as_ref()
                        .is_some_and(|ids| !ids.contains(&res.repo_id))
                    {
                        (None, false, None)
                    } else {
                        (res.role, true, Some(scopes.as_slice()))
                    }
                }
            }
        }
    };
    // Public repositories are readable by everyone, internal ones by every
    // signed-in user (a token restricted to other repositories counts as
    // anonymous here), private ones only with a role.
    let open = match res.visibility {
        Visibility::Public => true,
        Visibility::Internal => signed_in,
        Visibility::Private => false,
    };
    let can_read = open || role.is_some();
    if !can_read {
        return HIDDEN;
    }
    let deny = Decision::Deny { visible: true };
    if let Some(scopes) = scopes
        && !scopes.iter().any(|s| s.implies(action.scope()))
    {
        return deny;
    }
    if res.state == RepoState::Archived && action.is_write() && action != Action::RepoArchive {
        return deny;
    }
    if action == Action::RepoFork && !res.allow_forking {
        return deny;
    }
    let ok = match action.needs() {
        Needs::Read => true,
        Needs::SignedIn => signed_in,
        Needs::Role(min) => signed_in && role.is_some_and(|r| r >= min),
    };
    if ok { Decision::Allow } else { deny }
}

/// Everything that can give a user a role on a repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleGrants {
    pub site_admin: bool,
    /// The user owns the repository's namespace.
    pub namespace_owner: bool,
    pub collaborator: Option<Role>,
    /// Roles of teams the user is a member of.
    pub teams: Vec<Role>,
    /// Owner of the owning organization.
    pub org_owner: bool,
    /// Member of the owning organization.
    pub org_member: bool,
    /// The organization's role for all members (`read`, or none).
    pub org_default_member_role: Option<Role>,
    /// The owning organization requires MFA and the user has none.
    pub org_mfa_unmet: bool,
}

/// The effective role: the highest of every grant. Site administrators are
/// admins everywhere. A user who doesn't meet the organization's MFA
/// requirement gets nothing from any grant on its repositories until they
/// set it up. `crates/core/tests/roles.csv` checks this cell by cell.
#[must_use]
pub fn effective_role(g: &RoleGrants) -> Option<Role> {
    if g.site_admin {
        return Some(Role::Admin);
    }
    if g.org_mfa_unmet {
        return None;
    }
    let owner = (g.namespace_owner || g.org_owner).then_some(Role::Admin);
    let member = if g.org_member {
        g.org_default_member_role
    } else {
        None
    };
    [owner, g.collaborator, member]
        .into_iter()
        .flatten()
        .chain(g.teams.iter().copied())
        .max()
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
