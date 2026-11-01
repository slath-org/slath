// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Background migrations: long backfills that run in batches while the
//! server is up, instead of inside a schema migration.
//!
//! Each batch runs in one transaction together with the update of the
//! migration's cursor and counters, after locking the migration's row. A
//! batch that is interrupted (crash, kill, error) leaves no trace, and the
//! next run repeats it from the same cursor, so no effect happens twice.
//! Side effects outside the database must be idempotent for the same reason.
//!
//! Migrations are registered by the service layer (some need Git); their
//! state lives in `background_migrations`. Features that depend on an
//! unfinished migration check [`is_done`] and degrade visibly.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;

use crate::DbError;

/// Result of one batch.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    /// Cursor to continue from; `None` when the migration is complete.
    pub cursor: Option<Value>,
    /// Items processed by this batch (for progress).
    pub processed: i64,
}

pub type BatchFuture<'a> = Pin<Box<dyn Future<Output = Result<Batch, String>> + Send + 'a>>;
pub type EstimateFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<i64>, String>> + Send + 'a>>;

/// One background migration. Implementations live in
/// `crates/db/src/bgmigrations/NNNN_name.rs` (database-only) or in the
/// service crate (when they need Git).
pub trait BackgroundMigration: Send + Sync {
    /// Unique, stable name (`mr_change_patch_ids`).
    fn name(&self) -> &'static str;
    /// Release that introduced it.
    fn introduced_in(&self) -> &'static str;
    /// Rough number of items to process, for progress and ETA.
    fn estimate<'a>(&'a self, pool: &'a PgPool) -> EstimateFuture<'a>;
    /// Processes up to `limit` items after `cursor` inside `tx`.
    fn run_batch<'a>(
        &'a self,
        tx: &'a mut PgConnection,
        cursor: Option<&'a Value>,
        limit: i64,
    ) -> BatchFuture<'a>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Pending,
    Running,
    Paused,
    Done,
    Failed,
}

impl State {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "paused" => Self::Paused,
            "done" => Self::Done,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

/// A migration's persisted state.
#[derive(Debug, Clone)]
pub struct Status {
    pub name: String,
    pub introduced_in: String,
    pub state: State,
    pub cursor: Option<Value>,
    pub processed: i64,
    pub total_estimate: Option<i64>,
    pub last_error: Option<String>,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
}

impl Status {
    /// Remaining time at the average rate so far, if it can be estimated.
    #[must_use]
    pub fn eta(&self, now: OffsetDateTime) -> Option<Duration> {
        let total = self.total_estimate?;
        let started = self.started_at?;
        if self.state == State::Done || self.processed <= 0 {
            return None;
        }
        let elapsed = (now - started).as_seconds_f64().max(0.0);
        let left = (total - self.processed).max(0) as f64;
        let rate = self.processed as f64 / elapsed.max(1e-3);
        Some(Duration::from_secs_f64(left / rate.max(1e-9)))
    }

    /// Progress in percent (capped at 99 until done).
    #[must_use]
    pub fn percent(&self) -> Option<u8> {
        if self.state == State::Done {
            return Some(100);
        }
        let total = self.total_estimate.filter(|t| *t > 0)?;
        let p = (self.processed.max(0) * 100 / total).min(99);
        u8::try_from(p).ok()
    }
}

macro_rules! to_status {
    ($r:expr) => {
        Status {
            name: $r.name,
            introduced_in: $r.introduced_in,
            state: State::parse(&$r.state),
            cursor: $r.cursor,
            processed: $r.processed,
            total_estimate: $r.total_estimate,
            last_error: $r.last_error,
            started_at: $r.started_at,
            finished_at: $r.finished_at,
        }
    };
}

/// Records a migration as pending (idempotent; existing rows are kept).
pub async fn register(pool: &PgPool, m: &dyn BackgroundMigration) -> Result<bool, DbError> {
    let total = m
        .estimate(pool)
        .await
        .map_err(|e| DbError::BackgroundMigration(m.name(), e))?;
    let done = sqlx::query!(
        "INSERT INTO background_migrations (name, introduced_in, total_estimate)
         VALUES ($1, $2, $3) ON CONFLICT (name) DO NOTHING",
        m.name(),
        m.introduced_in(),
        total
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn list(pool: &PgPool) -> Result<Vec<Status>, DbError> {
    let rows = sqlx::query!(
        "SELECT name, introduced_in, state, cursor, processed, total_estimate, last_error,
                started_at, finished_at
         FROM background_migrations ORDER BY name"
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| to_status!(r)).collect())
}

pub async fn get(pool: &PgPool, name: &str) -> Result<Option<Status>, DbError> {
    let r = sqlx::query!(
        "SELECT name, introduced_in, state, cursor, processed, total_estimate, last_error,
                started_at, finished_at
         FROM background_migrations WHERE name = $1",
        name
    )
    .fetch_optional(pool)
    .await?;
    Ok(r.map(|r| to_status!(r)))
}

/// True once the migration has finished. Unknown names count as not done.
pub async fn is_done(pool: &PgPool, name: &str) -> Result<bool, DbError> {
    let s: Option<String> = sqlx::query_scalar!(
        "SELECT state FROM background_migrations WHERE name = $1",
        name
    )
    .fetch_optional(pool)
    .await?;
    Ok(s.as_deref() == Some("done"))
}

/// Requested state change from an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Pause,
    Resume,
    /// Failed → pending, keeping the cursor.
    Retry,
}

/// Applies an operator's control. Returns false if the migration doesn't
/// exist or isn't in a state the control applies to.
pub async fn control(pool: &PgPool, name: &str, c: Control) -> Result<bool, DbError> {
    let (from, to): (&[&str], &str) = match c {
        Control::Pause => (&["pending", "running"], "paused"),
        Control::Resume => (&["paused"], "pending"),
        Control::Retry => (&["failed"], "pending"),
    };
    let from: Vec<String> = from.iter().map(|s| (*s).to_owned()).collect();
    let done = sqlx::query!(
        "UPDATE background_migrations SET state = $3, last_error = CASE WHEN $3 = 'pending' THEN NULL ELSE last_error END,
                updated_at = now()
         WHERE name = $1 AND state = ANY($2)",
        name,
        &from,
        to
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// What one call of [`step`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// A batch was committed.
    Progress { processed: i64, done: bool },
    /// Nothing to do: paused, done, failed, or locked by another runner.
    Idle,
    /// The batch failed; the migration is marked failed.
    Failed(String),
}

/// Runs one batch of `m` in a transaction that also advances its cursor.
pub async fn step(pool: &PgPool, m: &dyn BackgroundMigration, limit: i64) -> Result<Step, DbError> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query!(
        "SELECT state, cursor FROM background_migrations
         WHERE name = $1 AND state IN ('pending', 'running')
         FOR UPDATE SKIP LOCKED",
        m.name()
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Ok(Step::Idle);
    };
    match m
        .run_batch(&mut tx, row.cursor.as_ref(), limit.max(1))
        .await
    {
        Ok(batch) => {
            let done = batch.cursor.is_none();
            sqlx::query!(
                "UPDATE background_migrations
                 SET state = CASE WHEN $2 THEN 'done' ELSE 'running' END,
                     cursor = $3, processed = processed + $4, last_error = NULL,
                     started_at = coalesce(started_at, now()),
                     finished_at = CASE WHEN $2 THEN now() ELSE NULL END,
                     updated_at = now()
                 WHERE name = $1",
                m.name(),
                done,
                batch.cursor,
                batch.processed
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(Step::Progress {
                processed: batch.processed,
                done,
            })
        }
        Err(e) => {
            drop(tx);
            sqlx::query!(
                "UPDATE background_migrations SET state = 'failed', last_error = $2, updated_at = now()
                 WHERE name = $1 AND state IN ('pending', 'running')",
                m.name(),
                e.chars().take(2000).collect::<String>()
            )
            .execute(pool)
            .await?;
            Ok(Step::Failed(e))
        }
    }
}

/// Runs `m` until it is done, paused, failed, or `stop` returns true,
/// sleeping `interval` between batches (rate limiting).
pub async fn run_to_end(
    pool: &PgPool,
    m: &dyn BackgroundMigration,
    limit: i64,
    interval: Duration,
    stop: &(dyn Fn() -> bool + Send + Sync),
) -> Result<Step, DbError> {
    let mut last = Step::Idle;
    loop {
        if stop() {
            return Ok(last);
        }
        match step(pool, m, limit).await? {
            Step::Progress { done: true, .. } => {
                return Ok(Step::Progress {
                    processed: 0,
                    done: true,
                });
            }
            s @ Step::Progress { .. } => last = s,
            other => return Ok(other),
        }
        if !interval.is_zero() {
            tokio::time::sleep(interval).await;
        }
    }
}

/// One `slath upgrade` (or `serve --migrate`) run.
#[derive(Debug, Clone)]
pub struct UpgradeRecord {
    pub from_version: Option<String>,
    pub to_version: String,
    pub from_schema: i32,
    pub to_schema: i32,
    pub backup_path: Option<String>,
    pub started_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
    pub outcome: Option<String>,
    pub error: Option<String>,
}

/// The most recent upgrades, newest first.
pub async fn upgrade_history(pool: &PgPool, limit: i64) -> Result<Vec<UpgradeRecord>, DbError> {
    let rows = sqlx::query_as!(
        UpgradeRecord,
        "SELECT from_version, to_version, from_schema, to_schema, backup_path, started_at,
                finished_at, outcome, error
         FROM upgrade_history ORDER BY started_at DESC LIMIT $1",
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
