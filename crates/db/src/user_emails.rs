// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! An account's addresses besides its primary one (`users.email`).
//!
//! A confirmed address belongs to one account across both tables:
//! [`lock`] the address, check [`confirmed_owner`], then confirm it, all in
//! one transaction.

use slath_core::ids::UserId;
use sqlx::PgExecutor;

use crate::DbError;

/// One of an account's addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub email: String,
    pub verified: bool,
    pub primary: bool,
}

/// Every address of `user`: the primary one first, then the others in
/// the order they were added.
pub async fn list(exec: &mut sqlx::PgConnection, user: UserId) -> Result<Vec<Address>, DbError> {
    let mut out = Vec::new();
    let p = sqlx::query!(
        r#"SELECT email::text AS "email", email_verified FROM users WHERE id = $1"#,
        user.as_uuid()
    )
    .fetch_optional(&mut *exec)
    .await?;
    if let Some(p) = p
        && let Some(email) = p.email
    {
        out.push(Address {
            email,
            verified: p.email_verified,
            primary: true,
        });
    }
    let rows = sqlx::query!(
        r#"SELECT email::text AS "email!", verified FROM user_emails
           WHERE user_id = $1 ORDER BY created_at, email"#,
        user.as_uuid()
    )
    .fetch_all(&mut *exec)
    .await?;
    out.extend(rows.into_iter().map(|r| Address {
        email: r.email,
        verified: r.verified,
        primary: false,
    }));
    Ok(out)
}

/// Adds an unconfirmed address that isn't primary. A conflict if the
/// account already has it.
pub async fn insert(exec: impl PgExecutor<'_>, user: UserId, email: &str) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO user_emails (user_id, email) VALUES ($1, $2::text::citext)",
        user.as_uuid(),
        email
    )
    .execute(exec)
    .await
    .map_err(|e| DbError::from_unique(e, "email"))?;
    Ok(())
}

/// Adds an address that isn't primary, confirmed (by an administrator).
/// A conflict if another account confirmed it.
pub async fn insert_confirmed(
    exec: impl PgExecutor<'_>,
    user: UserId,
    email: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO user_emails (user_id, email, verified) VALUES ($1, $2::text::citext, true)
         ON CONFLICT (user_id, email) DO UPDATE SET verified = true",
        user.as_uuid(),
        email
    )
    .execute(exec)
    .await
    .map_err(|e| DbError::from_unique(e, "email"))?;
    Ok(())
}

/// Removes an address that isn't primary. Returns whether there was one.
pub async fn remove(exec: impl PgExecutor<'_>, user: UserId, email: &str) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "DELETE FROM user_emails WHERE user_id = $1 AND email = $2::text::citext",
        user.as_uuid(),
        email
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// Marks an address that isn't primary confirmed. Returns whether the
/// account has it.
pub async fn confirm(
    exec: impl PgExecutor<'_>,
    user: UserId,
    email: &str,
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "UPDATE user_emails SET verified = true WHERE user_id = $1 AND email = $2::text::citext",
        user.as_uuid(),
        email
    )
    .execute(exec)
    .await
    .map_err(|e| DbError::from_unique(e, "email"))?;
    Ok(r.rows_affected() == 1)
}

/// Addresses besides the primary one.
pub async fn count(exec: impl PgExecutor<'_>, user: UserId) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM user_emails WHERE user_id = $1"#,
        user.as_uuid()
    )
    .fetch_one(exec)
    .await?)
}

/// Holds the address until the transaction ends, so two accounts can't
/// confirm it at once.
pub async fn lock(exec: impl PgExecutor<'_>, email: &str) -> Result<(), DbError> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtext(lower($1::text)))",
        email
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// The account that confirmed this address, as its primary address or
/// another one.
pub async fn confirmed_owner(
    exec: impl PgExecutor<'_>,
    email: &str,
) -> Result<Option<UserId>, DbError> {
    let r = sqlx::query_scalar!(
        r#"SELECT id AS "id!" FROM users WHERE email = $1::text::citext AND email_verified
           UNION ALL
           SELECT user_id AS "id!" FROM user_emails WHERE email = $1::text::citext AND verified
           LIMIT 1"#,
        email
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(UserId::from_uuid))
}

/// Makes a confirmed address that isn't primary the primary one; the old
/// primary address becomes one of the others, keeping whether it was
/// confirmed. Returns false if the account has no such confirmed address.
pub async fn make_primary(
    exec: &mut sqlx::PgConnection,
    user: UserId,
    email: &str,
) -> Result<bool, DbError> {
    let taken = sqlx::query!(
        "DELETE FROM user_emails WHERE user_id = $1 AND email = $2::text::citext AND verified",
        user.as_uuid(),
        email
    )
    .execute(&mut *exec)
    .await?;
    if taken.rows_affected() != 1 {
        return Ok(false);
    }
    sqlx::query!(
        "INSERT INTO user_emails (user_id, email, verified)
         SELECT id, email, email_verified FROM users WHERE id = $1 AND email IS NOT NULL",
        user.as_uuid()
    )
    .execute(&mut *exec)
    .await?;
    sqlx::query!(
        "UPDATE users SET email = $2::text::citext, email_verified = true, updated_at = now() WHERE id = $1",
        user.as_uuid(),
        email
    )
    .execute(&mut *exec)
    .await?;
    Ok(true)
}
