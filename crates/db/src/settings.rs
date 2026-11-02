// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Instance-wide settings (`instance_settings`): small JSON values by key.

use serde_json::Value;
use sqlx::PgExecutor;

use crate::DbError;

/// Read-only maintenance during an upgrade: `{"reason": …, "since": …}`.
pub const MAINTENANCE: &str = "maintenance";
/// Version of the last binary that served this database: `"0.2.0"`.
pub const SERVED_BY: &str = "served_by_version";
/// An upgrade that started and hasn't finished successfully:
/// `{"from_version": …, "from_schema": …, "to_version": …}`.
pub const UPGRADE_IN_PROGRESS: &str = "upgrade_in_progress";

pub async fn get(exec: impl PgExecutor<'_>, key: &str) -> Result<Option<Value>, DbError> {
    let v = sqlx::query_scalar!("SELECT value FROM instance_settings WHERE key = $1", key)
        .fetch_optional(exec)
        .await?;
    Ok(v)
}

pub async fn set(exec: impl PgExecutor<'_>, key: &str, value: &Value) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO instance_settings (key, value, updated_at) VALUES ($1, $2, now())
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
        key,
        value
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn delete(exec: impl PgExecutor<'_>, key: &str) -> Result<bool, DbError> {
    let r = sqlx::query!("DELETE FROM instance_settings WHERE key = $1", key)
        .execute(exec)
        .await?;
    Ok(r.rows_affected() == 1)
}
