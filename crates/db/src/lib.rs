// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! PostgreSQL access: connection pool, embedded migrations, the schema
//! version check, and the durable job queue.
#![forbid(unsafe_code)]

pub mod audit;
pub mod backup;
pub mod bgmigrations;
pub mod blobs;
pub mod email_tokens;
pub mod forks;
pub mod idempotency;
pub mod imports;
pub mod invitations;
pub mod issues;
pub mod jobs;
pub mod labels;
pub mod merge_requests;
pub mod mfa;
pub mod mr_changes;
pub mod mr_ledger;
pub mod mr_reviews;
pub mod mr_threads;
pub mod notifications;
pub mod oidc;
pub mod orgs;
pub mod protections;
pub mod references;
pub mod refs;
pub mod repos;
pub mod search;
pub mod secret_allowances;
pub mod secrets;
pub mod sessions;
pub mod settings;
pub mod ssh_keys;
pub mod statuses;
pub mod teams;
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
pub const REQUIRED_SCHEMA: i32 = 118;

/// Schema version of the baseline migration, where every supported
/// installation starts. Lower versions come from the 0.1/0.2 previews.
pub const BASELINE_SCHEMA: i32 = 100;

/// What to do with a database or backup from a preview release.
pub const PREVIEW_GUIDANCE: &str = "Releases before 0.3.0 were previews and can't be upgraded. \
     Keep running the preview, or install this version with an empty database and data \
     directory and bring the repositories over with \"Import a repository\" or `git push --mirror`.";

/// True if a database at `schema` was made by a preview release.
#[must_use]
pub const fn is_preview_schema(schema: i32) -> bool {
    schema > 0 && schema < BASELINE_SCHEMA
}

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
        "database schema is version {found}, from a preview release of Slath. {}",
        PREVIEW_GUIDANCE
    )]
    PreviewSchema { found: i32 },
    #[error("database schema is version {found}, newer than this binary ({required}). {guidance}")]
    SchemaAhead {
        found: i32,
        required: i32,
        /// How to get back: names the upgrade and its backup when known.
        guidance: String,
    },
    /// A background migration's own code failed.
    #[error("background migration {0}: {1}")]
    BackgroundMigration(&'static str, String),
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
    // A preview's migrations are gone from this binary; say so instead of
    // letting the migrator report a missing migration.
    if let Some(found) = schema_version(pool).await?
        && is_preview_schema(found)
    {
        return Err(DbError::PreviewSchema { found });
    }
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

/// What to tell someone running an older binary on a newer schema.
async fn downgrade_guidance(pool: &PgPool, schema: i32) -> String {
    const GENERIC: &str =
        "Install the newer Slath, or restore the pre-upgrade backup with the version that made it.";
    let row: Option<(Option<String>, String, Option<String>)> = sqlx::query_as(
        "SELECT from_version, to_version, backup_path FROM upgrade_history
         WHERE to_schema = $1 AND outcome = 'success' ORDER BY started_at DESC LIMIT 1",
    )
    .bind(schema)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);
    match row {
        Some((from, to, Some(path))) => format!(
            "This database was upgraded to schema {schema} by v{to}. Restore the pre-upgrade backup at {path} to return to v{}.",
            from.as_deref().unwrap_or("the previous version")
        ),
        Some((_, to, None)) => format!(
            "This database was upgraded to schema {schema} by v{to} without a backup. Install v{to} or newer."
        ),
        None => GENERIC.to_owned(),
    }
}

/// Fails unless the database schema is exactly [`REQUIRED_SCHEMA`].
pub async fn check_schema(pool: &PgPool) -> Result<(), DbError> {
    let found = schema_version(pool).await?.unwrap_or(0);
    if is_preview_schema(found) {
        return Err(DbError::PreviewSchema { found });
    }
    match found.cmp(&REQUIRED_SCHEMA) {
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Less => Err(DbError::SchemaBehind {
            found,
            required: REQUIRED_SCHEMA,
        }),
        std::cmp::Ordering::Greater => Err(DbError::SchemaAhead {
            found,
            required: REQUIRED_SCHEMA,
            guidance: downgrade_guidance(pool, found).await,
        }),
    }
}
