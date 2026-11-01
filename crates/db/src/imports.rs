// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Repository imports and their reports.

use slath_core::ids::{RepoId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

#[derive(Debug, Clone)]
pub struct Import {
    pub repo_id: RepoId,
    pub url: String,
    pub state: String,
    pub error: Option<String>,
    pub branches: Option<i32>,
    pub tags: Option<i32>,
    pub size_bytes: Option<i64>,
    pub created_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
}

pub async fn insert(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    url: &str,
    by: UserId,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO repo_imports (repo_id, url, requested_by) VALUES ($1, $2, $3)",
        repo.as_uuid(),
        url,
        by.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn get(exec: impl PgExecutor<'_>, repo: RepoId) -> Result<Option<Import>, DbError> {
    let r = sqlx::query!(
        "SELECT repo_id, url, state, error, branches, tags, size_bytes, created_at, finished_at
           FROM repo_imports WHERE repo_id = $1",
        repo.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| Import {
        repo_id: RepoId::from_uuid(r.repo_id),
        url: r.url,
        state: r.state,
        error: r.error,
        branches: r.branches,
        tags: r.tags,
        size_bytes: r.size_bytes,
        created_at: r.created_at,
        finished_at: r.finished_at,
    }))
}

pub async fn set_running(exec: impl PgExecutor<'_>, repo: RepoId) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE repo_imports SET state = 'running', started_at = now(), error = NULL WHERE repo_id = $1",
        repo.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn finish(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    branches: i32,
    tags: i32,
    size: i64,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE repo_imports SET state = 'done', branches = $2, tags = $3, size_bytes = $4,
                finished_at = now(), error = NULL WHERE repo_id = $1",
        repo.as_uuid(),
        branches,
        tags,
        size
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn fail(exec: impl PgExecutor<'_>, repo: RepoId, error: &str) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE repo_imports SET state = 'failed', error = $2, finished_at = now() WHERE repo_id = $1",
        repo.as_uuid(),
        error
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Removes the (encrypted) credentials from a finished job's payload.
pub async fn forget_job_credentials(exec: impl PgExecutor<'_>, job: i64) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE jobs SET payload = payload - 'credentials' - 'key_id' WHERE id = $1",
        job
    )
    .execute(exec)
    .await?;
    Ok(())
}
