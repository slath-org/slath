// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Second factors: TOTP credentials (seeds encrypted by the service layer),
//! recovery codes (Argon2id hashes), and pending second-factor sign-ins.

use serde_json::Value;
use slath_core::ids::UserId;
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

/// A stored TOTP credential.
#[derive(Debug, Clone)]
pub struct Totp {
    pub secret_enc: Vec<u8>,
    pub key_id: String,
    pub last_used_step: i64,
    pub confirmed: bool,
    pub created_at: OffsetDateTime,
}

pub async fn totp(exec: impl PgExecutor<'_>, user: UserId) -> Result<Option<Totp>, DbError> {
    let r = sqlx::query_as!(
        Totp,
        "SELECT secret_enc, key_id, last_used_step, confirmed, created_at
         FROM totp_credentials WHERE user_id = $1",
        user.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r)
}

/// Stores a new, unconfirmed credential (replacing an unconfirmed one).
/// Fails with a conflict if a confirmed one exists.
pub async fn put_pending_totp(
    exec: impl PgExecutor<'_>,
    user: UserId,
    secret_enc: &[u8],
    key_id: &str,
) -> Result<(), DbError> {
    let done = sqlx::query!(
        "INSERT INTO totp_credentials (user_id, secret_enc, key_id, confirmed)
         VALUES ($1, $2, $3, false)
         ON CONFLICT (user_id) DO UPDATE SET secret_enc = EXCLUDED.secret_enc,
             key_id = EXCLUDED.key_id, last_used_step = 0, created_at = now()
         WHERE NOT totp_credentials.confirmed",
        user.as_uuid(),
        secret_enc,
        key_id
    )
    .execute(exec)
    .await?;
    if done.rows_affected() == 0 {
        return Err(DbError::Conflict("totp"));
    }
    Ok(())
}

/// Records `step` as used if it is newer than the last used one (compare
/// and set, so two requests can't both use the same code). With `confirm`,
/// also marks the credential confirmed. Returns whether it did.
pub async fn use_step(
    exec: impl PgExecutor<'_>,
    user: UserId,
    step: i64,
    confirm: bool,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "UPDATE totp_credentials SET last_used_step = $2, confirmed = confirmed OR $3
         WHERE user_id = $1 AND last_used_step < $2 AND (confirmed OR $3)",
        user.as_uuid(),
        step,
        confirm
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn delete_totp(exec: impl PgExecutor<'_>, user: UserId) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "DELETE FROM totp_credentials WHERE user_id = $1",
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// True if the user has a confirmed second factor.
pub async fn has_second_factor(exec: impl PgExecutor<'_>, user: UserId) -> Result<bool, DbError> {
    let r = sqlx::query_scalar!(
        r#"SELECT (EXISTS (SELECT 1 FROM totp_credentials WHERE user_id = $1 AND confirmed)
                OR EXISTS (SELECT 1 FROM webauthn_credentials WHERE user_id = $1)) AS "x!""#,
        user.as_uuid()
    )
    .fetch_one(exec)
    .await?;
    Ok(r)
}

/// Replaces all recovery codes of a user.
pub async fn replace_recovery_codes(
    tx: &mut sqlx::PgConnection,
    user: UserId,
    hashes: &[String],
) -> Result<(), DbError> {
    sqlx::query!(
        "DELETE FROM recovery_codes WHERE user_id = $1",
        user.as_uuid()
    )
    .execute(&mut *tx)
    .await?;
    for h in hashes {
        sqlx::query!(
            "INSERT INTO recovery_codes (id, user_id, code_hash) VALUES ($1, $2, $3)",
            uuid::Uuid::now_v7(),
            user.as_uuid(),
            h
        )
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

pub async fn delete_recovery_codes(exec: impl PgExecutor<'_>, user: UserId) -> Result<(), DbError> {
    sqlx::query!(
        "DELETE FROM recovery_codes WHERE user_id = $1",
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Unused recovery codes as `(id, hash)`.
pub async fn unused_recovery_codes(
    exec: impl PgExecutor<'_>,
    user: UserId,
) -> Result<Vec<(uuid::Uuid, String)>, DbError> {
    let rows = sqlx::query!(
        "SELECT id, code_hash FROM recovery_codes WHERE user_id = $1 AND used_at IS NULL",
        user.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| (r.id, r.code_hash)).collect())
}

/// Marks a recovery code used, if it still is unused. Returns whether it did.
pub async fn use_recovery_code(exec: impl PgExecutor<'_>, id: uuid::Uuid) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "UPDATE recovery_codes SET used_at = now() WHERE id = $1 AND used_at IS NULL",
        id
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// A pending second-factor step.
#[derive(Debug, Clone)]
pub struct Challenge {
    pub user_id: UserId,
    pub purpose: String,
    pub first_factor: String,
    pub data: Value,
    pub failures: i32,
    pub expires_at: OffsetDateTime,
}

pub struct NewChallenge<'a> {
    pub id_hash: &'a [u8],
    pub user_id: UserId,
    pub purpose: &'a str,
    pub first_factor: &'a str,
    pub data: Value,
    pub expires_at: OffsetDateTime,
}

pub async fn insert_challenge(
    exec: impl PgExecutor<'_>,
    c: &NewChallenge<'_>,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO login_challenges (id_hash, user_id, purpose, first_factor, data, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
        c.id_hash,
        c.user_id.as_uuid(),
        c.purpose,
        c.first_factor,
        c.data,
        c.expires_at
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// A live (unexpired) challenge.
pub async fn challenge(
    exec: impl PgExecutor<'_>,
    id_hash: &[u8],
) -> Result<Option<Challenge>, DbError> {
    let r = sqlx::query!(
        "SELECT user_id, purpose, first_factor, data, failures, expires_at
         FROM login_challenges WHERE id_hash = $1 AND expires_at > now()",
        id_hash
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| Challenge {
        user_id: UserId::from_uuid(r.user_id),
        purpose: r.purpose,
        first_factor: r.first_factor,
        data: r.data,
        failures: r.failures,
        expires_at: r.expires_at,
    }))
}

/// Counts a wrong code; returns the new count.
pub async fn challenge_failed(exec: impl PgExecutor<'_>, id_hash: &[u8]) -> Result<i32, DbError> {
    let n = sqlx::query_scalar!(
        "UPDATE login_challenges SET failures = failures + 1 WHERE id_hash = $1 RETURNING failures",
        id_hash
    )
    .fetch_optional(exec)
    .await?;
    Ok(n.unwrap_or(i32::MAX))
}

/// Removes a challenge (used, given up, or exhausted). Returns whether it existed.
pub async fn delete_challenge(exec: impl PgExecutor<'_>, id_hash: &[u8]) -> Result<bool, DbError> {
    let done = sqlx::query!("DELETE FROM login_challenges WHERE id_hash = $1", id_hash)
        .execute(exec)
        .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn purge_challenges(exec: impl PgExecutor<'_>) -> Result<u64, DbError> {
    let done = sqlx::query!("DELETE FROM login_challenges WHERE expires_at < now()")
        .execute(exec)
        .await?;
    Ok(done.rows_affected())
}

/// A stored passkey.
#[derive(Debug, Clone)]
pub struct Passkey {
    pub id: uuid::Uuid,
    pub user_id: UserId,
    pub credential_id: Vec<u8>,
    pub passkey: Value,
    pub name: String,
    pub last_used_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

pub async fn passkeys(exec: impl PgExecutor<'_>, user: UserId) -> Result<Vec<Passkey>, DbError> {
    let rows = sqlx::query!(
        "SELECT id, user_id, credential_id, passkey, name, last_used_at, created_at
         FROM webauthn_credentials WHERE user_id = $1 ORDER BY created_at",
        user.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Passkey {
            id: r.id,
            user_id: UserId::from_uuid(r.user_id),
            credential_id: r.credential_id,
            passkey: r.passkey,
            name: r.name,
            last_used_at: r.last_used_at,
            created_at: r.created_at,
        })
        .collect())
}

pub async fn insert_passkey(
    exec: impl PgExecutor<'_>,
    user: UserId,
    credential_id: &[u8],
    passkey: &Value,
    name: &str,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO webauthn_credentials (id, user_id, credential_id, passkey, name)
         VALUES ($1, $2, $3, $4, $5)",
        id,
        user.as_uuid(),
        credential_id,
        passkey,
        name
    )
    .execute(exec)
    .await
    .map_err(|e| DbError::from_unique(e, "credential_id"))?;
    Ok(id)
}

/// Records a use: the updated credential (counters) and the time.
pub async fn passkey_used(
    exec: impl PgExecutor<'_>,
    id: uuid::Uuid,
    passkey: &Value,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE webauthn_credentials SET passkey = $2, last_used_at = now() WHERE id = $1",
        id,
        passkey
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn delete_passkey(
    exec: impl PgExecutor<'_>,
    user: UserId,
    id: uuid::Uuid,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "DELETE FROM webauthn_credentials WHERE id = $1 AND user_id = $2",
        id,
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn delete_passkeys(exec: impl PgExecutor<'_>, user: UserId) -> Result<u64, DbError> {
    let done = sqlx::query!(
        "DELETE FROM webauthn_credentials WHERE user_id = $1",
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected())
}

pub async fn insert_ceremony(
    exec: impl PgExecutor<'_>,
    id_hash: &[u8],
    user: Option<UserId>,
    kind: &str,
    state: &Value,
    expires_at: OffsetDateTime,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO webauthn_ceremonies (id_hash, user_id, kind, state, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
        id_hash,
        user.map(|u| *u.as_uuid()),
        kind,
        state,
        expires_at
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Takes (deletes and returns) a live ceremony: each can be finished once.
pub async fn take_ceremony(
    exec: impl PgExecutor<'_>,
    id_hash: &[u8],
    kind: &str,
) -> Result<Option<(Option<UserId>, Value)>, DbError> {
    let r = sqlx::query!(
        "DELETE FROM webauthn_ceremonies WHERE id_hash = $1 AND kind = $2 AND expires_at > now()
         RETURNING user_id, state",
        id_hash,
        kind
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| (r.user_id.map(UserId::from_uuid), r.state)))
}
