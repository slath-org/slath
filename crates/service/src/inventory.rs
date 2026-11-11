// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Diff inventories (D-54 item 1) for merge request revisions, change
//! versions, and commits: every file in exactly one class, with the limits
//! named and the files left out counted. The same function serves the API
//! and the diff views, so they agree.

use slath_core::authz::Subject;
use slath_core::ids::RepoId;
use slath_git::ObjectId;
use slath_git::inventory::Inventory;

use crate::Services;
use crate::error::{Result, ServiceError};

/// The largest raw diff read for an inventory (as for the diff views).
const MAX_DIFF_BYTES: usize = 64 * 1024 * 1024;

impl Services {
    /// The inventory of `base..head` in `repo` (from the empty tree without
    /// a base); generated files are read from `head`'s `.gitattributes`.
    pub async fn diff_inventory(
        &self,
        repo: RepoId,
        base: Option<&ObjectId>,
        head: &ObjectId,
    ) -> Result<Inventory> {
        let dir = self.repo_path(repo);
        let (raw, cut) = slath_git::browse::diff(
            &self.runner,
            &dir,
            base,
            head,
            slath_git::browse::DiffOptions::default(),
            MAX_DIFF_BYTES,
        )
        .await
        .map_err(ServiceError::internal)?;
        let limits = &self.config.limits;
        let diff = slath_git::diff::parse(&raw, limits.diff_max_files, limits.diff_max_lines);
        let total = if cut {
            slath_git::browse::changed_file_count(&self.runner, &dir, base, head)
                .await
                .map_err(ServiceError::internal)?
        } else {
            raw.split(|&b| b == b'\n')
                .filter(|l| l.starts_with(b"diff --git "))
                .count()
        };
        let paths: Vec<Vec<u8>> = diff.files.iter().map(|f| f.path().to_vec()).collect();
        let generated = slath_git::browse::generated_paths(&self.runner, &dir, head, &paths)
            .await
            .unwrap_or_default();
        Ok(slath_git::inventory::inventory(
            &diff,
            total,
            limits.diff_max_files,
            &generated,
        ))
    }

    /// The inventory of a merge request's revision `revision` (default:
    /// the current one), or of the change version `commit` in it.
    pub async fn mr_inventory(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        revision: Option<i32>,
        commit: Option<&str>,
    ) -> Result<Inventory> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        let seq = revision.unwrap_or(m.revisions);
        let revs = slath_db::merge_requests::revisions(&self.pool, m.id).await?;
        let r = revs
            .iter()
            .find(|r| r.seq == seq)
            .ok_or(ServiceError::NotFound)?;
        let parse = |s: &str| ObjectId::parse(s).map_err(ServiceError::internal);
        let (base, head) = match commit {
            Some(c) => {
                let v = slath_db::mr_changes::versions(&self.pool, m.id)
                    .await?
                    .into_iter()
                    .find(|v| v.revision_seq == seq && v.commit_oid == c)
                    .ok_or(ServiceError::NotFound)?;
                (
                    v.parent_oid.as_deref().map(parse).transpose()?,
                    parse(&v.commit_oid)?,
                )
            }
            None => (
                r.merge_base_oid.as_deref().map(parse).transpose()?,
                parse(&r.head_oid)?,
            ),
        };
        self.diff_inventory(a.repo.id, base.as_ref(), &head).await
    }
}
