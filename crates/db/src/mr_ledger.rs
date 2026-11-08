// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The review continuity ledger of merge requests (append-only).

use slath_core::ids::{MergeRequestId, UserId};
use sqlx::PgExecutor;

use crate::DbError;

/// A ledger entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: uuid::Uuid,
    pub revision_seq: Option<i32>,
    /// `binding`, `approval_carried`, `approval_stale`, `status_stale`,
    /// `target_moved`, or `signature_removed`.
    pub kind: String,
    pub change_id: Option<String>,
    pub state: Option<String>,
    pub reason: Option<String>,
    pub detail: serde_json::Value,
    pub actor: Option<String>,
    pub created_at: time::OffsetDateTime,
}

/// What a new entry records.
#[derive(Debug, Default)]
pub struct NewEntry<'a> {
    pub revision_seq: Option<i32>,
    pub kind: &'a str,
    pub change_id: Option<&'a str>,
    pub state: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub detail: serde_json::Value,
    pub actor: Option<UserId>,
}

pub async fn append(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    e: &NewEntry<'_>,
) -> Result<(), DbError> {
    let detail = if e.detail.is_null() {
        serde_json::json!({})
    } else {
        e.detail.clone()
    };
    sqlx::query!(
        "INSERT INTO mr_ledger (id, mr_id, revision_seq, kind, change_id, state, reason, detail, actor_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        uuid::Uuid::now_v7(),
        mr.as_uuid(),
        e.revision_seq,
        e.kind,
        e.change_id,
        e.state,
        e.reason,
        detail,
        e.actor.map(|u| *u.as_uuid())
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Every entry, oldest first.
pub async fn list(exec: impl PgExecutor<'_>, mr: MergeRequestId) -> Result<Vec<Entry>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT l.id, l.revision_seq, l.kind, l.change_id, l.state, l.reason, l.detail,
                  n.name::text AS actor, l.created_at
           FROM mr_ledger l
           LEFT JOIN users u ON u.id = l.actor_id AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE l.mr_id = $1 ORDER BY l.created_at, l.id"#,
        mr.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Entry {
            id: r.id,
            revision_seq: r.revision_seq,
            kind: r.kind,
            change_id: r.change_id,
            state: r.state,
            reason: r.reason,
            detail: r.detail,
            actor: r.actor,
            created_at: r.created_at,
        })
        .collect())
}
