// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merge requests that land by a push of their target branch: someone
//! merges or fast-forwards locally and pushes the target (`git push`, or
//! `jj git push` of the bookmark). When the merge request's head is then
//! part of the target, it's recorded as merged by the pusher with strategy
//! `external`, as if it had been merged in Slath.

use slath_core::ids::{RepoId, UserId};
use slath_db::merge_requests::MergeRequest;
use slath_git::ObjectId;
use slath_git::ipc::RefUpdate;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::notifications::MrActivity;

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
        Ok(())
    }
}
