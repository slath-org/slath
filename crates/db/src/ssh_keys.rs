// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! SSH public keys.

use slath_core::authz::{UserKind, UserState};
use slath_core::ids::{RepoId, SshKeyId, UserId};
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
        r#"SELECT id, user_id AS "user_id!", title, key_type, public_key, fingerprint_sha256, last_used_at, created_at
         FROM ssh_keys WHERE user_id = $1 ORDER BY created_at DESC LIMIT 500"#,
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
        r#"SELECT k.id, k.user_id AS "user_id!", k.title, k.key_type, k.public_key, k.fingerprint_sha256, k.last_used_at, k.created_at,
                u.kind, u.state, u.is_admin
         FROM ssh_keys k JOIN users u ON u.id = k.user_id
         WHERE k.fingerprint_sha256 = $1"#,
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
        r#"SELECT k.id, k.user_id AS "user_id!", k.title, k.key_type, k.public_key, k.fingerprint_sha256, k.last_used_at, k.created_at,
                u.kind, u.state, u.is_admin
         FROM ssh_keys k JOIN users u ON u.id = k.user_id
         WHERE k.id = $1"#,
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

/// A deploy key of a repository.
#[derive(Debug, Clone)]
pub struct DeployKeyRow {
    pub id: SshKeyId,
    pub repo_id: RepoId,
    pub title: String,
    pub key_type: String,
    pub public_key: String,
    pub fingerprint: String,
    pub can_push: bool,
    /// Who added it (`None` once their account is gone).
    pub added_by: Option<String>,
    pub last_used_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

pub struct NewDeployKey<'a> {
    pub id: SshKeyId,
    pub repo_id: RepoId,
    pub title: &'a str,
    pub key_type: &'a str,
    pub public_key: &'a str,
    pub fingerprint: &'a str,
    pub can_push: bool,
    pub added_by: UserId,
}

/// Inserts a deploy key; a fingerprint already registered (to a user or a
/// repository) is a conflict.
pub async fn insert_deploy_key(
    exec: impl PgExecutor<'_>,
    k: &NewDeployKey<'_>,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO ssh_keys (id, repo_id, title, key_type, public_key, fingerprint_sha256, can_push, added_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        k.id.as_uuid(),
        k.repo_id.as_uuid(),
        k.title,
        k.key_type,
        k.public_key,
        k.fingerprint,
        k.can_push,
        k.added_by.as_uuid(),
    )
    .execute(exec)
    .await
    .map_err(|e| DbError::from_unique(e, "fingerprint"))?;
    Ok(())
}

macro_rules! to_deploy_key {
    ($r:expr) => {
        DeployKeyRow {
            id: SshKeyId::from_uuid($r.id),
            repo_id: RepoId::from_uuid($r.repo_id),
            title: $r.title,
            key_type: $r.key_type,
            public_key: $r.public_key,
            fingerprint: $r.fingerprint_sha256,
            can_push: $r.can_push,
            added_by: $r.added_by,
            last_used_at: $r.last_used_at,
            created_at: $r.created_at,
        }
    };
}

/// The deploy keys of a repository, newest first.
pub async fn deploy_keys(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
) -> Result<Vec<DeployKeyRow>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT k.id, k.repo_id AS "repo_id!", k.title, k.key_type, k.public_key, k.fingerprint_sha256,
                  k.can_push, n.name::text AS added_by, k.last_used_at, k.created_at
           FROM ssh_keys k LEFT JOIN users u ON u.id = k.added_by AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE k.repo_id = $1 ORDER BY k.created_at DESC LIMIT 500"#,
        repo.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| to_deploy_key!(r)).collect())
}

/// A deploy key by fingerprint (exact) or by ID, for SSH authentication.
pub async fn find_deploy_key(
    exec: impl PgExecutor<'_>,
    fingerprint: Option<&str>,
    id: Option<SshKeyId>,
) -> Result<Option<DeployKeyRow>, DbError> {
    let r = sqlx::query!(
        r#"SELECT k.id, k.repo_id AS "repo_id!", k.title, k.key_type, k.public_key, k.fingerprint_sha256,
                  k.can_push, NULL::text AS added_by, k.last_used_at, k.created_at
           FROM ssh_keys k
           WHERE k.repo_id IS NOT NULL
             AND (k.fingerprint_sha256 = $1 OR k.id = $2)"#,
        fingerprint,
        id.map(|i| *i.as_uuid())
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| to_deploy_key!(r)))
}

/// Removes a deploy key of a repository.
pub async fn delete_deploy_key(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    id: SshKeyId,
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "DELETE FROM ssh_keys WHERE id = $1 AND repo_id = $2",
        id.as_uuid(),
        repo.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() > 0)
}
