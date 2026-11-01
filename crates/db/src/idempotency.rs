// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Idempotency keys for mutating API requests.

use sqlx::PgExecutor;

use crate::DbError;

/// What happened when a request claimed a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// The key is new: run the request, then [`complete`] it.
    Fresh,
    /// The same request already finished: replay this response.
    Replay {
        status: i32,
        response: serde_json::Value,
    },
    /// The same request is still running.
    InProgress,
    /// The key was used for a different request.
    Mismatch,
}

/// Claims `(principal, key)` for a request, or reports an earlier use
/// within the last 24 hours.
pub async fn claim(
    exec: impl PgExecutor<'_> + Copy,
    principal: uuid::Uuid,
    key: &str,
    method: &str,
    path: &str,
    request_sha256: &[u8],
) -> Result<Claim, DbError> {
    sqlx::query!(
        "DELETE FROM idempotency_keys WHERE principal_id = $1 AND key = $2 AND created_at < now() - interval '24 hours'",
        principal,
        key
    )
    .execute(exec)
    .await?;
    let inserted = sqlx::query!(
        "INSERT INTO idempotency_keys (principal_id, key, method, path, request_sha256)
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        principal,
        key,
        method,
        path,
        request_sha256
    )
    .execute(exec)
    .await?;
    if inserted.rows_affected() == 1 {
        return Ok(Claim::Fresh);
    }
    let Some(row) = sqlx::query!(
        "SELECT method, path, request_sha256, status, response FROM idempotency_keys WHERE principal_id = $1 AND key = $2",
        principal,
        key
    )
    .fetch_optional(exec)
    .await?
    else {
        return Ok(Claim::InProgress);
    };
    if row.method != method || row.path != path || row.request_sha256 != request_sha256 {
        return Ok(Claim::Mismatch);
    }
    Ok(match (row.status, row.response) {
        (Some(status), Some(response)) => Claim::Replay { status, response },
        _ => Claim::InProgress,
    })
}

/// Stores the response of a claimed request.
pub async fn complete(
    exec: impl PgExecutor<'_>,
    principal: uuid::Uuid,
    key: &str,
    status: i32,
    response: &serde_json::Value,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE idempotency_keys SET status = $3, response = $4 WHERE principal_id = $1 AND key = $2",
        principal,
        key,
        status,
        response
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Releases a claim (the request failed in a way a retry may fix).
pub async fn release(
    exec: impl PgExecutor<'_>,
    principal: uuid::Uuid,
    key: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "DELETE FROM idempotency_keys WHERE principal_id = $1 AND key = $2 AND status IS NULL",
        principal,
        key
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Removes keys older than 24 hours.
pub async fn purge(exec: impl PgExecutor<'_>) -> Result<u64, DbError> {
    let r =
        sqlx::query!("DELETE FROM idempotency_keys WHERE created_at < now() - interval '24 hours'")
            .execute(exec)
            .await?;
    Ok(r.rows_affected())
}
