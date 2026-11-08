// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Commit statuses: reports by authenticated reporters on a commit.

use slath_core::ids::{RepoId, UserId};
use sqlx::PgExecutor;

use crate::DbError;

/// One reported status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub id: uuid::Uuid,
    pub commit_oid: String,
    pub context: String,
    /// `pending`, `success`, `failure`, or `error`.
    pub state: String,
    pub description: Option<String>,
    pub target_url: Option<String>,
    /// The reporting user or bot.
    pub reporter_id: Option<UserId>,
    pub reporter: Option<String>,
    pub created_at: time::OffsetDateTime,
}

/// What a new status records.
#[derive(Debug)]
pub struct NewStatus<'a> {
    pub repo: RepoId,
    pub commit_oid: &'a str,
    pub context: &'a str,
    pub state: &'a str,
    pub description: Option<&'a str>,
    pub target_url: Option<&'a str>,
    pub reporter: UserId,
}

pub async fn insert(exec: impl PgExecutor<'_>, s: &NewStatus<'_>) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO commit_statuses (id, repo_id, commit_oid, context, state, description, target_url,
             creator_kind, creator_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'user', $8)",
        id,
        s.repo.as_uuid(),
        s.commit_oid,
        s.context,
        s.state,
        s.description,
        s.target_url,
        s.reporter.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// How many statuses a commit has for a context.
pub async fn count(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    oid: &str,
    context: &str,
) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM commit_statuses WHERE repo_id = $1 AND commit_oid = $2 AND context = $3"#,
        repo.as_uuid(),
        oid,
        context
    )
    .fetch_one(exec)
    .await?)
}

/// Every status of a commit, newest first.
pub async fn for_commit(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    oid: &str,
) -> Result<Vec<Status>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT s.id, s.commit_oid, s.context, s.state, s.description, s.target_url, s.creator_id,
                  n.name::text AS reporter, s.created_at
           FROM commit_statuses s
           LEFT JOIN users u ON u.id = s.creator_id AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE s.repo_id = $1 AND s.commit_oid = $2
           ORDER BY s.created_at DESC, s.id DESC"#,
        repo.as_uuid(),
        oid
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Status {
            id: r.id,
            commit_oid: r.commit_oid,
            context: r.context,
            state: r.state,
            description: r.description,
            target_url: r.target_url,
            reporter_id: r.creator_id.map(UserId::from_uuid),
            reporter: r.reporter,
            created_at: r.created_at,
        })
        .collect())
}
