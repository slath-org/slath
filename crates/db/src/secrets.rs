// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Columns holding values encrypted at rest, and the bookkeeping for key
//! rotation: which master keys exist and which are retired.

use sqlx::{AssertSqlSafe, PgExecutor, PgPool};
use time::OffsetDateTime;

use crate::DbError;

/// A column of encrypted values with the column naming its key.
#[derive(Debug, Clone, Copy)]
pub struct EncryptedColumn {
    pub table: &'static str,
    /// Primary key; its text form is the row part of the associated data.
    pub id: &'static str,
    pub value: &'static str,
    pub key: &'static str,
}

/// Every encrypted column. A new one must be added here, or rotation
/// would leave it behind (the `encryption_keys` foreign keys then keep
/// the old key from being retired).
pub const ENCRYPTED_COLUMNS: &[EncryptedColumn] = &[EncryptedColumn {
    table: "totp_credentials",
    id: "user_id",
    value: "secret_enc",
    key: "key_id",
}];

/// One encrypted value.
#[derive(Debug, Clone)]
pub struct EncryptedRow {
    pub id: String,
    pub value: Vec<u8>,
    pub key_id: String,
}

/// Records a master key (idempotent). Called before a key encrypts anything.
pub async fn register_key(exec: impl PgExecutor<'_>, key_id: &str) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO encryption_keys (key_id) VALUES ($1)
         ON CONFLICT (key_id) DO UPDATE SET retired_at = NULL",
        key_id
    )
    .execute(exec)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct KeyInfo {
    pub key_id: String,
    pub created_at: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
}

pub async fn list_keys(exec: impl PgExecutor<'_>) -> Result<Vec<KeyInfo>, DbError> {
    let rows = sqlx::query_as!(
        KeyInfo,
        "SELECT key_id, created_at, retired_at FROM encryption_keys ORDER BY created_at"
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Up to `limit` rows of `col` not encrypted with `current`, after `after`
/// (by primary key text), so a rotation pass makes progress even past rows
/// it can't decrypt.
pub async fn rows_not_using(
    pool: &PgPool,
    col: EncryptedColumn,
    current: &str,
    after: Option<&str>,
    limit: i64,
) -> Result<Vec<EncryptedRow>, DbError> {
    let sql = format!(
        "SELECT {id}::text, {value}, {key} FROM {table}
         WHERE {key} <> $1 AND ($2::text IS NULL OR {id}::text > $2)
         ORDER BY {id}::text LIMIT $3",
        id = col.id,
        value = col.value,
        key = col.key,
        table = col.table
    );
    let rows: Vec<(String, Vec<u8>, String)> = sqlx::query_as(AssertSqlSafe(sql))
        .bind(current)
        .bind(after)
        .bind(limit)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(id, value, key_id)| EncryptedRow { id, value, key_id })
        .collect())
}

/// Replaces one value if it is still the one that was read (compare and
/// swap, so a concurrent change isn't overwritten). Returns whether it did.
pub async fn replace(
    pool: &PgPool,
    col: EncryptedColumn,
    row: &EncryptedRow,
    value: &[u8],
    key_id: &str,
) -> Result<bool, DbError> {
    let sql = format!(
        "UPDATE {table} SET {value} = $1, {key} = $2
         WHERE {id}::text = $3 AND {value} = $4 AND {key} = $5",
        id = col.id,
        value = col.value,
        key = col.key,
        table = col.table
    );
    let done = sqlx::query(AssertSqlSafe(sql))
        .bind(value)
        .bind(key_id)
        .bind(&row.id)
        .bind(&row.value)
        .bind(&row.key_id)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() == 1)
}

/// Rows of every encrypted column per key ID.
pub async fn usage(pool: &PgPool) -> Result<Vec<(String, i64)>, DbError> {
    let mut out: Vec<(String, i64)> = Vec::new();
    for col in ENCRYPTED_COLUMNS {
        let sql = format!(
            "SELECT {key}, count(*) FROM {table} GROUP BY {key}",
            key = col.key,
            table = col.table
        );
        let rows: Vec<(String, i64)> = sqlx::query_as(AssertSqlSafe(sql)).fetch_all(pool).await?;
        for (k, n) in rows {
            match out.iter_mut().find(|(id, _)| *id == k) {
                Some(e) => e.1 += n,
                None => out.push((k, n)),
            }
        }
    }
    Ok(out)
}

/// Marks keys other than `current` that no row uses any more as retired.
/// Returns the IDs retired by this call.
pub async fn retire_unused(pool: &PgPool, current: &str) -> Result<Vec<String>, DbError> {
    let used: Vec<String> = usage(pool).await?.into_iter().map(|(k, _)| k).collect();
    let retired = sqlx::query_scalar!(
        "UPDATE encryption_keys SET retired_at = now()
         WHERE key_id <> $1 AND retired_at IS NULL AND NOT (key_id = ANY($2))
         RETURNING key_id",
        current,
        &used
    )
    .fetch_all(pool)
    .await?;
    Ok(retired)
}
