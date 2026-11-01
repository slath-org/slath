// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! SSH public keys.

use slath_core::authz::{UserKind, UserState};
use slath_core::ids::{SshKeyId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

#[derive(Debug, Clone)]
pub struct KeyRow {
    pub id: SshKeyId,
    pub user_id: UserId,
    pub title: String,
    pub key_type: String,
    pub public_key: String,
    pub fingerprint: String,
    pub last_used_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

pub struct NewKey<'a> {
    pub id: SshKeyId,
    pub user_id: UserId,
    pub title: &'a str,
    pub key_type: &'a str,
    pub public_key: &'a str,
    pub fingerprint: &'a str,
}

/// Inserts a key; a fingerprint already registered (to anyone) is a conflict.
pub async fn insert(exec: impl PgExecutor<'_>, k: &NewKey<'_>) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO ssh_keys (id, user_id, title, key_type, public_key, fingerprint_sha256)
         VALUES ($1, $2, $3, $4, $5, $6)",
        k.id.as_uuid(),
        k.user_id.as_uuid(),
        k.title,
        k.key_type,
        k.public_key,
        k.fingerprint,
    )
    .execute(exec)
    .await
    .map_err(|e| DbError::from_unique(e, "fingerprint"))?;
    Ok(())
}

pub async fn list_for_user(
    exec: impl PgExecutor<'_>,
    user: UserId,
) -> Result<Vec<KeyRow>, DbError> {
    let rows = sqlx::query!(
        "SELECT id, user_id, title, key_type, public_key, fingerprint_sha256, last_used_at, created_at
         FROM ssh_keys WHERE user_id = $1 ORDER BY created_at DESC LIMIT 500",
        user.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| KeyRow {
            id: SshKeyId::from_uuid(r.id),
            user_id: UserId::from_uuid(r.user_id),
            title: r.title,
            key_type: r.key_type,
            public_key: r.public_key,
            fingerprint: r.fingerprint_sha256,
            last_used_at: r.last_used_at,
            created_at: r.created_at,
        })
        .collect())
}

pub async fn delete_for_user(
    exec: impl PgExecutor<'_>,
    user: UserId,
    id: SshKeyId,
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "DELETE FROM ssh_keys WHERE id = $1 AND user_id = $2",
        id.as_uuid(),
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// A key with the state of its owner, for SSH authentication.
#[derive(Debug, Clone)]
pub struct KeyOwner {
    pub key: KeyRow,
    pub user_kind: UserKind,
    pub user_state: UserState,
    pub is_admin: bool,
}

/// Exact lookup by fingerprint (indexed, case-sensitive).
pub async fn find_by_fingerprint(
    exec: impl PgExecutor<'_>,
    fingerprint: &str,
) -> Result<Option<KeyOwner>, DbError> {
    let r = sqlx::query!(
        "SELECT k.id, k.user_id, k.title, k.key_type, k.public_key, k.fingerprint_sha256, k.last_used_at, k.created_at,
                u.kind, u.state, u.is_admin
         FROM ssh_keys k JOIN users u ON u.id = k.user_id
         WHERE k.fingerprint_sha256 = $1",
        fingerprint
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| KeyOwner {
        key: KeyRow {
            id: SshKeyId::from_uuid(r.id),
            user_id: UserId::from_uuid(r.user_id),
            title: r.title,
            key_type: r.key_type,
            public_key: r.public_key,
            fingerprint: r.fingerprint_sha256,
            last_used_at: r.last_used_at,
            created_at: r.created_at,
        },
        user_kind: crate::users::kind(&r.kind),
        user_state: crate::users::state(&r.state),
        is_admin: r.is_admin,
    }))
}

/// A key by ID with its owner's state (for `ssh-serve`).
pub async fn find_by_id(
    exec: impl PgExecutor<'_>,
    id: SshKeyId,
) -> Result<Option<KeyOwner>, DbError> {
    let r = sqlx::query!(
        "SELECT k.id, k.user_id, k.title, k.key_type, k.public_key, k.fingerprint_sha256, k.last_used_at, k.created_at,
                u.kind, u.state, u.is_admin
         FROM ssh_keys k JOIN users u ON u.id = k.user_id
         WHERE k.id = $1",
        id.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| KeyOwner {
        key: KeyRow {
            id: SshKeyId::from_uuid(r.id),
            user_id: UserId::from_uuid(r.user_id),
            title: r.title,
            key_type: r.key_type,
            public_key: r.public_key,
            fingerprint: r.fingerprint_sha256,
            last_used_at: r.last_used_at,
            created_at: r.created_at,
        },
        user_kind: crate::users::kind(&r.kind),
        user_state: crate::users::state(&r.state),
        is_admin: r.is_admin,
    }))
}

/// Records use at most once per minute.
pub async fn touch(exec: impl PgExecutor<'_>, id: SshKeyId) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE ssh_keys SET last_used_at = now()
         WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
        id.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}
