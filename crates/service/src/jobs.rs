// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Background job handlers and periodic maintenance.

use slath_core::ids::RepoId;
use slath_db::jobs::LeasedJob;
use slath_db::worker::{JobError, WorkerPool};
use time::OffsetDateTime;

use crate::Services;

/// Moves a deleted repository's directory into the trash.
pub const REPO_TRASH: &str = "repo.trash";
/// Follow-up work after refs changed (webhooks, merge requests, CI later).
pub const REF_UPDATED: &str = "ref.updated";
/// `git gc` and size accounting for one repository.
pub const REPO_MAINTENANCE: &str = "repo.maintenance";

/// Pushes after which a repository is maintained without waiting a day.
pub const MAINTENANCE_PUSHES: i32 = 50;
/// The longest a maintenance run may take; also its lease.
const MAINTENANCE_TIMEOUT_SECS: u32 = 3600;

fn repo_id(job: &LeasedJob) -> Result<RepoId, JobError> {
    job.payload
        .get("repo_id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| JobError::Permanent("payload has no valid repo_id".to_owned()))
}

impl Services {
    /// Registers every job handler on the worker pool.
    #[must_use]
    pub fn register_jobs(&self, pool: WorkerPool) -> WorkerPool {
        let s = self.clone();
        pool.handle(REPO_TRASH, move |job: LeasedJob| {
            let s = s.clone();
            async move { s.trash_repo(repo_id(&job)?).await }
        })
        .handle(REF_UPDATED, {
            let s = self.clone();
            move |job: LeasedJob| {
                let s = s.clone();
                async move { s.run_ref_updated(&job).await }
            }
        })
        .handle(crate::merge_requests::MR_SYNC, {
            let s = self.clone();
            move |job: LeasedJob| {
                let s = s.clone();
                async move { s.run_mr_sync(&job).await }
            }
        })
        .handle(crate::email::EMAIL_SEND, {
            let s = self.clone();
            move |job: LeasedJob| {
                let s = s.clone();
                async move { s.run_email_job(&job).await }
            }
        })
        .handle(crate::notifications::NOTIFY_FANOUT, {
            let s = self.clone();
            move |job: LeasedJob| {
                let s = s.clone();
                async move { s.run_fanout(&job).await }
            }
        })
        .handle(crate::forks::REPO_FORK, {
            let s = self.clone();
            move |job: LeasedJob| {
                let s = s.clone();
                async move { s.run_fork(&job).await }
            }
        })
        .handle(crate::imports::REPO_IMPORT, {
            let s = self.clone();
            move |job: LeasedJob| {
                let s = s.clone();
                async move { s.run_import(&job).await }
            }
        })
        .handle(REPO_MAINTENANCE, {
            let s = self.clone();
            move |job: LeasedJob| {
                let s = s.clone();
                async move { s.maintain_repo(repo_id(&job)?).await.map(|_| ()) }
            }
        })
    }

    /// The maintenance job for a repository. Its dedupe key includes the
    /// run counter, so at most one job per repository is pending at a time.
    pub(crate) fn maintenance_job(id: RepoId, runs: i32) -> slath_db::jobs::NewJob {
        slath_db::jobs::NewJob::new(
            "default",
            REPO_MAINTENANCE,
            serde_json::json!({"repo_id": id.to_string()}),
        )
        .dedupe_key(format!("{REPO_MAINTENANCE}:{id}:{runs}"))
    }

    /// Enqueues maintenance for repositories with pushes whose last run is
    /// more than a day old. Run periodically; returns the number enqueued.
    pub async fn schedule_maintenance(&self) -> Result<usize, crate::ServiceError> {
        let day_ago = OffsetDateTime::now_utc() - time::Duration::days(1);
        let due = slath_db::repos::maintenance_due(&self.pool, day_ago).await?;
        let mut n = 0;
        for (id, runs) in due {
            if slath_db::jobs::enqueue(&self.pool, &Self::maintenance_job(id, runs))
                .await?
                .is_some()
            {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Runs `git gc` on a repository and records its size. Never overlaps
    /// with another run on the same repository (a lease in the database);
    /// returns `false` if one is in progress. Git failures are logged and
    /// end the run, which is retried after the next pushes.
    pub async fn maintain_repo(&self, id: RepoId) -> Result<bool, JobError> {
        let Some(counted) =
            slath_db::repos::take_maintenance_lease(&self.pool, id, MAINTENANCE_TIMEOUT_SECS)
                .await
                .map_err(|e| JobError::Retry(e.to_string()))?
        else {
            return Ok(false);
        };
        let path = self.repo_path(id);
        let timeout = std::time::Duration::from_secs(u64::from(MAINTENANCE_TIMEOUT_SECS));
        if let Err(e) = slath_git::storage::maintain(&self.runner, &path, timeout).await {
            tracing::warn!(error = %e, repo = %id, "repository maintenance failed");
        }
        let size = match slath_git::storage::disk_usage(&self.runner, &path).await {
            Ok(b) => Some(i64::try_from(b).unwrap_or(i64::MAX)),
            Err(e) => {
                tracing::warn!(error = %e, repo = %id, "measuring the repository failed");
                None
            }
        };
        slath_db::repos::finish_maintenance(&self.pool, id, counted, size)
            .await
            .map_err(|e| JobError::Retry(e.to_string()))?;
        Ok(true)
    }

    async fn trash_repo(&self, id: RepoId) -> Result<(), JobError> {
        let live = self.repo_path(id);
        let trash = self.trash_path(id);
        if !live.exists() {
            return Ok(());
        }
        if let Some(p) = trash.parent() {
            tokio::fs::create_dir_all(p)
                .await
                .map_err(|e| JobError::Retry(e.to_string()))?;
        }
        tokio::fs::rename(&live, &trash)
            .await
            .map_err(|e| JobError::Retry(e.to_string()))
    }

    /// Purges repositories whose trash retention has passed. Run periodically.
    pub async fn purge_deleted_repos(&self) -> Result<usize, crate::ServiceError> {
        let days = i64::from(self.config.maintenance.deleted_repo_days);
        let cutoff = OffsetDateTime::now_utc() - time::Duration::days(days);
        let ids = slath_db::repos::deleted_before(&self.pool, cutoff).await?;
        for id in &ids {
            for p in [self.trash_path(*id), self.repo_path(*id)] {
                if p.exists() {
                    tokio::fs::remove_dir_all(&p)
                        .await
                        .map_err(crate::ServiceError::internal)?;
                }
            }
            slath_db::repos::purge(&self.pool, *id).await?;
        }
        Ok(ids.len())
    }
}
