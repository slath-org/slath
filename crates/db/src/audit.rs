// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The append-only audit log.

use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

/// One event to record.
#[derive(Debug, Clone)]
pub struct NewEvent<'a> {
    pub actor_id: Option<uuid::Uuid>,
    pub actor_ip: Option<&'a str>,
    pub action: &'a str,
    pub target_kind: &'a str,
    pub target_id: Option<uuid::Uuid>,
    pub repo_id: Option<uuid::Uuid>,
    pub org_id: Option<uuid::Uuid>,
    pub data: serde_json::Value,
}

pub async fn insert(exec: impl PgExecutor<'_>, e: &NewEvent<'_>) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO audit_events (id, actor_id, actor_ip, action, target_kind, target_id, repo_id, org_id, data)
         VALUES ($1, $2, $3::text::inet, $4, $5, $6, $7, $8, $9)",
        uuid::Uuid::now_v7(),
        e.actor_id,
        e.actor_ip,
        e.action,
        e.target_kind,
        e.target_id,
        e.repo_id,
        e.org_id,
        e.data,
    )
    .execute(exec)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Event {
    pub id: uuid::Uuid,
    pub actor: Option<String>,
    pub actor_ip: Option<String>,
    pub action: String,
    pub target_kind: String,
    pub target_id: Option<uuid::Uuid>,
    pub data: serde_json::Value,
    pub created_at: OffsetDateTime,
}

/// Filters for listing events.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Prefix of the action (e.g. `repo.`).
    pub action: Option<String>,
    /// Actor username (exact, case-insensitive).
    pub actor: Option<String>,
    /// Only events of this organization.
    pub org_id: Option<uuid::Uuid>,
}

/// Newest events first.
pub async fn list(
    exec: impl PgExecutor<'_>,
    f: &Filter,
    offset: i64,
    limit: i64,
) -> Result<Vec<Event>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT e.id, n.name::text AS actor, host(e.actor_ip) AS actor_ip, e.action, e.target_kind, e.target_id, e.data, e.created_at
           FROM audit_events e
           LEFT JOIN users u ON u.id = e.actor_id
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE ($1::text IS NULL OR e.action LIKE replace(replace($1, '%', '\%'), '_', '\_') || '%')
             AND ($2::text IS NULL OR n.name = $2::citext)
             AND ($3::uuid IS NULL OR e.org_id = $3)
           ORDER BY e.created_at DESC, e.id DESC OFFSET $4 LIMIT $5"#,
        f.action,
        f.actor,
        f.org_id,
        offset,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Event {
            id: r.id,
            actor: r.actor,
            actor_ip: r.actor_ip,
            action: r.action,
            target_kind: r.target_kind,
            target_id: r.target_id,
            data: r.data,
            created_at: r.created_at,
        })
        .collect())
}

/// Deletes events older than `days` (the retention period).
pub async fn purge(tx: &mut sqlx::PgConnection, days: i32) -> Result<u64, DbError> {
    sqlx::query!("SET LOCAL slath.audit_purge = 'on'")
        .execute(&mut *tx)
        .await?;
    let r = sqlx::query!(
        "DELETE FROM audit_events WHERE created_at < now() - make_interval(days => $1)",
        days
    )
    .execute(&mut *tx)
    .await?;
    Ok(r.rows_affected())
}
