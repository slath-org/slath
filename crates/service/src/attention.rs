// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The attention gate: a bot's merge request starts `iterating` and keeps
//! its requested reviewers' attention until it's marked ready. With
//! `ready_waits_for_checks` (the default) it then waits until every
//! required check passes on its head (`waiting_for_checks`). Each status
//! and each new revision queues a check (job `mr.attention`); the move to
//! `ready` happens in one statement, so the reviewers hear of it once.

use slath_core::ids::{MergeRequestId, RepoId};
use slath_db::merge_requests::MergeRequest;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::notifications::MrActivity;
use crate::protections::Effective;

/// The job that checks whether a waiting merge request is ready.
pub const MR_ATTENTION: &str = "mr.attention";

impl Services {
    /// Whether every required check of `eff` is `success` on the merge
    /// request's head, counted as for merging but whether or not the
    /// head's statuses count for the commit that lands.
    pub(crate) async fn checks_pass_on_head(
        &self,
        m: &MergeRequest,
        eff: &Effective,
    ) -> Result<bool> {
        if eff.required_checks.is_empty() {
            return Ok(true);
        }
        if m.head_oid.is_none() {
            return Ok(false);
        }
        Ok(self
            .check_requirements(m, eff, true)
            .await?
            .iter()
            .all(|r| r.state == "met"))
    }

    /// Queues a check of a merge request, if it's waiting for its checks.
    pub(crate) async fn queue_attention(&self, id: MergeRequestId) {
        match slath_db::merge_requests::get_by_id(&self.pool, id).await {
            Ok(Some(m)) if m.state == "open" && m.attention == "waiting_for_checks" => {}
            _ => return,
        }
        let job = slath_db::jobs::NewJob::new(
            "default",
            MR_ATTENTION,
            serde_json::json!({"mr_id": id.to_string()}),
        )
        .dedupe_key(format!("{MR_ATTENTION}:{}", uuid::Uuid::now_v7()));
        if let Err(e) = slath_db::jobs::enqueue(&self.pool, &job).await {
            tracing::warn!(error = %e, mr = %id, "queueing an attention check failed");
        }
    }

    /// Queues checks of every merge request of a repository that waits for
    /// its checks (after a status).
    pub(crate) async fn queue_attention_in(&self, repo: RepoId) {
        match slath_db::merge_requests::waiting_for_checks(&self.pool, repo).await {
            Ok(ids) => {
                for id in ids {
                    self.queue_attention(id).await;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "finding merge requests waiting for checks failed")
            }
        }
    }

    /// The `mr.attention` job.
    pub(crate) async fn run_attention(
        &self,
        job: &slath_db::jobs::LeasedJob,
    ) -> std::result::Result<(), slath_db::worker::JobError> {
        use slath_db::worker::JobError;
        let id: MergeRequestId = job
            .payload
            .get("mr_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| JobError::Permanent("payload has no valid mr_id".to_owned()))?;
        self.release_attention(id)
            .await
            .map(|_| ())
            .map_err(|e| JobError::Retry(e.to_string()))
    }

    /// Makes a waiting merge request ready once its required checks pass
    /// on its head, and tells its reviewers. Returns whether it did.
    pub(crate) async fn release_attention(&self, id: MergeRequestId) -> Result<bool> {
        let Some(m) = slath_db::merge_requests::get_by_id(&self.pool, id).await? else {
            return Ok(false);
        };
        if m.state != "open" || m.attention != "waiting_for_checks" {
            return Ok(false);
        }
        let eff = self.effective_for(&m).await?;
        if eff.ready_waits_for_checks && !self.checks_pass_on_head(&m, &eff).await? {
            return Ok(false);
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        if !slath_db::merge_requests::set_attention(&mut *tx, id, &["waiting_for_checks"], "ready")
            .await?
        {
            return Ok(false);
        }
        slath_db::issues::insert_event(
            &mut *tx,
            m.repo_id,
            "merge_request",
            *id.as_uuid(),
            None,
            "attention_released",
            &serde_json::json!({"head": m.head_oid}),
        )
        .await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        let m = slath_db::merge_requests::get_by_id(&self.pool, id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        if !m.held() {
            self.notify_mr(None, &m, MrActivity::Ready).await;
        }
        Ok(true)
    }
}
