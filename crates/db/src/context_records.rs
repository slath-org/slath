// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Agent context records: metadata and binding. The payload is an
//! encrypted blob (`payload_sha256`) whose own key is sealed in
//! `payload_key_enc`.

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
    pub agent_name: String,
    pub agent_model: Option<String>,
    pub agent_tool: Option<String>,
    pub detail: String,
    pub requested_mr: Option<i32>,
    pub requested_change: Option<String>,
    pub requested_commit: Option<String>,
    pub mr_id: Option<MergeRequestId>,
    pub change_id: Option<uuid::Uuid>,
    pub binding_state: String,
    pub payload_sha256: String,
    pub payload_bytes: i32,
    pub transcript_entries: i32,
    pub payload_key_enc: Vec<u8>,
    pub key_id: String,
    pub created_at: OffsetDateTime,
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
          WHERE repo_id = $1 AND id = $2 AND binding_state <> 'bound'",
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

/// A repository's context settings: (detail level, Entire import on).
pub async fn settings(exec: impl PgExecutor<'_>, repo: RepoId) -> Result<(String, bool), DbError> {
    let r = sqlx::query!(
        "SELECT context_detail, context_entire_import FROM repositories WHERE id = $1",
        repo.as_uuid()
    )
    .fetch_one(exec)
    .await?;
    Ok((r.context_detail, r.context_entire_import))
}

pub async fn set_settings(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    detail: Option<&str>,
    entire_import: Option<bool>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE repositories SET context_detail = COALESCE($2, context_detail),
             context_entire_import = COALESCE($3, context_entire_import)
          WHERE id = $1",
        repo.as_uuid(),
        detail,
        entire_import
    )
    .execute(exec)
    .await?;
    Ok(())
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
