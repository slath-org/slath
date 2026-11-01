// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Content-addressed blobs, issue attachments, and uploaded avatars.
//!
//! The `refcount` of a blob is maintained by triggers on the rows that use
//! it; nothing here changes it directly.

use slath_core::ids::{OrgId, RepoId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

#[derive(Debug, Clone)]
pub struct Attachment {
    pub id: uuid::Uuid,
    pub repo_id: RepoId,
    pub uploader_id: Option<UserId>,
    pub blob_sha256: String,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub created_at: OffsetDateTime,
}

/// Serializes work on one blob's file until the transaction ends (uploads
/// against the collector's orphan sweep).
pub async fn lock_blob(tx: &mut sqlx::PgConnection, sha256: &str) -> Result<(), DbError> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtext('blob:' || $1::text))",
        sha256
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Records a blob (or locks the existing row until the transaction ends, so
/// the collector can't remove it meanwhile).
pub async fn upsert_blob(
    tx: &mut sqlx::PgConnection,
    sha256: &str,
    size: i64,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO blobs (sha256, size_bytes) VALUES ($1, $2)
         ON CONFLICT (sha256) DO UPDATE SET size_bytes = EXCLUDED.size_bytes",
        sha256,
        size
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

pub async fn insert_attachment(
    tx: &mut sqlx::PgConnection,
    repo: RepoId,
    uploader: UserId,
    sha256: &str,
    filename: &str,
    content_type: &str,
    size: i64,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO attachments (id, repo_id, uploader_id, blob_sha256, filename, content_type, size_bytes)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        id,
        repo.as_uuid(),
        uploader.as_uuid(),
        sha256,
        filename,
        content_type,
        size
    )
    .execute(&mut *tx)
    .await?;
    Ok(id)
}

/// An attachment of a repository (`None` if it belongs to another one).
pub async fn get_attachment(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    id: uuid::Uuid,
) -> Result<Option<Attachment>, DbError> {
    let r = sqlx::query!(
        "SELECT id, repo_id, uploader_id, blob_sha256, filename, content_type, size_bytes, created_at
           FROM attachments WHERE id = $1 AND repo_id = $2",
        id,
        repo.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| Attachment {
        id: r.id,
        repo_id: RepoId::from_uuid(r.repo_id),
        uploader_id: r.uploader_id.map(UserId::from_uuid),
        blob_sha256: r.blob_sha256,
        filename: r.filename,
        content_type: r.content_type,
        size_bytes: r.size_bytes,
        created_at: r.created_at,
    }))
}

/// Blobs nothing has used since before `cutoff`.
pub async fn collectable(
    exec: impl PgExecutor<'_>,
    cutoff: OffsetDateTime,
    limit: i64,
) -> Result<Vec<String>, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT sha256 FROM blobs WHERE refcount = 0 AND unreferenced_since < $1
         ORDER BY unreferenced_since LIMIT $2",
        cutoff,
        limit
    )
    .fetch_all(exec)
    .await?)
}

/// Deletes the blob's row if it is still unused since before `cutoff`. The
/// row stays locked until the transaction ends; remove the file before
/// committing.
pub async fn delete_unreferenced(
    tx: &mut sqlx::PgConnection,
    sha256: &str,
    cutoff: OffsetDateTime,
) -> Result<bool, DbError> {
    let n = sqlx::query!(
        "DELETE FROM blobs WHERE sha256 = $1 AND refcount = 0 AND unreferenced_since < $2",
        sha256,
        cutoff
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    Ok(n == 1)
}

/// Whether a blob row exists.
pub async fn exists(exec: impl PgExecutor<'_>, sha256: &str) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT EXISTS (SELECT 1 FROM blobs WHERE sha256 = $1) AS \"e!\"",
        sha256
    )
    .fetch_one(exec)
    .await?)
}

pub async fn set_user_avatar(
    tx: &mut sqlx::PgConnection,
    user: UserId,
    sha256: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE users SET avatar_blob_sha256 = $2, updated_at = now() WHERE id = $1",
        user.as_uuid(),
        sha256
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

pub async fn set_org_avatar(
    tx: &mut sqlx::PgConnection,
    org: OrgId,
    sha256: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE organizations SET avatar_blob_sha256 = $2, updated_at = now() WHERE id = $1",
        org.as_uuid(),
        sha256
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// The uploaded avatar of a user or organization, by namespace name.
pub async fn namespace_avatar(
    exec: impl PgExecutor<'_>,
    name: &str,
) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT COALESCE(u.avatar_blob_sha256, o.avatar_blob_sha256) AS sha
           FROM namespaces n
           LEFT JOIN users u ON u.namespace_id = n.id AND u.state = 'active'
           LEFT JOIN organizations o ON o.namespace_id = n.id
          WHERE n.name = $1::text::citext",
        name
    )
    .fetch_optional(exec)
    .await?
    .flatten())
}

/// Whether some user or organization uses the blob as its avatar.
pub async fn is_avatar(exec: impl PgExecutor<'_>, sha256: &str) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT (EXISTS (SELECT 1 FROM users WHERE avatar_blob_sha256 = $1)
              OR EXISTS (SELECT 1 FROM organizations WHERE avatar_blob_sha256 = $1)) AS \"e!\"",
        sha256
    )
    .fetch_one(exec)
    .await?)
}
