// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Browser sessions (stored by the SHA-256 of the cookie value) and the
//! failed-login counters.

use slath_core::authz::{UserKind, UserState};
use slath_core::ids::UserId;
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthLevel {
    Password,
    Mfa,
    Passkey,
    Oidc,
}

impl AuthLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Mfa => "mfa",
            Self::Passkey => "passkey",
            Self::Oidc => "oidc",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "mfa" => Self::Mfa,
            "passkey" => Self::Passkey,
            "oidc" => Self::Oidc,
            _ => Self::Password,
        }
    }
}

/// A live session with what authorization needs about its user.
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id_hash: Vec<u8>,
    pub user_id: UserId,
    pub user_name: String,
    pub user_kind: UserKind,
    pub user_state: UserState,
    pub is_admin: bool,
    pub theme: String,
    pub created_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub auth_level: AuthLevel,
    pub sudo_until: Option<OffsetDateTime>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

pub struct NewSession<'a> {
    pub id_hash: &'a [u8; 32],
    pub user_id: UserId,
    pub expires_at: OffsetDateTime,
    pub auth_level: AuthLevel,
    pub sudo_until: Option<OffsetDateTime>,
    pub ip: Option<&'a str>,
    pub user_agent: Option<&'a str>,
}

pub async fn insert(exec: impl PgExecutor<'_>, s: &NewSession<'_>) -> Result<(), DbError> {
    let ua = s
        .user_agent
        .map(|u| u.chars().take(512).collect::<String>());
    sqlx::query!(
        "INSERT INTO sessions (id_hash, user_id, expires_at, auth_level, sudo_until, ip, user_agent)
         VALUES ($1, $2, $3, $4, $5, $6::text::inet, $7)",
        &s.id_hash[..],
        s.user_id.as_uuid(),
        s.expires_at,
        s.auth_level.as_str(),
        s.sudo_until,
        s.ip,
        ua,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// The session for a cookie hash if it exists and hasn't expired
/// (absolutely, or by being idle longer than `idle`).
pub async fn find(
    exec: impl PgExecutor<'_>,
    id_hash: &[u8; 32],
    idle: time::Duration,
) -> Result<Option<SessionRow>, DbError> {
    let row = sqlx::query!(
        r#"SELECT s.id_hash, s.user_id, n.name::text AS "user_name!", u.kind, u.state, u.is_admin, u.theme,
                  s.created_at, s.last_seen_at, s.expires_at, s.auth_level, s.sudo_until,
                  host(s.ip) AS ip, s.user_agent
           FROM sessions s
           JOIN users u ON u.id = s.user_id
           JOIN namespaces n ON n.id = u.namespace_id
           WHERE s.id_hash = $1 AND s.expires_at > now() AND s.last_seen_at > now() - $2::interval"#,
        &id_hash[..],
        sqlx::postgres::types::PgInterval::try_from(idle).map_err(|_| DbError::Invalid("idle"))?,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| SessionRow {
        id_hash: r.id_hash,
        user_id: UserId::from_uuid(r.user_id),
        user_name: r.user_name,
        user_kind: crate::users::kind(&r.kind),
        user_state: crate::users::state(&r.state),
        is_admin: r.is_admin,
        theme: r.theme,
        created_at: r.created_at,
        last_seen_at: r.last_seen_at,
        expires_at: r.expires_at,
        auth_level: AuthLevel::parse(&r.auth_level),
        sudo_until: r.sudo_until,
        ip: r.ip,
        user_agent: r.user_agent,
    }))
}

/// Records activity at most once per minute.
pub async fn touch(exec: impl PgExecutor<'_>, id_hash: &[u8]) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE sessions SET last_seen_at = now()
         WHERE id_hash = $1 AND last_seen_at < now() - interval '1 minute'",
        id_hash
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn delete(exec: impl PgExecutor<'_>, id_hash: &[u8]) -> Result<bool, DbError> {
    let r = sqlx::query!("DELETE FROM sessions WHERE id_hash = $1", id_hash)
        .execute(exec)
        .await?;
    Ok(r.rows_affected() > 0)
}

/// Deletes all sessions of a user, optionally keeping one.
pub async fn delete_all_for_user(
    exec: impl PgExecutor<'_>,
    user: UserId,
    keep: Option<&[u8]>,
) -> Result<u64, DbError> {
    let r = sqlx::query!(
        "DELETE FROM sessions WHERE user_id = $1 AND ($2::bytea IS NULL OR id_hash <> $2)",
        user.as_uuid(),
        keep
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected())
}

/// Removes expired sessions (periodic cleanup).
pub async fn purge_expired(
    exec: impl PgExecutor<'_>,
    idle: time::Duration,
) -> Result<u64, DbError> {
    let r = sqlx::query!(
        "DELETE FROM sessions WHERE expires_at <= now() OR last_seen_at <= now() - $1::interval",
        sqlx::postgres::types::PgInterval::try_from(idle).map_err(|_| DbError::Invalid("idle"))?,
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected())
}

/// Start of the 15-minute window containing now.
const WINDOW: &str = "15 minutes";

/// Counts a failed sign-in for each key and returns the new counts.
pub async fn record_failures(
    exec: impl PgExecutor<'_>,
    keys: &[String],
) -> Result<Vec<i32>, DbError> {
    let counts = sqlx::query_scalar!(
        r#"INSERT INTO login_attempts (key, window_start, count)
           SELECT k, date_bin($2::text::interval, now(), 'epoch'), 1 FROM unnest($1::text[]) AS k
           ON CONFLICT (key, window_start) DO UPDATE SET count = login_attempts.count + 1
           RETURNING count"#,
        keys,
        WINDOW,
    )
    .fetch_all(exec)
    .await?;
    Ok(counts)
}

/// Failures recorded for `key` in the current window.
pub async fn failures(exec: impl PgExecutor<'_>, key: &str) -> Result<i32, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT COALESCE(sum(count), 0)::int AS "n!" FROM login_attempts
           WHERE key = $1 AND window_start = date_bin($2::text::interval, now(), 'epoch')"#,
        key,
        WINDOW,
    )
    .fetch_one(exec)
    .await?)
}

/// Forgets the failures for `key` (after a successful sign-in).
pub async fn clear_failures(exec: impl PgExecutor<'_>, key: &str) -> Result<(), DbError> {
    sqlx::query!("DELETE FROM login_attempts WHERE key = $1", key)
        .execute(exec)
        .await?;
    Ok(())
}

/// Removes counters of past windows.
pub async fn purge_attempts(exec: impl PgExecutor<'_>) -> Result<u64, DbError> {
    let r =
        sqlx::query!("DELETE FROM login_attempts WHERE window_start < now() - interval '1 hour'")
            .execute(exec)
            .await?;
    Ok(r.rows_affected())
}

/// One session in a user's session list.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id_hash: Vec<u8>,
    pub created_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

/// The user's live sessions, most recently used first.
pub async fn list_for_user(
    exec: impl PgExecutor<'_>,
    user: UserId,
    idle: time::Duration,
) -> Result<Vec<SessionSummary>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT id_hash, created_at, last_seen_at, host(ip) AS ip, user_agent FROM sessions
           WHERE user_id = $1 AND expires_at > now() AND last_seen_at > now() - $2::interval
           ORDER BY last_seen_at DESC LIMIT 200"#,
        user.as_uuid(),
        sqlx::postgres::types::PgInterval::try_from(idle).map_err(|_| DbError::Invalid("idle"))?,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| SessionSummary {
            id_hash: r.id_hash,
            created_at: r.created_at,
            last_seen_at: r.last_seen_at,
            ip: r.ip,
            user_agent: r.user_agent,
        })
        .collect())
}

/// Deletes one of the user's own sessions.
pub async fn delete_for_user(
    exec: impl PgExecutor<'_>,
    user: UserId,
    id_hash: &[u8],
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "DELETE FROM sessions WHERE user_id = $1 AND id_hash = $2",
        user.as_uuid(),
        id_hash
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Starts (or extends) sudo mode for a session.
pub async fn set_sudo(
    exec: impl PgExecutor<'_>,
    id_hash: &[u8],
    until: OffsetDateTime,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE sessions SET sudo_until = $2 WHERE id_hash = $1",
        id_hash,
        until
    )
    .execute(exec)
    .await?;
    Ok(())
}
