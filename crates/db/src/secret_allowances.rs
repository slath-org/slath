// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Push protection allowances and remembered first-push rejections.

use std::collections::HashSet;

use slath_core::ids::{RepoId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

/// An allowance as shown to repository writers.
#[derive(Debug, Clone)]
pub struct Allowance {
    pub id: uuid::Uuid,
    pub fingerprint: String,
    pub rule: String,
    pub created_by: Option<String>,
    pub reason: Option<String>,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

pub async fn insert(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    fingerprint: &str,
    rule: &str,
    by: UserId,
    reason: Option<&str>,
    expires_at: OffsetDateTime,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO secret_allowances (id, repo_id, fingerprint, rule, created_by, reason, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        id,
        repo.as_uuid(),
        fingerprint,
        rule,
        by.as_uuid(),
        reason,
        expires_at
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Fingerprints with an allowance in force for `repo`.
pub async fn active(exec: impl PgExecutor<'_>, repo: RepoId) -> Result<HashSet<String>, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT fingerprint FROM secret_allowances WHERE repo_id = $1 AND expires_at > now()",
        repo.as_uuid()
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .collect())
}

/// Allowances in force, newest first.
pub async fn list(exec: impl PgExecutor<'_>, repo: RepoId) -> Result<Vec<Allowance>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT a.id, a.fingerprint, a.rule, n.name::text AS "created_by?", a.reason, a.created_at, a.expires_at
             FROM secret_allowances a
             LEFT JOIN users u ON u.id = a.created_by
             LEFT JOIN namespaces n ON n.id = u.namespace_id
            WHERE a.repo_id = $1 AND a.expires_at > now()
            ORDER BY a.created_at DESC
            LIMIT 200"#,
        repo.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Allowance {
            id: r.id,
            fingerprint: r.fingerprint,
            rule: r.rule,
            created_by: r.created_by,
            reason: r.reason,
            created_at: r.created_at,
            expires_at: r.expires_at,
        })
        .collect())
}

/// Remembers the findings of a rejected push (`kind`: `first_push` into
/// an empty repository, or `push` with several findings); returns its ID.
pub async fn insert_rejection(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    kind: &str,
    findings: &serde_json::Value,
    expires_at: OffsetDateTime,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO secret_rejections (id, repo_id, kind, findings, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
        id,
        repo.as_uuid(),
        kind,
        findings,
        expires_at
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// A remembered rejection of `repo` that hasn't expired: its kind and
/// findings.
pub async fn rejection(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    id: uuid::Uuid,
) -> Result<Option<(String, serde_json::Value)>, DbError> {
    Ok(sqlx::query!(
        "SELECT kind, findings FROM secret_rejections
          WHERE id = $1 AND repo_id = $2 AND expires_at > now()",
        id,
        repo.as_uuid()
    )
    .fetch_optional(exec)
    .await?
    .map(|r| (r.kind, r.findings)))
}

/// Deletes expired allowances and rejections.
pub async fn purge(exec: impl PgExecutor<'_> + Copy) -> Result<u64, DbError> {
    let a =
        sqlx::query!("DELETE FROM secret_allowances WHERE expires_at < now() - interval '30 days'")
            .execute(exec)
            .await?
            .rows_affected();
    let b = sqlx::query!("DELETE FROM secret_rejections WHERE expires_at < now()")
        .execute(exec)
        .await?
        .rows_affected();
    Ok(a + b)
}
