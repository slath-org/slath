// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Single-use emailed tokens: password resets and email verifications.

use slath_core::ids::UserId;
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

pub async fn insert_reset(
    exec: impl PgExecutor<'_>,
    hash: &[u8],
    user: UserId,
    expires_at: OffsetDateTime,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO password_resets (token_hash, user_id, expires_at) VALUES ($1, $2, $3)",
        hash,
        user.as_uuid(),
        expires_at
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// The user of a live (unused, unexpired) reset token.
pub async fn reset_user(exec: impl PgExecutor<'_>, hash: &[u8]) -> Result<Option<UserId>, DbError> {
    let r = sqlx::query_scalar!(
        "SELECT user_id FROM password_resets WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()",
        hash
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(UserId::from_uuid))
}

/// Uses a reset token (once). Returns its user, or `None` if it was used,
/// expired, or unknown. Other live tokens of the user are used up too.
pub async fn use_reset(
    exec: &mut sqlx::PgConnection,
    hash: &[u8],
) -> Result<Option<UserId>, DbError> {
    let user = sqlx::query_scalar!(
        "UPDATE password_resets SET used_at = now()
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now() RETURNING user_id",
        hash
    )
    .fetch_optional(&mut *exec)
    .await?;
    if let Some(u) = user {
        sqlx::query!(
            "UPDATE password_resets SET used_at = now() WHERE user_id = $1 AND used_at IS NULL",
            u
        )
        .execute(&mut *exec)
        .await?;
    }
    Ok(user.map(UserId::from_uuid))
}

/// Live reset tokens a user requested since `since` (rate limit).
pub async fn recent_resets(
    exec: impl PgExecutor<'_>,
    user: UserId,
    since: OffsetDateTime,
) -> Result<i64, DbError> {
    let n = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM password_resets WHERE user_id = $1 AND created_at > $2"#,
        user.as_uuid(),
        since
    )
    .fetch_one(exec)
    .await?;
    Ok(n)
}

pub async fn insert_verification(
    exec: impl PgExecutor<'_>,
    hash: &[u8],
    user: UserId,
    email: &str,
    expires_at: OffsetDateTime,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO email_verifications (token_hash, user_id, email, expires_at) VALUES ($1, $2, $3, $4)",
        hash,
        user.as_uuid(),
        email,
        expires_at
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// A live verification: user and address.
pub async fn verification(
    exec: impl PgExecutor<'_>,
    hash: &[u8],
) -> Result<Option<(UserId, String)>, DbError> {
    let r = sqlx::query!(
        r#"SELECT user_id, email::text AS "email!" FROM email_verifications
           WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()"#,
        hash
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| (UserId::from_uuid(r.user_id), r.email)))
}

/// Uses a verification token and marks the address verified, if it is
/// still the user's address. Returns false for a stale token; a conflict if
/// another account already verified the address.
pub async fn use_verification(exec: &mut sqlx::PgConnection, hash: &[u8]) -> Result<bool, DbError> {
    let Some(r) = sqlx::query!(
        r#"UPDATE email_verifications SET used_at = now()
           WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()
           RETURNING user_id, email::text AS "email!""#,
        hash
    )
    .fetch_optional(&mut *exec)
    .await?
    else {
        return Ok(false);
    };
    let done = sqlx::query!(
        "UPDATE users SET email_verified = true, updated_at = now()
         WHERE id = $1 AND email = $2::text::citext",
        r.user_id,
        r.email
    )
    .execute(&mut *exec)
    .await
    .map_err(|e| DbError::from_unique(e, "email"))?;
    Ok(done.rows_affected() == 1)
}

/// The user with this verified address (sign-in by email, resets).
pub async fn user_by_verified_email(
    exec: impl PgExecutor<'_>,
    email: &str,
) -> Result<Option<UserId>, DbError> {
    let r = sqlx::query_scalar!(
        "SELECT id FROM users WHERE email = $1::text::citext AND email_verified AND state = 'active' AND kind = 'human'",
        email
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(UserId::from_uuid))
}

/// Sets a new (unverified) address.
pub async fn set_email(
    exec: impl PgExecutor<'_>,
    user: UserId,
    email: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE users SET email = $2::text::citext, email_verified = false, updated_at = now() WHERE id = $1",
        user.as_uuid(),
        email
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Whether the user's current address is verified.
pub async fn email_status(
    exec: impl PgExecutor<'_>,
    user: UserId,
) -> Result<(Option<String>, bool), DbError> {
    let r = sqlx::query!(
        r#"SELECT email::text AS email, email_verified FROM users WHERE id = $1"#,
        user.as_uuid()
    )
    .fetch_one(exec)
    .await?;
    Ok((r.email, r.email_verified))
}
