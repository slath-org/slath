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
        let (parent, parent_deleted) = match f.parent_id {
            None => (None, true),
            Some(p) => match self.repo_access_by_id(subject, p, Action::RepoRead).await {
                Ok(pa) => (
                    Some(format!("{}/{}", pa.repo.namespace, pa.repo.name)),
                    false,
                ),
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
        }))
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
            .map_err(retry)
    }
}
