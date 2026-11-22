// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! A person's saved views of a repository's merge request list: a name and
//! the list's query string, never results.

use slath_core::ids::{RepoId, UserId};
use sqlx::PgExecutor;

use crate::DbError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedView {
    pub id: uuid::Uuid,
    pub name: String,
    /// The list's query string without `?`.
    pub query: String,
}

/// A user's saved views of a repository, by name.
pub async fn list(
    exec: impl PgExecutor<'_>,
    user: UserId,
    repo: RepoId,
) -> Result<Vec<SavedView>, DbError> {
    Ok(sqlx::query_as!(
        SavedView,
        "SELECT id, name, query FROM mr_saved_views WHERE user_id = $1 AND repo_id = $2 ORDER BY name",
        user.as_uuid(),
        repo.as_uuid()
    )
    .fetch_all(exec)
    .await?)
}

/// Saves a view, replacing the one of the same name; at most `most`
/// views per user and repository (a replacement always fits). `None` when
/// that many exist already.
pub async fn save(
    exec: impl PgExecutor<'_>,
    user: UserId,
    repo: RepoId,
    name: &str,
    query: &str,
    most: i64,
) -> Result<Option<SavedView>, DbError> {
    Ok(sqlx::query_as!(
        SavedView,
        "INSERT INTO mr_saved_views (id, user_id, repo_id, name, query)
         SELECT $1, $2, $3, $4, $5
          WHERE EXISTS (SELECT 1 FROM mr_saved_views WHERE user_id = $2 AND repo_id = $3 AND name = $4)
             OR (SELECT count(*) FROM mr_saved_views WHERE user_id = $2 AND repo_id = $3) < $6
         ON CONFLICT (user_id, repo_id, name) DO UPDATE SET query = EXCLUDED.query
         RETURNING id, name, query",
        uuid::Uuid::now_v7(),
        user.as_uuid(),
        repo.as_uuid(),
        name,
        query,
        most
    )
    .fetch_optional(exec)
    .await?)
}

/// Deletes one of a user's views of a repository; false if there's none.
pub async fn delete(
    exec: impl PgExecutor<'_>,
    user: UserId,
    repo: RepoId,
    id: uuid::Uuid,
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "DELETE FROM mr_saved_views WHERE id = $1 AND user_id = $2 AND repo_id = $3",
        id,
        user.as_uuid(),
        repo.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() == 1)
}
