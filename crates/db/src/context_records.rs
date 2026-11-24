// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Agent context records: metadata and binding. The payload is an
//! encrypted blob (`payload_sha256`) whose own key is sealed in
//! `payload_key_enc`. Deleting or expiring a record turns its row into a
//! tombstone: the agent's claimed fields, the key, and the blob go; the
//! rest stays, with who deleted it.

use slath_core::ids::{MergeRequestId, RepoId, UserId};
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;

use crate::DbError;

/// A record's metadata.
#[derive(Debug, Clone)]
pub struct Record {
    pub id: uuid::Uuid,
    pub repo_id: RepoId,
    pub uploader_id: Option<UserId>,
    pub initiated_by: Option<UserId>,
    /// `None` only on a tombstone.
    pub agent_name: Option<String>,
    pub agent_model: Option<String>,
    pub agent_tool: Option<String>,
    pub detail: String,
    pub requested_mr: Option<i32>,
    pub requested_change: Option<String>,
    pub requested_commit: Option<String>,
    pub mr_id: Option<MergeRequestId>,
    pub change_id: Option<uuid::Uuid>,
    pub binding_state: String,
    /// The payload's blob, key, and key ID; `None` only on a tombstone.
    pub payload_sha256: Option<String>,
    pub payload_bytes: i32,
    pub transcript_entries: i32,
    pub payload_key_enc: Option<Vec<u8>>,
    pub key_id: Option<String>,
    pub created_at: OffsetDateTime,
    /// Set on a tombstone, with who deleted it or that retention did.
    pub deleted: Option<Deletion>,
}

/// How a record became a tombstone.
#[derive(Debug, Clone)]
pub struct Deletion {
    pub at: OffsetDateTime,
    /// The person who deleted it, while that account exists.
    pub by: Option<UserId>,
    /// Retention deleted it, not a person.
    pub retention: bool,
    /// The payload's digest as it was stored.
    pub payload_sha256: String,
}

impl Record {
    /// The payload's digest as stored, also on a tombstone.
    #[must_use]
    pub fn digest(&self) -> &str {
        self.payload_sha256
            .as_deref()
            .or(self.deleted.as_ref().map(|d| d.payload_sha256.as_str()))
            .unwrap_or_default()
    }
}

/// A new record; `id` is chosen by the caller (the payload is bound to it).
#[derive(Debug, Clone)]
pub struct NewRecord<'a> {
    pub id: uuid::Uuid,
    pub repo_id: RepoId,
    pub uploader_id: UserId,
    pub initiated_by: Option<UserId>,
    pub agent_name: &'a str,
    pub agent_model: Option<&'a str>,
    pub agent_tool: Option<&'a str>,
    pub detail: &'a str,
    pub requested_mr: Option<i32>,
    pub requested_change: Option<&'a str>,
    pub requested_commit: Option<&'a str>,
    pub mr_id: Option<MergeRequestId>,
    pub change_id: Option<uuid::Uuid>,
    pub binding_state: &'a str,
    pub payload_sha256: &'a str,
    pub payload_bytes: i32,
    pub transcript_entries: i32,
    pub payload_key_enc: &'a [u8],
    pub key_id: &'a str,
}

pub async fn insert(exec: &mut PgConnection, r: &NewRecord<'_>) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO context_records (id, repo_id, uploader_id, initiated_by, agent_name,
             agent_model, agent_tool, detail, requested_mr, requested_change, requested_commit,
             mr_id, change_id, binding_state, payload_sha256, payload_bytes, transcript_entries,
             payload_key_enc, key_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
             $18, $19)",
        r.id,
        r.repo_id.as_uuid(),
        r.uploader_id.as_uuid(),
        r.initiated_by.map(|u| *u.as_uuid()),
        r.agent_name,
        r.agent_model,
        r.agent_tool,
        r.detail,
        r.requested_mr,
        r.requested_change,
        r.requested_commit,
        r.mr_id.map(|m| *m.as_uuid()),
        r.change_id,
        r.binding_state,
        r.payload_sha256,
        r.payload_bytes,
        r.transcript_entries,
        r.payload_key_enc,
        r.key_id,
    )
    .execute(&mut *exec)
    .await?;
    Ok(())
}

macro_rules! record {
    ($row:expr) => {{
        let r = $row;
        Record {
            id: r.id,
            repo_id: RepoId::from_uuid(r.repo_id),
            uploader_id: r.uploader_id.map(UserId::from_uuid),
            initiated_by: r.initiated_by.map(UserId::from_uuid),
            agent_name: r.agent_name,
            agent_model: r.agent_model,
            agent_tool: r.agent_tool,
            detail: r.detail,
            requested_mr: r.requested_mr,
            requested_change: r.requested_change,
            requested_commit: r.requested_commit,
            mr_id: r.mr_id.map(MergeRequestId::from_uuid),
            change_id: r.change_id,
            binding_state: r.binding_state,
            payload_sha256: r.payload_sha256,
            payload_bytes: r.payload_bytes,
            transcript_entries: r.transcript_entries,
            payload_key_enc: r.payload_key_enc,
            key_id: r.key_id,
            created_at: r.created_at,
            deleted: r.deleted_at.map(|at| Deletion {
                at,
                by: r.deleted_by.map(UserId::from_uuid),
                retention: r.deleted_by_retention,
                payload_sha256: r.deleted_payload_sha256.unwrap_or_default(),
            }),
        }
    }};
}

/// A record of `repo` by ID.
pub async fn get(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    id: uuid::Uuid,
) -> Result<Option<Record>, DbError> {
    Ok(sqlx::query!(
        "SELECT * FROM context_records WHERE repo_id = $1 AND id = $2",
        repo.as_uuid(),
        id
    )
    .fetch_optional(exec)
    .await?
    .map(|r| record!(r)))
}

/// What a list asks for.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filter {
    pub mr: Option<MergeRequestId>,
    pub change: Option<uuid::Uuid>,
    pub unlinked: bool,
    /// Records older than this (`created_at`, `id`), for paging.
    pub before: Option<(OffsetDateTime, uuid::Uuid)>,
}

/// A repository's records, newest first.
pub async fn list(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    f: Filter,
    limit: i64,
) -> Result<Vec<Record>, DbError> {
    let (before_at, before_id) = f.before.unzip();
    Ok(sqlx::query!(
        "SELECT * FROM context_records
          WHERE repo_id = $1
            AND ($2::uuid IS NULL OR mr_id = $2)
            AND ($3::uuid IS NULL OR change_id = $3)
            AND (NOT $4 OR binding_state = 'unlinked')
            AND ($5::timestamptz IS NULL OR (created_at, id) < ($5, $6))
          ORDER BY created_at DESC, id DESC
          LIMIT $7",
        repo.as_uuid(),
        f.mr.map(|m| *m.as_uuid()),
        f.change,
        f.unlinked,
        before_at,
        before_id,
        limit
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .map(|r| record!(r))
    .collect())
}

/// Binds a record that isn't bound yet (a commit trailer naming it);
/// false if it was bound already or isn't in `repo`.
pub async fn bind(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    id: uuid::Uuid,
    mr: MergeRequestId,
    change: Option<uuid::Uuid>,
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "UPDATE context_records SET mr_id = $3, change_id = $4, binding_state = 'bound'
          WHERE repo_id = $1 AND id = $2 AND binding_state <> 'bound'
            AND deleted_at IS NULL",
        repo.as_uuid(),
        id,
        mr.as_uuid(),
        change
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// Whether `viewer` owns `user`'s namespace: it's `user`, or `user` is a
/// bot that `viewer` owns directly or as an owner of its organization.
pub async fn owns_uploader(
    exec: impl PgExecutor<'_>,
    user: UserId,
    viewer: UserId,
) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT slath_owns_namespace(u.namespace_id, $2) AS \"owns!\" FROM users u WHERE u.id = $1",
        user.as_uuid(),
        viewer.as_uuid()
    )
    .fetch_optional(exec)
    .await?
    .unwrap_or(false))
}

/// The context settings to change; `None` keeps one.
#[derive(Debug, Default, Clone)]
pub struct Settings<'a> {
    pub detail: Option<&'a str>,
    pub entire_import: Option<bool>,
    pub visibility: Option<&'a str>,
    /// `Some(None)` clears the retention.
    pub retention_days: Option<Option<i32>>,
}

pub async fn set_settings(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    s: &Settings<'_>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE repositories SET context_detail = COALESCE($2, context_detail),
             context_entire_import = COALESCE($3, context_entire_import),
             context_visibility = COALESCE($4, context_visibility),
             context_retention_days = CASE WHEN $5 THEN $6 ELSE context_retention_days END
          WHERE id = $1",
        repo.as_uuid(),
        s.detail,
        s.entire_import,
        s.visibility,
        s.retention_days.is_some(),
        s.retention_days.flatten()
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Turns one record of `repo` into a tombstone, deleted by `by`; whether
/// it was a record (not already a tombstone).
pub async fn delete(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    id: uuid::Uuid,
    by: UserId,
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "UPDATE context_records SET agent_name = NULL, agent_model = NULL, agent_tool = NULL,
             payload_key_enc = NULL, key_id = NULL,
             deleted_payload_sha256 = payload_sha256, payload_sha256 = NULL,
             deleted_at = now(), deleted_by = $3
          WHERE repo_id = $1 AND id = $2 AND deleted_at IS NULL",
        repo.as_uuid(),
        id,
        by.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Turns up to `limit` records past their repository's retention into
/// tombstones, and says how many each repository lost.
pub async fn delete_expired(
    exec: impl PgExecutor<'_>,
    limit: i64,
) -> Result<Vec<(RepoId, i64)>, DbError> {
    let rows = sqlx::query!(
        r#"WITH gone AS (
               UPDATE context_records c
                  SET agent_name = NULL, agent_model = NULL, agent_tool = NULL,
                      payload_key_enc = NULL, key_id = NULL,
                      deleted_payload_sha256 = c.payload_sha256, payload_sha256 = NULL,
                      deleted_at = now(), deleted_by_retention = true
                WHERE c.id IN (
                      SELECT c2.id FROM context_records c2
                        JOIN repositories r ON r.id = c2.repo_id
                       WHERE r.context_retention_days IS NOT NULL
                         AND c2.deleted_at IS NULL
                         AND c2.created_at < now() - make_interval(days => r.context_retention_days)
                       LIMIT $1)
               RETURNING c.repo_id)
           SELECT repo_id AS "repo_id!", count(*) AS "n!" FROM gone GROUP BY repo_id"#,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (RepoId::from_uuid(r.repo_id), r.n))
        .collect())
}

/// The (merge request, change) pairs of `repo`'s open merge requests that
/// have a version with commit `oid`, in any revision.
pub async fn open_changes_with_commit(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    oid: &str,
) -> Result<Vec<(MergeRequestId, uuid::Uuid)>, DbError> {
    Ok(sqlx::query!(
        r#"SELECT DISTINCT m.id AS "mr!", v.change_id AS "change!"
             FROM mr_change_versions v
             JOIN mr_revisions r ON r.id = v.revision_id
             JOIN merge_requests m ON m.id = r.mr_id
            WHERE m.repo_id = $1 AND m.state = 'open' AND v.commit_oid = $2
              AND v.change_id IS NOT NULL"#,
        repo.as_uuid(),
        oid
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .map(|r| (MergeRequestId::from_uuid(r.mr), r.change))
    .collect())
}
