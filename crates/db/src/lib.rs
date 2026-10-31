// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! PostgreSQL access: connection pool, embedded migrations, the schema
//! version check, and the durable job queue.
#![forbid(unsafe_code)]

pub mod jobs;
pub mod worker;

use std::time::Duration;

use slath_core::Secret;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
pub use sqlx::{PgPool, Postgres, Transaction};

/// Schema version this binary was built for: the number of the newest file
/// in `migrations/`. `serve` refuses to start on a mismatch unless it's told
/// to migrate.
pub const REQUIRED_SCHEMA: i32 = 1;

/// The migrations in `migrations/`, embedded at compile time.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error(
        "database schema is version {found}, this binary needs {required}; \
         run `slath migrate` (or `slath upgrade`) first"
    )]
    SchemaBehind { found: i32, required: i32 },
    #[error(
        "database schema is version {found}, newer than this binary ({required}); \
         install a newer Slath or restore the pre-upgrade backup"
    )]
    SchemaAhead { found: i32, required: i32 },
}

/// Pool settings from `[database]`.
#[derive(Debug)]
pub struct PoolConfig {
    pub url: Secret<String>,
    pub max_connections: u32,
    pub acquire_timeout: Duration,
}

impl PoolConfig {
    #[must_use]
    pub fn new(url: Secret<String>) -> Self {
        Self {
            url,
            max_connections: 20,
            acquire_timeout: Duration::from_secs(5),
        }
    }
}

/// Opens a connection pool. The URL is never logged.
pub async fn connect(cfg: &PoolConfig) -> Result<PgPool, DbError> {
    let opts: PgConnectOptions = cfg.url.expose().parse()?;
    let opts = opts.application_name("slath");
    let pool = PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .acquire_timeout(cfg.acquire_timeout)
        .connect_with(opts)
        .await?;
    Ok(pool)
}

/// Applies all pending migrations.
pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

/// Reads `schema_info.version`; `None` on an empty database.
pub async fn schema_version(pool: &PgPool) -> Result<Option<i32>, DbError> {
    let exists: bool =
        sqlx::query_scalar!(r#"SELECT to_regclass('public.schema_info') IS NOT NULL AS "exists!""#)
            .fetch_one(pool)
            .await?;
    if !exists {
        return Ok(None);
    }
    let v = sqlx::query_scalar!("SELECT version FROM schema_info")
        .fetch_optional(pool)
        .await?;
    Ok(v)
}

/// Fails unless the database schema is exactly [`REQUIRED_SCHEMA`].
pub async fn check_schema(pool: &PgPool) -> Result<(), DbError> {
    let found = schema_version(pool).await?.unwrap_or(0);
    match found.cmp(&REQUIRED_SCHEMA) {
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Less => Err(DbError::SchemaBehind {
            found,
            required: REQUIRED_SCHEMA,
        }),
        std::cmp::Ordering::Greater => Err(DbError::SchemaAhead {
            found,
            required: REQUIRED_SCHEMA,
        }),
    }
}
