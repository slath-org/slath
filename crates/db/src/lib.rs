// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! PostgreSQL access: connection pool, embedded migrations, the schema
//! version check, and the durable job queue.
#![forbid(unsafe_code)]

pub mod audit;
pub mod backup;
pub mod idempotency;
pub mod jobs;
pub mod orgs;
pub mod refs;
pub mod repos;
pub mod sessions;
pub mod ssh_keys;
pub mod tokens;
pub mod users;
pub mod worker;

use std::time::Duration;

use slath_core::Secret;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
pub use sqlx::{PgPool, Postgres, Transaction};

/// Schema version this binary was built for: the number of the newest file
/// in `migrations/`. `serve` refuses to start on a mismatch unless it's told
/// to migrate.
pub const REQUIRED_SCHEMA: i32 = 8;

/// The migrations in `migrations/`, embedded at compile time.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// A uniqueness constraint failed; the field names what collided.
    #[error("{0} is already taken")]
    Conflict(&'static str),
    #[error("invalid value for {0}")]
    Invalid(&'static str),
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
    /// A backup can't be taken or restored.
    #[error("{0}")]
    Backup(String),
}

impl DbError {
    /// Maps a unique-constraint violation to [`DbError::Conflict`].
    #[must_use]
    pub fn from_unique(e: sqlx::Error, field: &'static str) -> Self {
        match &e {
            sqlx::Error::Database(d) if d.code().as_deref() == Some("23505") => {
                Self::Conflict(field)
            }
            _ => Self::Sqlx(e),
        }
    }

    /// True for a lost race that a retry may resolve (serialization failure, deadlock).
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Sqlx(sqlx::Error::Database(d))
            if matches!(d.code().as_deref(), Some("40001" | "40P01")))
    }
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

/// Migrations embedded in this binary that the database hasn't applied yet,
/// as `(version, description)`.
pub async fn pending_migrations(pool: &PgPool) -> Result<Vec<(i64, String)>, DbError> {
    let exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await?;
    let applied: Vec<i64> = if exists {
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success")
            .fetch_all(pool)
            .await?
    } else {
        Vec::new()
    };
    Ok(MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration() && !applied.contains(&m.version))
        .map(|m| (m.version, m.description.to_string()))
        .collect())
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
