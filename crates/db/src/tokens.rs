// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Personal access tokens.

use slath_core::ids::{RepoId, TokenId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

pub struct NewToken<'a> {
    pub id: TokenId,
    pub user_id: UserId,
    pub name: &'a str,
    pub prefix: &'a str,
    pub hash: &'a [u8; 32],
    pub scopes: &'a [String],
    pub repo_ids: Option<&'a [RepoId]>,
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct TokenRow {
    pub id: TokenId,
    pub user_id: UserId,
    pub name: String,
    pub prefix: String,
    pub scopes: Vec<String>,
    pub repo_ids: Option<Vec<RepoId>>,
    pub expires_at: Option<OffsetDateTime>,
    pub last_used_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

pub async fn insert(exec: impl PgExecutor<'_>, t: &NewToken<'_>) -> Result<(), DbError> {
    let repos: Option<Vec<uuid::Uuid>> = t
        .repo_ids
        .map(|r| r.iter().map(|id| *id.as_uuid()).collect());
    sqlx::query!(
        "INSERT INTO access_tokens (id, user_id, name, token_prefix, token_hash, scopes, repo_ids, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        t.id.as_uuid(),
        t.user_id.as_uuid(),
        t.name,
        t.prefix,
        &t.hash[..],
        t.scopes,
        repos.as_deref(),
        t.expires_at,
    )
    .execute(exec)
    .await?;
    Ok(())
}

fn ids(v: Option<Vec<uuid::Uuid>>) -> Option<Vec<RepoId>> {
    v.map(|v| v.into_iter().map(RepoId::from_uuid).collect())
}

/// The user's tokens (including revoked and expired ones), newest first.
pub async fn list_for_user(
    exec: impl PgExecutor<'_>,
    user: UserId,
) -> Result<Vec<TokenRow>, DbError> {
    let rows = sqlx::query!(
        "SELECT id, user_id, name, token_prefix, scopes, repo_ids, expires_at, last_used_at, revoked_at, created_at
         FROM access_tokens WHERE user_id = $1 ORDER BY created_at DESC LIMIT 500",
        user.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| TokenRow {
            id: TokenId::from_uuid(r.id),
            user_id: UserId::from_uuid(r.user_id),
            name: r.name,
            prefix: r.token_prefix,
            scopes: r.scopes,
            repo_ids: ids(r.repo_ids),
            expires_at: r.expires_at,
            last_used_at: r.last_used_at,
            revoked_at: r.revoked_at,
            created_at: r.created_at,
        })
        .collect())
}

/// A usable token by hash: not revoked, not expired.
pub async fn find_active(
    exec: impl PgExecutor<'_>,
    hash: &[u8; 32],
) -> Result<Option<TokenRow>, DbError> {
    let r = sqlx::query!(
        "SELECT id, user_id, name, token_prefix, scopes, repo_ids, expires_at, last_used_at, revoked_at, created_at
         FROM access_tokens
         WHERE token_hash = $1 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())",
        &hash[..]
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| TokenRow {
        id: TokenId::from_uuid(r.id),
        user_id: UserId::from_uuid(r.user_id),
        name: r.name,
        prefix: r.token_prefix,
        scopes: r.scopes,
        repo_ids: ids(r.repo_ids),
        expires_at: r.expires_at,
        last_used_at: r.last_used_at,
        revoked_at: r.revoked_at,
        created_at: r.created_at,
    }))
}

/// Records use at most once per minute.
pub async fn touch(exec: impl PgExecutor<'_>, id: TokenId) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE access_tokens SET last_used_at = now()
         WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
        id.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Revokes one of the user's tokens; false if there's no such token.
pub async fn revoke(exec: impl PgExecutor<'_>, user: UserId, id: TokenId) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "UPDATE access_tokens SET revoked_at = now() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
        id.as_uuid(),
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Revokes every token of a user (suspension, deletion).
pub async fn revoke_all_for_user(exec: impl PgExecutor<'_>, user: UserId) -> Result<u64, DbError> {
    let r = sqlx::query!(
        "UPDATE access_tokens SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected())
}
