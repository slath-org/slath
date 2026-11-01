// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Invitation links (registration mode `invite`).

use slath_core::ids::UserId;
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

#[derive(Debug, Clone)]
pub struct Invitation {
    pub id: uuid::Uuid,
    pub created_by: Option<String>,
    pub email: Option<String>,
    pub note: Option<String>,
    pub expires_at: OffsetDateTime,
    pub used_at: Option<OffsetDateTime>,
    pub used_by: Option<String>,
    pub revoked_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

pub struct NewInvitation<'a> {
    pub token_hash: &'a [u8],
    pub created_by: Option<UserId>,
    pub email: Option<&'a str>,
    pub note: Option<&'a str>,
    pub expires_at: OffsetDateTime,
}

pub async fn insert(
    exec: impl PgExecutor<'_>,
    i: &NewInvitation<'_>,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO invitations (id, token_hash, created_by, email, note, expires_at)
         VALUES ($1, $2, $3, $4::text::citext, $5, $6)",
        id,
        i.token_hash,
        i.created_by.map(|u| *u.as_uuid()),
        i.email,
        i.note,
        i.expires_at
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// The most recent invitations, newest first.
pub async fn list(exec: impl PgExecutor<'_>, limit: i64) -> Result<Vec<Invitation>, DbError> {
    let rows = sqlx::query_as!(
        Invitation,
        r#"SELECT i.id, cn.name::text AS created_by, i.email::text AS email, i.note, i.expires_at,
                  i.used_at, un.name::text AS used_by, i.revoked_at, i.created_at
           FROM invitations i
           LEFT JOIN users c ON c.id = i.created_by LEFT JOIN namespaces cn ON cn.id = c.namespace_id
           LEFT JOIN users u ON u.id = i.used_by LEFT JOIN namespaces un ON un.id = u.namespace_id
           ORDER BY i.created_at DESC LIMIT $1"#,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// The bound address of a live invitation (`Some(None)`: live, unbound).
pub async fn live(
    exec: impl PgExecutor<'_>,
    token_hash: &[u8],
) -> Result<Option<Option<String>>, DbError> {
    let r = sqlx::query!(
        r#"SELECT email::text AS email FROM invitations
           WHERE token_hash = $1 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > now()"#,
        token_hash
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| r.email))
}

/// Uses an invitation for `user` (once). Returns its bound address, or
/// `None` if it can't be used.
pub async fn use_for(
    exec: impl PgExecutor<'_>,
    token_hash: &[u8],
    user: UserId,
) -> Result<Option<Option<String>>, DbError> {
    let r = sqlx::query!(
        r#"UPDATE invitations SET used_at = now(), used_by = $2
           WHERE token_hash = $1 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > now()
           RETURNING email::text AS email"#,
        token_hash,
        user.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| r.email))
}

pub async fn revoke(exec: impl PgExecutor<'_>, id: uuid::Uuid) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "UPDATE invitations SET revoked_at = now() WHERE id = $1 AND used_at IS NULL AND revoked_at IS NULL",
        id
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Whether the user's address is confirmed (or they have none).
pub async fn email_confirmed(exec: impl PgExecutor<'_>, user: UserId) -> Result<bool, DbError> {
    let r = sqlx::query_scalar!(
        r#"SELECT (email IS NULL OR email_verified) AS "ok!" FROM users WHERE id = $1"#,
        user.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.unwrap_or(false))
}

/// Marks the user's current address confirmed (invited with that address).
pub async fn confirm_email(exec: impl PgExecutor<'_>, user: UserId) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE users SET email_verified = true WHERE id = $1",
        user.as_uuid()
    )
    .execute(exec)
    .await
    .map_err(|e| DbError::from_unique(e, "email"))?;
    Ok(())
}
