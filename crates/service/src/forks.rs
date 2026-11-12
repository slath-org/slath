// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Forks: a new repository in a namespace of the forker's, with the
//! parent's branches and tags.
//!
//! The request creates the (empty) repository and queues `repo.fork`. The
//! job clones the parent locally, so objects are hardlinked when both are
//! on the same filesystem, and swaps the clone into place; pushes to the
//! fork wait until it's done. Only branches and tags are copied. There are
//! no alternates: a fork stays whole when its parent is deleted or
//! garbage-collected. A fork is never more visible than its parent was.

use slath_core::authz::{Action, RepoState, Subject, Visibility};
use slath_core::ids::{RepoId, UserId};
use slath_db::jobs::LeasedJob;
use slath_db::worker::JobError;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::repos::{NewRepoInput, RepoAccess};

/// The job that copies a fork's parent.
pub const REPO_FORK: &str = "repo.fork";
/// The job that locks (or unlocks) the forks of a repository whose owners
/// lost (or regained) read access to it.
pub const FORK_ACCESS_REVIEW: &str = "fork.access_review";

/// Where to fork to.
#[derive(Debug, Clone)]
pub struct ForkInput {
    /// The user or organization to fork into.
    pub namespace: String,
    /// Empty: the parent's name.
    pub name: String,
    /// `None`: the parent's visibility.
    pub visibility: Option<Visibility>,
    pub default_branch_only: bool,
}

/// What `create_repo_as` records for a fork.
pub(crate) struct ForkPlan {
    pub parent: RepoId,
    pub default_branch_only: bool,
    pub max_visibility: Visibility,
    pub by: UserId,
}

/// Visibilities from least to most visible.
#[must_use]
pub(crate) const fn rank(v: Visibility) -> u8 {
    match v {
        Visibility::Private => 0,
        Visibility::Internal => 1,
        Visibility::Public => 2,
    }
}

/// A fork's branch next to the same branch of its parent.
#[derive(Debug, Clone)]
pub struct BranchCompare {
    /// `ns/name` of the parent.
    pub parent: String,
    pub branch: String,
    /// Commits only the fork has, and only the parent has.
    pub ahead: u32,
    pub behind: u32,
}

/// What syncing a fork's branch did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Synced {
    UpToDate,
    FastForwarded,
    /// The fork didn't have the branch.
    Created,
    /// The fork's own commits were dropped.
    Discarded,
    /// Nothing changed: both have commits the other lacks.
    Diverged {
        ahead: u32,
        behind: u32,
    },
}

impl Synced {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UpToDate => "up_to_date",
            Self::FastForwarded => "fast_forwarded",
            Self::Created => "created",
            Self::Discarded => "discarded",
            Self::Diverged { .. } => "diverged",
        }
    }
}

/// A fork as its viewers see it.
#[derive(Debug, Clone)]
pub struct ForkView {
    /// `ns/name` of the parent, if the viewer can read it.
    pub parent: Option<String>,
    /// The parent was deleted.
    pub parent_deleted: bool,
    /// `queued`, `running`, `done`, or `failed`.
    pub state: String,
    pub error: Option<String>,
    /// The most it may be seen by.
    pub max_visibility: Visibility,
    /// The viewer can open merge requests from this fork into the parent.
    pub contribute: bool,
}

impl Services {
    /// Forks a repository the subject can read (and that allows forks) into
    /// a namespace where they can create repositories.
    pub async fn fork_repo(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        input: ForkInput,
    ) -> Result<RepoAccess> {
        let parent = self
            .repo_access(subject, ns, repo, Action::RepoFork)
            .await?;
        let Subject::User { id: by, .. } = subject else {
            return Err(ServiceError::Unauthenticated);
        };
        let cap = parent.repo.visibility;
        let visibility = input.visibility.unwrap_or(cap);
        if rank(visibility) > rank(cap) {
            return Err(ServiceError::invalid(
                "visibility",
                "fork",
                format!(
                    "A fork of a {} repository can't be more visible than that.",
                    slath_db::repos::visibility_str(cap)
                ),
            ));
        }
        let name = match input.name.trim() {
            "" => parent.repo.name.clone(),
            n => n.to_owned(),
        };
        if let Some(ns_id) = self.can_create_in(subject, &input.namespace).await?
            && slath_db::repos::name_in_use(&self.pool, ns_id, &name).await?
        {
            let mut k = 1;
            let free = loop {
                let candidate = format!("{name}-{k}");
                if !slath_db::repos::name_in_use(&self.pool, ns_id, &candidate).await? || k > 99 {
                    break candidate;
                }
                k += 1;
            };
            return Err(ServiceError::invalid(
                "name",
                "taken",
                format!("A repository with this name already exists; how about {free}?"),
            ));
        }
        let plan = ForkPlan {
            parent: parent.repo.id,
            default_branch_only: input.default_branch_only,
            max_visibility: cap,
            by: *by,
        };
        let a = self
            .create_repo_as(
                subject,
                NewRepoInput {
                    namespace: input.namespace,
                    name,
                    description: parent.repo.description.clone().unwrap_or_default(),
                    visibility,
                    default_branch: parent.repo.default_branch.clone(),
                },
                Some(&plan),
            )
            .await?;
        let target = self.repo_target(&a.repo).await;
        self.audit(
            subject,
            "repo.fork",
            target,
            serde_json::json!({
                "parent": format!("{}/{}", parent.repo.namespace, parent.repo.name),
                "default_branch_only": input.default_branch_only,
            }),
        )
        .await;
        Ok(a)
    }

    /// Whether the repository is a fork, and of what.
    pub async fn fork_status(&self, subject: &Subject, a: &RepoAccess) -> Result<Option<ForkView>> {
        let Some(f) = slath_db::forks::get(&self.pool, a.repo.id).await? else {
            return Ok(None);
        };
        let mut contribute = false;
        let (parent, parent_deleted) = match f.parent_id {
            None => (None, true),
            Some(p) => match self.repo_access_by_id(subject, p, Action::RepoRead).await {
                Ok(pa) => {
                    contribute = a.can(subject, Action::RepoPush)
                        && pa.can(subject, Action::MrCreateFork)
                        && self.mrs_enabled(&pa).await;
                    (
                        Some(format!("{}/{}", pa.repo.namespace, pa.repo.name)),
                        false,
                    )
                }
                Err(_) => (
                    None,
                    slath_db::repos::get(&self.pool, p)
                        .await?
                        .is_none_or(|r| r.state == RepoState::Deleting),
                ),
            },
        };
        Ok(Some(ForkView {
            parent,
            parent_deleted,
            state: f.state,
            error: f.error,
            max_visibility: f.max_visibility,
            contribute,
        }))
    }

    /// The direct forks of a repository that the subject can read (up to
    /// 200), newest first.
    pub async fn list_forks(
        &self,
        subject: &Subject,
        a: &RepoAccess,
    ) -> Result<Vec<slath_db::repos::Repo>> {
        self.authorize(subject, a, Action::RepoRead)?;
        let mut out = Vec::new();
        for id in slath_db::forks::children(&self.pool, a.repo.id, 200).await? {
            if let Ok(f) = self.repo_access_by_id(subject, id, Action::RepoRead).await {
                out.push(f.repo);
            }
        }
        Ok(out)
    }

    /// Where the subject may fork a repository to: their own namespace and
    /// their organizations', without the repository's own namespace (a copy
    /// next to the original isn't worth offering; a branch does the job).
    /// Empty if they may not fork it at all.
    pub async fn fork_targets(&self, subject: &Subject, a: &RepoAccess) -> Result<Vec<String>> {
        let Subject::User { id, .. } = subject else {
            return Ok(Vec::new());
        };
        if !a.can(subject, Action::RepoFork) {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        if let Some(u) = slath_db::users::get(&self.pool, *id).await? {
            out.push(u.name);
        }
        for (org, _) in slath_db::orgs::for_user(&self.pool, *id).await? {
            out.push(org);
        }
        out.retain(|n| *n != a.repo.namespace);
        Ok(out)
    }

    /// The most a repository may be seen by: its cap if it's a fork.
    pub async fn max_visibility(&self, a: &RepoAccess) -> Result<Visibility> {
        Ok(slath_db::forks::get(&self.pool, a.repo.id)
            .await?
            .map_or(Visibility::Public, |f| f.max_visibility))
    }

    /// The parent of a finished fork, if the subject can read it.
    async fn readable_parent(
        &self,
        subject: &Subject,
        a: &RepoAccess,
    ) -> Result<Option<RepoAccess>> {
        let Some(f) = slath_db::forks::get(&self.pool, a.repo.id).await? else {
            return Ok(None);
        };
        let Some(p) = f.parent_id.filter(|_| f.state == "done") else {
            return Ok(None);
        };
        Ok(self
            .repo_access_by_id(subject, p, Action::RepoRead)
            .await
            .ok())
    }

    /// How a fork's `branch` compares with the parent's, if both have it
    /// and the subject can read the parent.
    pub async fn fork_compare(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        branch: &str,
    ) -> Result<Option<BranchCompare>> {
        self.authorize(subject, a, Action::RepoRead)?;
        let Some(p) = self.readable_parent(subject, a).await? else {
            return Ok(None);
        };
        let Ok(name) = slath_git::args::RefName::branch(branch.as_bytes()) else {
            return Ok(None);
        };
        let (fork_dir, parent_dir) = (self.repo_path(a.repo.id), self.repo_path(p.repo.id));
        let read = |dir: std::path::PathBuf| {
            let name = name.clone();
            async move { slath_git::refs::read(&self.runner, &dir, &name).await }
        };
        let (Some(ours), Some(theirs)) = (
            read(fork_dir.clone())
                .await
                .map_err(ServiceError::internal)?,
            read(parent_dir.clone())
                .await
                .map_err(ServiceError::internal)?,
        ) else {
            return Ok(None);
        };
        let (ahead, behind) =
            slath_git::forks::ahead_behind(&self.runner, &fork_dir, &parent_dir, &ours, &theirs)
                .await
                .map_err(ServiceError::internal)?;
        Ok(Some(BranchCompare {
            parent: format!("{}/{}", p.repo.namespace, p.repo.name),
            branch: branch.to_owned(),
            ahead,
            behind,
        }))
    }

    /// Brings a fork's `branch` up to the parent's: a fast-forward (or the
    /// new branch). If both have their own commits, nothing changes unless
    /// `discard` (sudo for browsers) drops the fork's. Branch protection of
    /// the fork applies as for a push.
    pub async fn sync_fork(
        &self,
        subject: &Subject,
        (ns, repo): (&str, &str),
        branch: &str,
        discard: bool,
        sudo: bool,
    ) -> Result<Synced> {
        let a = self
            .repo_access(subject, ns, repo, Action::RepoPush)
            .await?;
        let Subject::User { id: user, .. } = subject else {
            return Err(ServiceError::Unauthenticated);
        };
        if slath_db::forks::get(&self.pool, a.repo.id)
            .await?
            .is_some_and(|f| f.in_progress())
        {
            return Err(ServiceError::Conflict(
                "The fork is still being created.".into(),
            ));
        }
        let Some(p) = self.readable_parent(subject, &a).await? else {
            return Err(ServiceError::invalid(
                "branch",
                "no_parent",
                "This isn't a fork of a repository you can read.",
            ));
        };
        let name = slath_git::args::RefName::branch(branch.trim().as_bytes())
            .map_err(|_| ServiceError::invalid("branch", "invalid", "Choose a branch."))?;
        let rules = slath_db::protections::list(&self.pool, a.repo.id).await?;
        let rule = crate::protections::effective(&rules, &name.as_bytes()[b"refs/heads/".len()..]);
        let role = a.ctx.role;
        if rule.protected
            && (rule.nobody
                || rule.require_mr
                || rule
                    .push_role
                    .is_some_and(|min| role.is_none_or(|r| r < min)))
        {
            return Err(ServiceError::invalid(
                "branch",
                "protected",
                "This branch is protected in your fork; change it with a push or merge request.",
            ));
        }
        let dir = self.repo_path(a.repo.id);
        let Some(tip) = slath_git::forks::fetch_parent_branch(
            &self.runner,
            &dir,
            &self.repo_path(p.repo.id),
            &name,
        )
        .await
        .map_err(ServiceError::internal)?
        else {
            return Err(ServiceError::invalid(
                "branch",
                "missing",
                format!(
                    "{}/{} has no branch {branch}.",
                    p.repo.namespace, p.repo.name
                ),
            ));
        };
        let ours = slath_git::refs::read(&self.runner, &dir, &name)
            .await
            .map_err(ServiceError::internal)?;
        let result = match &ours {
            None => Synced::Created,
            Some(o) if *o == tip => return Ok(Synced::UpToDate),
            Some(o) => {
                if slath_git::forks::is_ancestor(&self.runner, &dir, o, &tip)
                    .await
                    .map_err(ServiceError::internal)?
                {
                    Synced::FastForwarded
                } else if discard {
                    crate::repos::require_sudo_for_sessions(subject, sudo)?;
                    if rule.block_force_push {
                        return Err(ServiceError::invalid(
                            "branch",
                            "protected",
                            "Force updates of this branch are blocked in your fork.",
                        ));
                    }
                    Synced::Discarded
                } else {
                    let (ahead, behind) =
                        slath_git::forks::ahead_behind(&self.runner, &dir, &dir, o, &tip)
                            .await
                            .map_err(ServiceError::internal)?;
                    return Ok(Synced::Diverged { ahead, behind });
                }
            }
        };
        match slath_git::refs::update_cas(&self.runner, &dir, &name, Some(&tip), ours.as_ref())
            .await
        {
            Ok(()) => {}
            Err(slath_git::refs::RefUpdateError::Conflict { .. }) => {
                return Err(ServiceError::Conflict(
                    "The branch moved meanwhile; try again.".into(),
                ));
            }
            Err(e) => return Err(ServiceError::internal(e)),
        }
        let zero = "0".repeat(40);
        self.record_push(
            a.repo.id,
            Some(*user),
            "internal",
            &[slath_git::ipc::RefUpdate {
                name: name.as_bytes().to_vec(),
                old: ours.as_ref().map_or(zero, |o| o.as_str().to_owned()),
                new: tip.as_str().to_owned(),
            }],
        )
        .await?;
        let target = self.repo_target(&a.repo).await;
        self.audit(
            subject,
            "repo.sync",
            target,
            serde_json::json!({"branch": branch, "result": result.as_str()}),
        )
        .await;
        Ok(result)
    }

    /// Queues a review of the forks of `repo` after access to it changed.
    /// Failing to queue is logged; it never fails the change itself.
    pub(crate) async fn review_forks(&self, repo: RepoId) {
        let job = slath_db::jobs::NewJob::new(
            "default",
            FORK_ACCESS_REVIEW,
            serde_json::json!({"repo_id": repo.to_string()}),
        )
        .dedupe_key(format!(
            "{FORK_ACCESS_REVIEW}:{repo}:{}",
            uuid::Uuid::now_v7()
        ));
        if let Err(e) = slath_db::jobs::enqueue(&self.pool, &job).await {
            tracing::warn!(error = %e, repo = %repo, "queueing a fork access review failed");
        }
    }

    /// [`Self::review_forks`] for every repository of a namespace that has
    /// forks (after a membership change).
    pub(crate) async fn review_forks_in(&self, namespace: slath_core::ids::NamespaceId) {
        match slath_db::forks::parents_in(&self.pool, namespace).await {
            Ok(ids) => {
                for id in ids {
                    self.review_forks(id).await;
                }
            }
            Err(e) => tracing::warn!(error = %e, "listing repositories with forks failed"),
        }
    }

    /// [`Self::review_forks_in`] for an organization's repositories.
    pub(crate) async fn review_org_forks(&self, org: slath_core::ids::OrgId) {
        match slath_db::orgs::get(&self.pool, org).await {
            Ok(Some(o)) => self.review_forks_in(o.namespace_id).await,
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "loading an organization failed"),
        }
    }

    /// The `fork.access_review` job: a fork of a repository that wasn't
    /// public when it was forked is locked when its owner (the forker, for
    /// a fork in an organization) can no longer read the repository, and
    /// unlocked when they can again. Locking a fork reviews its own forks.
    pub async fn run_access_review(&self, job: &LeasedJob) -> std::result::Result<(), JobError> {
        let parent: RepoId = job
            .payload
            .get("repo_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| JobError::Permanent("payload has no valid repo_id".to_owned()))?;
        let retry = |e: slath_db::DbError| JobError::Retry(e.to_string());
        let Some(p) = slath_db::repos::get(&self.pool, parent)
            .await
            .map_err(retry)?
            .filter(|r| r.state != RepoState::Deleting)
        else {
            // A deleted parent leaves its forks as they are.
            return Ok(());
        };
        for child in slath_db::forks::children(&self.pool, p.id, i64::MAX)
            .await
            .map_err(retry)?
        {
            let (Some(fork), Some(repo)) = (
                slath_db::forks::get(&self.pool, child)
                    .await
                    .map_err(retry)?,
                slath_db::repos::get(&self.pool, child)
                    .await
                    .map_err(retry)?,
            ) else {
                continue;
            };
            if fork.max_visibility == Visibility::Public {
                continue;
            }
            let owner = match slath_db::orgs::resolve_namespace(&self.pool, &repo.namespace)
                .await
                .map_err(retry)?
            {
                Some((_, _, slath_db::orgs::NamespaceKind::User(u))) => Some(u),
                _ => fork.requested_by,
            };
            let can_read = match owner {
                Some(u) => match slath_db::users::get(&self.pool, u).await.map_err(retry)? {
                    Some(row) => self
                        .repo_access_by_id(
                            &crate::notifications::user_subject(u, &row),
                            p.id,
                            Action::RepoRead,
                        )
                        .await
                        .is_ok(),
                    None => false,
                },
                None => false,
            };
            let to = match (repo.state, can_read) {
                (RepoState::Locked, true) => RepoState::Active,
                (RepoState::Active | RepoState::Archived, false) => RepoState::Locked,
                _ => continue,
            };
            slath_db::repos::set_state(&self.pool, child, to)
                .await
                .map_err(retry)?;
            let target = self.repo_target(&repo).await;
            self.audit(
                &Subject::System,
                if to == RepoState::Locked {
                    "repo.lock"
                } else {
                    "repo.unlock"
                },
                target,
                serde_json::json!({"reason": "parent_access", "parent": format!("{}/{}", p.namespace, p.name)}),
            )
            .await;
            self.review_forks(child).await;
        }
        Ok(())
    }

    /// The `repo.fork` job.
    pub async fn run_fork(&self, job: &LeasedJob) -> std::result::Result<(), JobError> {
        let id: RepoId = job
            .payload
            .get("repo_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| JobError::Permanent("payload has no valid repo_id".to_owned()))?;
        let retry = |e: slath_db::DbError| JobError::Retry(e.to_string());
        let Some(fork) = slath_db::forks::get(&self.pool, id).await.map_err(retry)? else {
            return Ok(());
        };
        let Some(repo) = slath_db::repos::get(&self.pool, id).await.map_err(retry)? else {
            return Ok(());
        };
        if !fork.in_progress() || repo.state == RepoState::Deleting {
            return Ok(());
        }
        let fail = |message: &'static str| async move {
            slath_db::forks::set_state(&self.pool, id, "failed", Some(message))
                .await
                .map_err(retry)
        };
        slath_db::forks::set_state(&self.pool, id, "running", None)
            .await
            .map_err(retry)?;
        let parent = match fork.parent_id {
            Some(p) => slath_db::repos::get(&self.pool, p)
                .await
                .map_err(retry)?
                .filter(|r| r.state != RepoState::Deleting),
            None => None,
        };
        let Some(parent) = parent else {
            return fail("The repository you forked was deleted before it was copied.").await;
        };
        let branch = fork
            .default_branch_only
            .then(|| parent.default_branch.clone());
        if let Err(e) = slath_git::storage::fork_clone(
            &self.runner,
            &self.config.storage.data_dir,
            &self.repo_path(parent.id),
            id,
            branch.as_deref(),
        )
        .await
        {
            tracing::error!(error = %e, repo = %id, parent = %parent.id, "copying a fork failed");
            return fail("Copying the repository failed.").await;
        }
        let dir = self.repo_path(id);
        if let Err(e) = slath_git::storage::set_head(
            &self.runner,
            &dir,
            &format!("refs/heads/{}", repo.default_branch),
        )
        .await
        {
            tracing::warn!(error = %e, repo = %id, "setting a fork's HEAD failed");
        }
        // The database's view of the refs follows Git.
        if let Err(e) = self.reconcile_repo(id, true).await {
            tracing::warn!(error = %e, repo = %id, "recording a fork's refs failed");
        }
        slath_db::forks::set_state(&self.pool, id, "done", None)
            .await
            .map_err(retry)?;
        // The original's webhooks hear about forks their principal can see.
        let base = self
            .config
            .server
            .public_url
            .origin()
            .trim_end_matches('/')
            .to_owned();
        self.emit_webhook(
            parent.id,
            fork.requested_by,
            ("fork", Some("created")),
            crate::webhooks::Needs::Also(id),
            serde_json::json!({"fork": {
                "full_name": format!("{}/{}", repo.namespace, repo.name),
                "visibility": repo.visibility,
                "html_url": format!("{base}/{}/{}", repo.namespace, repo.name),
            }}),
        )
        .await;
        Ok(())
    }
}
