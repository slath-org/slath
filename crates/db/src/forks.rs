// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Forks: how a fork was made and how visible it may become, and the
//! repositories of a fork network.

use slath_core::authz::Visibility;
use slath_core::ids::{RepoId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;
use crate::repos::{visibility, visibility_str};

/// A fork and the state of the job that copies its parent.
#[derive(Debug, Clone)]
pub struct Fork {
    pub repo_id: RepoId,
    /// `None` once the parent is purged.
    pub parent_id: Option<RepoId>,
    pub default_branch_only: bool,
    pub max_visibility: Visibility,
    /// `queued`, `running`, `done`, or `failed`.
    pub state: String,
    pub error: Option<String>,
    /// Who forked it.
    pub requested_by: Option<UserId>,
    pub created_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
}

impl Fork {
    /// Still being copied: pushes wait until it's done.
    #[must_use]
    pub fn in_progress(&self) -> bool {
        matches!(self.state.as_str(), "queued" | "running")
    }
}

/// Makes `repo` a fork of `parent` (same network) and records the job.
pub async fn insert(
    tx: &mut sqlx::PgConnection,
    repo: RepoId,
    parent: RepoId,
    default_branch_only: bool,
    max_visibility: Visibility,
    by: UserId,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE repositories SET fork_parent_id = $2,
             fork_network_id = (SELECT fork_network_id FROM repositories WHERE id = $2)
         WHERE id = $1",
        repo.as_uuid(),
        parent.as_uuid()
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO repo_forks (repo_id, parent_id, default_branch_only, max_visibility, requested_by)
         VALUES ($1, $2, $3, $4, $5)",
        repo.as_uuid(),
        parent.as_uuid(),
        default_branch_only,
        visibility_str(max_visibility),
        by.as_uuid()
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

pub async fn get(exec: impl PgExecutor<'_>, repo: RepoId) -> Result<Option<Fork>, DbError> {
    let r = sqlx::query!(
        "SELECT f.repo_id, r.fork_parent_id, f.default_branch_only, f.max_visibility, f.state, f.error,
                f.requested_by, f.created_at, f.finished_at
           FROM repo_forks f JOIN repositories r ON r.id = f.repo_id WHERE f.repo_id = $1",
        repo.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| Fork {
        repo_id: RepoId::from_uuid(r.repo_id),
        parent_id: r.fork_parent_id.map(RepoId::from_uuid),
        default_branch_only: r.default_branch_only,
        max_visibility: visibility(&r.max_visibility),
        state: r.state,
        error: r.error,
        requested_by: r.requested_by.map(UserId::from_uuid),
        created_at: r.created_at,
        finished_at: r.finished_at,
    }))
}

pub async fn set_state(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    state: &str,
    error: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE repo_forks SET state = $2, error = $3,
             finished_at = CASE WHEN $2 IN ('done', 'failed') THEN now() END
         WHERE repo_id = $1",
        repo.as_uuid(),
        state,
        error
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// The direct forks of a repository that aren't being deleted, newest
/// first.
pub async fn children(
    exec: impl PgExecutor<'_>,
    parent: RepoId,
    limit: i64,
) -> Result<Vec<RepoId>, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT id FROM repositories WHERE fork_parent_id = $1 AND state <> 'deleting'
          ORDER BY created_at DESC, id DESC LIMIT $2",
        parent.as_uuid(),
        limit
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .map(RepoId::from_uuid)
    .collect())
}

/// Repositories in a namespace that have forks.
pub async fn parents_in(
    exec: impl PgExecutor<'_>,
    namespace: slath_core::ids::NamespaceId,
) -> Result<Vec<RepoId>, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT DISTINCT p.id AS "id!" FROM repositories p
            JOIN repositories f ON f.fork_parent_id = p.id
           WHERE p.namespace_id = $1"#,
        namespace.as_uuid()
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .map(RepoId::from_uuid)
    .collect())
}

/// Whether two repositories belong to the same fork network.
pub async fn same_network(
    exec: impl PgExecutor<'_>,
    a: RepoId,
    b: RepoId,
) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM repositories x JOIN repositories y
                            ON y.fork_network_id = x.fork_network_id
                          WHERE x.id = $1 AND y.id = $2) AS "same!""#,
        a.as_uuid(),
        b.as_uuid()
    )
    .fetch_one(exec)
    .await?)
}

/// Whether `upstream` is a (direct or indirect) parent of `fork`.
pub async fn is_upstream(
    exec: impl PgExecutor<'_>,
    upstream: RepoId,
    fork: RepoId,
) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        r#"WITH RECURSIVE up (id, depth) AS (
               SELECT fork_parent_id, 1 FROM repositories WHERE id = $2
               UNION ALL
               SELECT r.fork_parent_id, up.depth + 1 FROM repositories r JOIN up ON r.id = up.id
                WHERE up.depth < 1000)
           SELECT EXISTS (SELECT 1 FROM up WHERE id = $1) AS "up!""#,
        upstream.as_uuid(),
        fork.as_uuid()
    )
    .fetch_one(exec)
    .await?)
}
