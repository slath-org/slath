// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Branch protection rules.

use slath_core::ids::RepoId;
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protection {
    pub id: uuid::Uuid,
    pub pattern: String,
    pub block_force_push: bool,
    pub block_deletion: bool,
    pub require_mr: bool,
    /// `write`, `maintain`, `admin`, or `nobody`.
    pub push_role: String,
    pub required_approvals: i32,
    pub dismiss_stale_approvals: bool,
    pub require_resolved_threads: bool,
    pub require_up_to_date: bool,
    pub bots_count_as_approvers: bool,
    pub created_at: OffsetDateTime,
}

pub struct NewProtection<'a> {
    pub pattern: &'a str,
    pub block_force_push: bool,
    pub block_deletion: bool,
    pub require_mr: bool,
    pub push_role: &'a str,
    pub required_approvals: i32,
    pub dismiss_stale_approvals: bool,
    pub require_resolved_threads: bool,
    pub require_up_to_date: bool,
    pub bots_count_as_approvers: bool,
}

pub async fn list(exec: impl PgExecutor<'_>, repo: RepoId) -> Result<Vec<Protection>, DbError> {
    let rows = sqlx::query_as!(
        Protection,
        "SELECT id, pattern, block_force_push, block_deletion, require_mr, push_role,
                required_approvals, dismiss_stale_approvals, require_resolved_threads,
                require_up_to_date, bots_count_as_approvers, created_at
         FROM branch_protections WHERE repo_id = $1 ORDER BY pattern",
        repo.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Creates or replaces the rule for `pattern`.
pub async fn upsert(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    p: &NewProtection<'_>,
) -> Result<uuid::Uuid, DbError> {
    let id = sqlx::query_scalar!(
        "INSERT INTO branch_protections (id, repo_id, pattern, block_force_push, block_deletion, require_mr, push_role,
             required_approvals, dismiss_stale_approvals, require_resolved_threads, require_up_to_date,
             bots_count_as_approvers)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         ON CONFLICT (repo_id, pattern) DO UPDATE SET block_force_push = EXCLUDED.block_force_push,
             block_deletion = EXCLUDED.block_deletion, require_mr = EXCLUDED.require_mr,
             push_role = EXCLUDED.push_role, required_approvals = EXCLUDED.required_approvals,
             dismiss_stale_approvals = EXCLUDED.dismiss_stale_approvals,
             require_resolved_threads = EXCLUDED.require_resolved_threads,
             require_up_to_date = EXCLUDED.require_up_to_date,
             bots_count_as_approvers = EXCLUDED.bots_count_as_approvers
         RETURNING id",
        uuid::Uuid::now_v7(),
        repo.as_uuid(),
        p.pattern,
        p.block_force_push,
        p.block_deletion,
        p.require_mr,
        p.push_role,
        p.required_approvals,
        p.dismiss_stale_approvals,
        p.require_resolved_threads,
        p.require_up_to_date,
        p.bots_count_as_approvers
    )
    .fetch_one(exec)
    .await?;
    Ok(id)
}

pub async fn delete(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    id: uuid::Uuid,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "DELETE FROM branch_protections WHERE id = $1 AND repo_id = $2",
        id,
        repo.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}
