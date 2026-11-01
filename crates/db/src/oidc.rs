// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! OpenID Connect identities, authorization states, and pending first
//! sign-ins.

use slath_core::ids::UserId;
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

#[derive(Debug, Clone)]
pub struct Identity {
    pub issuer: String,
    pub subject: String,
    pub user_id: UserId,
    pub provider_key: String,
    pub email: Option<String>,
    pub created_at: OffsetDateTime,
    pub last_login_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct State {
    pub provider_key: String,
    pub browser_hash: String,
    pub nonce: String,
    pub pkce_verifier: String,
    pub purpose: String,
    pub user_id: Option<UserId>,
    pub next: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Pending {
    pub provider_key: String,
    pub browser_hash: String,
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub preferred_username: Option<String>,
    pub display_name: Option<String>,
}

pub async fn find_identity(
    exec: impl PgExecutor<'_>,
    issuer: &str,
    subject: &str,
) -> Result<Option<Identity>, DbError> {
    let r = sqlx::query!(
        "SELECT issuer, subject, user_id, provider_key, email, created_at, last_login_at
           FROM oidc_identities WHERE issuer = $1 AND subject = $2",
        issuer,
        subject
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| Identity {
        issuer: r.issuer,
        subject: r.subject,
        user_id: UserId::from_uuid(r.user_id),
        provider_key: r.provider_key,
        email: r.email,
        created_at: r.created_at,
        last_login_at: r.last_login_at,
    }))
}

pub async fn identities(exec: impl PgExecutor<'_>, user: UserId) -> Result<Vec<Identity>, DbError> {
    let rows = sqlx::query!(
        "SELECT issuer, subject, user_id, provider_key, email, created_at, last_login_at
           FROM oidc_identities WHERE user_id = $1 ORDER BY created_at",
        user.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Identity {
            issuer: r.issuer,
            subject: r.subject,
            user_id: UserId::from_uuid(r.user_id),
            provider_key: r.provider_key,
            email: r.email,
            created_at: r.created_at,
            last_login_at: r.last_login_at,
        })
        .collect())
}

/// Links an identity; `Conflict` if it is already linked (to anyone).
pub async fn link(
    exec: impl PgExecutor<'_>,
    issuer: &str,
    subject: &str,
    user: UserId,
    provider_key: &str,
    email: Option<&str>,
) -> Result<(), DbError> {
    let n = sqlx::query!(
        "INSERT INTO oidc_identities (issuer, subject, user_id, provider_key, email)
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        issuer,
        subject,
        user.as_uuid(),
        provider_key,
        email
    )
    .execute(exec)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(DbError::Conflict("identity"));
    }
    Ok(())
}

pub async fn unlink(
    exec: impl PgExecutor<'_>,
    user: UserId,
    issuer: &str,
    subject: &str,
) -> Result<bool, DbError> {
    Ok(sqlx::query!(
        "DELETE FROM oidc_identities WHERE user_id = $1 AND issuer = $2 AND subject = $3",
        user.as_uuid(),
        issuer,
        subject
    )
    .execute(exec)
    .await?
    .rows_affected()
        == 1)
}

pub async fn touch_identity(
    exec: impl PgExecutor<'_>,
    issuer: &str,
    subject: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE oidc_identities SET last_login_at = now() WHERE issuer = $1 AND subject = $2",
        issuer,
        subject
    )
    .execute(exec)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_state(
    exec: impl PgExecutor<'_>,
    state_hash: &str,
    s: &State,
    expires_at: OffsetDateTime,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO oidc_states (state_hash, provider_key, browser_hash, nonce, pkce_verifier,
                                  purpose, user_id, next, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        state_hash,
        s.provider_key,
        s.browser_hash,
        s.nonce,
        s.pkce_verifier,
        s.purpose,
        s.user_id.map(|u| *u.as_uuid()),
        s.next,
        expires_at
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Takes a state (single use): deletes it and returns it if it was still
/// valid.
pub async fn take_state(
    exec: impl PgExecutor<'_>,
    state_hash: &str,
) -> Result<Option<State>, DbError> {
    let r = sqlx::query!(
        "DELETE FROM oidc_states WHERE state_hash = $1
         RETURNING provider_key, browser_hash, nonce, pkce_verifier, purpose, user_id, next,
                   expires_at > now() AS \"live!\"",
        state_hash
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.filter(|r| r.live).map(|r| State {
        provider_key: r.provider_key,
        browser_hash: r.browser_hash,
        nonce: r.nonce,
        pkce_verifier: r.pkce_verifier,
        purpose: r.purpose,
        user_id: r.user_id.map(UserId::from_uuid),
        next: r.next,
    }))
}

pub async fn insert_pending(
    exec: impl PgExecutor<'_>,
    token_hash: &str,
    p: &Pending,
    expires_at: OffsetDateTime,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO oidc_pending (token_hash, provider_key, browser_hash, issuer, subject, email,
                                   email_verified, preferred_username, display_name, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        token_hash,
        p.provider_key,
        p.browser_hash,
        p.issuer,
        p.subject,
        p.email,
        p.email_verified,
        p.preferred_username,
        p.display_name,
        expires_at
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn pending(
    exec: impl PgExecutor<'_>,
    token_hash: &str,
) -> Result<Option<Pending>, DbError> {
    let r = sqlx::query!(
        "SELECT provider_key, browser_hash, issuer, subject, email, email_verified,
                preferred_username, display_name
           FROM oidc_pending WHERE token_hash = $1 AND expires_at > now()",
        token_hash
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| Pending {
        provider_key: r.provider_key,
        browser_hash: r.browser_hash,
        issuer: r.issuer,
        subject: r.subject,
        email: r.email,
        email_verified: r.email_verified,
        preferred_username: r.preferred_username,
        display_name: r.display_name,
    }))
}

pub async fn delete_pending(exec: impl PgExecutor<'_>, token_hash: &str) -> Result<(), DbError> {
    sqlx::query!("DELETE FROM oidc_pending WHERE token_hash = $1", token_hash)
        .execute(exec)
        .await?;
    Ok(())
}

/// Removes expired states and pending sign-ins.
pub async fn purge_expired(exec: impl PgExecutor<'_> + Copy) -> Result<(), DbError> {
    sqlx::query!("DELETE FROM oidc_states WHERE expires_at < now()")
        .execute(exec)
        .await?;
    sqlx::query!("DELETE FROM oidc_pending WHERE expires_at < now()")
        .execute(exec)
        .await?;
    Ok(())
}

/// Active users whose confirmed email is `email` (case-insensitive).
pub async fn users_with_verified_email(
    exec: impl PgExecutor<'_>,
    email: &str,
) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM users
            WHERE email = $1::text::citext AND email_verified AND state = 'active'"#,
        email
    )
    .fetch_one(exec)
    .await?)
}
