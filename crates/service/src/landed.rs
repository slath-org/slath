// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merge requests that land by a push of their target branch: someone
//! merges or fast-forwards locally and pushes the target (`git push`, or
//! `jj git push` of the bookmark). When the merge request's head is then
//! part of the target, it's recorded as merged by the pusher with strategy
//! `external`, as if it had been merged in Slath.
//!
//! When the commits were rebased or cherry-picked first, each change of
//! the current revision lands on its own: when the target gained a commit
//! with the change's latest patch that names no other change of the merge
//! request. Once all have, the merge request is merged. A commit that names
//! a change but brings another patch only notes that the change possibly
//! landed with modifications; it never merges anything.

use std::collections::HashMap;

use slath_core::change_identity::{self, TargetCommit};
use slath_core::ids::{RepoId, UserId};
use slath_db::merge_requests::MergeRequest;
use slath_db::mr_changes as db;
use slath_git::ObjectId;
use slath_git::changes::StackCommit;
use slath_git::ipc::RefUpdate;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::mr_changes::{MAX_PATCH_BYTES, alias_of, aliases_of};
use crate::notifications::MrActivity;

/// At most this many new target commits are compared with the changes.
const MAX_TARGET_COMMITS: usize = 1000;

impl Services {
    /// Follow-up of a push (`ref.updated`): open merge requests into a
    /// branch the push moved whose commits that branch now contains. Runs
    /// before the push's source updates are handled, so pushing the target
    /// and deleting the source at once still counts as landing.
    pub(crate) async fn land_after_push(
        &self,
        repo: RepoId,
        pusher: Option<UserId>,
        updates: &[RefUpdate],
    ) -> Result<()> {
        for u in updates {
            let Some(branch) = u
                .name
                .strip_prefix(b"refs/heads/")
                .and_then(|b| std::str::from_utf8(b).ok())
            else {
                continue;
            };
            let Ok(new) = ObjectId::parse(&u.new) else {
                continue;
            };
            if u.new.bytes().all(|b| b == b'0') {
                continue;
            }
            let old = ObjectId::parse(&u.old)
                .ok()
                .filter(|o| !o.as_str().bytes().all(|b| b == b'0'));
            // The commits the target gained, read once for all its merge
            // requests that need them.
            let mut gained: Option<Vec<StackCommit>> = None;
            for id in slath_db::merge_requests::open_by_target(&self.pool, repo, branch).await? {
                let Some(before) = slath_db::merge_requests::get_by_id(&self.pool, id).await?
                else {
                    continue;
                };
                // The source may have moved in the same push: its revision
                // comes first, so what lands is what the merge request shows.
                let moved = before.source_repo_id == Some(repo)
                    && updates.iter().any(|s| s.name == before.source_ref);
                self.sync_mr(id, if moved { pusher } else { None }).await?;
                let Some(m) = slath_db::merge_requests::get_by_id(&self.pool, id).await? else {
                    continue;
                };
                let Some(head) = m.head_oid.as_deref().and_then(|h| ObjectId::parse(h).ok()) else {
                    continue;
                };
                if m.state == "open"
                    && slath_git::merge::is_ancestor(
                        &self.runner,
                        &self.repo_path(repo),
                        &head,
                        &new,
                    )
                    .await
                    .map_err(ServiceError::internal)?
                {
                    self.mark_landed(&m, pusher, &new).await?;
                } else if m.state == "open"
                    && let Some(old) = &old
                {
                    if gained.is_none() {
                        gained = Some(self.gained_commits(repo, old, &new).await?);
                    }
                    let targets = gained.as_deref().unwrap_or_default();
                    self.land_changes(&m, pusher, &new, targets).await?;
                }
            }
        }
        Ok(())
    }

    /// Records a merge request as merged by a push that moved its target
    /// to `target_head`.
    async fn mark_landed(
        &self,
        m: &MergeRequest,
        pusher: Option<UserId>,
        target_head: &ObjectId,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        // A merge in Slath may be finishing meanwhile; whichever comes
        // second finds the merge request no longer open.
        slath_db::merge_requests::lock_target(&mut *tx, m.repo_id, &m.target_branch).await?;
        if !slath_db::merge_requests::set_merged(
            &mut *tx,
            m.id,
            pusher,
            "external",
            target_head.as_str(),
        )
        .await?
        {
            return Ok(());
        }
        slath_db::mr_changes::set_merged(&mut *tx, m.id, m.revisions).await?;
        slath_db::issues::insert_event(
            &mut *tx,
            m.repo_id,
            "merge_request",
            *m.id.as_uuid(),
            pusher,
            "merged",
            &serde_json::json!({
                "strategy": "external",
                "oid": target_head.as_str(),
                "target": m.target_branch,
            }),
        )
        .await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        self.sync_mr_branch_by_id(m.id).await;
        self.notify_mr(pusher, m, MrActivity::Merged).await;
        self.remove_change_source(m).await;
        Ok(())
    }

    /// The commits without several parents in `old..new`, oldest first,
    /// at most [`MAX_TARGET_COMMITS`] (the newest).
    async fn gained_commits(
        &self,
        repo: RepoId,
        old: &ObjectId,
        new: &ObjectId,
    ) -> Result<Vec<StackCommit>> {
        let dir = self.repo_path(repo);
        let log =
            slath_git::browse::log_range(&self.runner, &dir, Some(old), new, MAX_TARGET_COMMITS)
                .await
                .map_err(ServiceError::internal)?;
        let oids: Vec<ObjectId> = log
            .into_iter()
            .filter(|c| c.parents.len() <= 1)
            .map(|c| c.oid)
            .collect();
        slath_git::changes::stack(&self.runner, &dir, &oids, MAX_PATCH_BYTES)
            .await
            .map_err(ServiceError::internal)
    }

    /// Marks the changes of the current revision that the target gained
    /// (`targets`) merged, and the merge request once all are.
    async fn land_changes(
        &self,
        m: &MergeRequest,
        pusher: Option<UserId>,
        new: &ObjectId,
        targets: &[StackCommit],
    ) -> Result<()> {
        if targets.is_empty() {
            return Ok(());
        }
        let versions = db::versions(&self.pool, m.id).await?;
        let current: Vec<&db::Version> = versions
            .iter()
            .filter(|v| v.revision_seq == m.revisions)
            .collect();
        // Reviewed as a whole, or a commit still unresolved: only the head
        // landing merges it.
        if current.is_empty() || current.iter().any(|v| v.change_id.is_none()) {
            return Ok(());
        }
        let changes: HashMap<uuid::Uuid, db::Change> = db::changes(&self.pool, m.id)
            .await?
            .into_iter()
            .map(|c| (c.id, c))
            .collect();
        let pending: Vec<(uuid::Uuid, Option<String>)> = current
            .iter()
            .filter_map(|v| Some((v.change_id?, v.patch_id.clone())))
            .filter(|(k, _)| changes.get(k).is_some_and(|c| c.state != "merged"))
            .collect();
        let bound: HashMap<_, uuid::Uuid> = db::aliases(&self.pool, m.id)
            .await?
            .into_iter()
            .filter_map(|a| Some((alias_of(&a.kind, &a.value)?, a.change_id)))
            .collect();
        let named: Vec<TargetCommit<uuid::Uuid>> = targets
            .iter()
            .map(|c| {
                let mut names: Vec<uuid::Uuid> = aliases_of(c)
                    .iter()
                    .filter_map(|a| bound.get(a).copied())
                    .collect();
                names.sort_unstable();
                names.dedup();
                TargetCommit {
                    names,
                    patch_id: c.patch_id.clone(),
                }
            })
            .collect();
        let landing = change_identity::landed(&pending, &named);
        if landing.merged.len() == pending.len() {
            return self.mark_landed(m, pusher, new).await;
        }
        if landing.merged.is_empty() && landing.modified.is_empty() {
            return Ok(());
        }
        let display = |k: &uuid::Uuid| changes.get(k).map(|c| c.display_id.clone());
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        slath_db::merge_requests::lock_target(&mut *tx, m.repo_id, &m.target_branch).await?;
        match slath_db::merge_requests::get_by_id(&mut *tx, m.id).await? {
            Some(now) if now.state == "open" && now.revisions == m.revisions => {}
            _ => return Ok(()),
        }
        if !landing.merged.is_empty() {
            db::set_changes_merged(&mut *tx, &landing.merged).await?;
            let ids: Vec<String> = landing.merged.iter().filter_map(display).collect();
            self.landed_event(
                &mut tx,
                m,
                pusher,
                "changes_landed",
                serde_json::json!({
                    "changes": ids,
                    "oid": new.as_str(),
                    "target": m.target_branch,
                }),
            )
            .await?;
        }
        for (k, i) in &landing.modified {
            self.landed_event(
                &mut tx,
                m,
                pusher,
                "possibly_landed",
                serde_json::json!({
                    "change": display(k),
                    "commit": targets.get(*i).map(|c| c.oid.as_str()),
                    "target": m.target_branch,
                }),
            )
            .await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        Ok(())
    }

    async fn landed_event(
        &self,
        tx: &mut sqlx::PgConnection,
        m: &MergeRequest,
        pusher: Option<UserId>,
        kind: &str,
        data: serde_json::Value,
    ) -> Result<()> {
        slath_db::issues::insert_event(
            &mut *tx,
            m.repo_id,
            "merge_request",
            *m.id.as_uuid(),
            pusher,
            kind,
            &data,
        )
        .await?;
        Ok(())
    }
}
