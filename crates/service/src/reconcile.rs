// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reconciler: Git is the source of truth for refs. It compares each
//! repository's refs with the database snapshot and records every
//! difference as a push with transport `reconcile`, which also updates the
//! snapshot and triggers the usual follow-up work.

use std::collections::BTreeMap;

use slath_core::authz::RepoState;
use slath_core::ids::{MergeRequestId, RepoId};
use slath_git::ipc::RefUpdate;

use crate::Services;
use crate::error::{Result, ServiceError};

const ZERO: &str = "0000000000000000000000000000000000000000";

/// What the check found for one repository.
#[derive(Debug, Clone, Default)]
pub struct RepoCheck {
    pub repo_id: Option<RepoId>,
    /// `namespace/name`, or the directory for orphans.
    pub name: String,
    /// Refs that differ between Git and the snapshot.
    pub ref_differences: Vec<RefUpdate>,
    /// The repository directory is missing.
    pub missing_on_disk: bool,
    /// A directory under `repos/` that belongs to no repository.
    pub orphan: bool,
    /// Merge request revisions whose pin ref was missing or wrong while
    /// the commit still exists (restored with `--reconcile`).
    pub unpinned_revisions: usize,
    /// Merge request revisions whose head commit is gone from Git.
    pub missing_revisions: Vec<(MergeRequestId, i32)>,
    /// Differences were recorded (with `--reconcile`).
    pub repaired: bool,
}

impl RepoCheck {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.ref_differences.is_empty()
            && !self.missing_on_disk
            && !self.orphan
            && self.unpinned_revisions == 0
            && self.missing_revisions.is_empty()
    }
}

impl Services {
    /// Lists a repository's branches and tags from Git as `name → oid`.
    /// Refs the server keeps for itself (`refs/slath/`, merge request
    /// heads) aren't part of the snapshot.
    async fn git_refs(&self, id: RepoId) -> Result<BTreeMap<Vec<u8>, String>> {
        let out = self
            .runner
            .repo(&self.repo_path(id))
            .args([
                "for-each-ref",
                "--format=%(objectname) %(refname)",
                "refs/heads/",
                "refs/tags/",
            ])
            .run()
            .await
            .map_err(ServiceError::internal)?;
        let mut refs = BTreeMap::new();
        for line in out.stdout.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let Some(sp) = line.iter().position(|&b| b == b' ') else {
                continue;
            };
            let oid = String::from_utf8_lossy(&line[..sp]).into_owned();
            refs.insert(line[sp + 1..].to_vec(), oid);
        }
        Ok(refs)
    }

    /// Compares one repository with its snapshot; with `repair`, records the
    /// differences (transport `reconcile`).
    pub async fn reconcile_repo(&self, id: RepoId, repair: bool) -> Result<RepoCheck> {
        let repo = slath_db::repos::get(&self.pool, id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        let mut check = RepoCheck {
            repo_id: Some(id),
            name: format!("{}/{}", repo.namespace, repo.name),
            ..RepoCheck::default()
        };
        if repo.state == RepoState::Deleting {
            return Ok(check);
        }
        if !self.repo_path(id).join("HEAD").is_file() {
            check.missing_on_disk = true;
            return Ok(check);
        }
        let git: BTreeMap<Vec<u8>, String> = self.git_refs(id).await?;
        let db: BTreeMap<Vec<u8>, String> = slath_db::refs::snapshot(&self.pool, id)
            .await?
            .into_iter()
            .collect();
        for (name, oid) in &git {
            if db.get(name) != Some(oid) {
                check.ref_differences.push(RefUpdate {
                    old: db.get(name).cloned().unwrap_or_else(|| ZERO.to_owned()),
                    new: oid.clone(),
                    name: name.clone(),
                });
            }
        }
        for (name, oid) in &db {
            if !git.contains_key(name) {
                check.ref_differences.push(RefUpdate {
                    old: oid.clone(),
                    new: ZERO.to_owned(),
                    name: name.clone(),
                });
            }
        }
        if repair && !check.ref_differences.is_empty() {
            self.record_push(id, None, "reconcile", &check.ref_differences)
                .await?;
            check.repaired = true;
        }
        self.check_revision_pins(id, repair, &mut check).await?;
        Ok(check)
    }

    /// Every merge request revision must be pinned by its ref. A missing
    /// pin is recreated if the commit still exists; otherwise the revision
    /// is flagged as missing its objects.
    async fn check_revision_pins(
        &self,
        id: RepoId,
        repair: bool,
        check: &mut RepoCheck,
    ) -> Result<()> {
        use slath_git::refs;
        let path = self.repo_path(id);
        for rev in slath_db::merge_requests::revision_heads(&self.pool, id).await? {
            let (Ok(head), Ok(seq)) = (
                slath_git::ObjectId::parse(&rev.head_oid),
                u32::try_from(rev.seq),
            ) else {
                continue;
            };
            let pin = refs::mr_revision_pin(rev.mr_id, seq);
            let pinned = refs::read(&self.runner, &path, &pin)
                .await
                .map_err(ServiceError::internal)?;
            if pinned.as_ref() == Some(&head) {
                if rev.missing_objects && repair {
                    slath_db::merge_requests::set_revision_missing(
                        &self.pool, rev.mr_id, rev.seq, false,
                    )
                    .await?;
                }
                continue;
            }
            if refs::commit_exists(&self.runner, &path, &head)
                .await
                .map_err(ServiceError::internal)?
            {
                check.unpinned_revisions += 1;
                if repair {
                    refs::set(&self.runner, &path, &pin, &head)
                        .await
                        .map_err(ServiceError::internal)?;
                    check.repaired = true;
                }
            } else {
                check.missing_revisions.push((rev.mr_id, rev.seq));
                if repair && !rev.missing_objects {
                    tracing::error!(repo = %check.name, mr = %rev.mr_id, seq = rev.seq, "a merge request revision's commits are missing");
                    slath_db::merge_requests::set_revision_missing(
                        &self.pool, rev.mr_id, rev.seq, true,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// Checks every repository (or one) and finds directories that belong
    /// to no repository.
    pub async fn fsck(&self, only: Option<RepoId>, repair: bool) -> Result<Vec<RepoCheck>> {
        let ids = match only {
            Some(id) => vec![id],
            None => slath_db::repos::all_ids(&self.pool).await?,
        };
        let mut out = Vec::new();
        for id in &ids {
            out.push(self.reconcile_repo(*id, repair).await?);
        }
        if only.is_none() {
            let known: std::collections::HashSet<String> =
                ids.iter().map(ToString::to_string).collect();
            let root = self.config.storage.data_dir.join("repos");
            if let Ok(shards) = std::fs::read_dir(&root) {
                for shard in shards.flatten().filter(|s| s.file_name() != ".tmp") {
                    for entry in std::fs::read_dir(shard.path())
                        .into_iter()
                        .flatten()
                        .flatten()
                    {
                        let file = entry.file_name().to_string_lossy().into_owned();
                        let id = file.trim_end_matches(".git");
                        if !known.contains(id) {
                            out.push(RepoCheck {
                                name: entry.path().display().to_string(),
                                orphan: true,
                                ..RepoCheck::default()
                            });
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Reconciles repositories pushed to since `since` (startup) or all
    /// active ones; returns the number of repaired repositories.
    pub async fn reconcile_recent(&self, since: Option<time::OffsetDateTime>) -> Result<usize> {
        let ids = slath_db::repos::active_ids_pushed_since(&self.pool, since).await?;
        let mut repaired = 0;
        for id in ids {
            match self.reconcile_repo(id, true).await {
                Ok(c) if c.repaired => {
                    tracing::warn!(repo = %c.name, refs = c.ref_differences.len(), "reconciler repaired refs changed outside Slath");
                    repaired += 1;
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, repo = %id, "reconciling a repository failed"),
            }
        }
        Ok(repaired)
    }
}
