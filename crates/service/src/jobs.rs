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
