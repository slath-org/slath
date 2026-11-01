// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The durable job queue.
//!
//! Jobs move `ready → running → done`, or back to `ready` with exponential
//! backoff on failure, and to `dead` after `max_attempts`. A worker holds a
//! lease (`locked_by`, `locked_until`) and must renew it with [`heartbeat`];
//! a job whose lease expired is leased again by the next worker. Every state
//! change by a worker is conditional on still holding the lease, so a worker
//! that lost its lease can't overwrite the newer attempt's outcome.

use std::time::Duration;

use sqlx::PgExecutor;
use sqlx::postgres::types::PgInterval;
use time::OffsetDateTime;

use crate::DbError;

/// `NOTIFY` channel used to wake workers; the payload is the queue name.
pub const NOTIFY_CHANNEL: &str = "slath_jobs";

/// First retry delay; doubles per attempt up to [`MAX_BACKOFF`].
pub const BASE_BACKOFF: Duration = Duration::from_secs(5);
pub const MAX_BACKOFF: Duration = Duration::from_secs(3600);

/// A job to enqueue.
#[derive(Debug, Clone)]
pub struct NewJob {
    pub queue: String,
    pub kind: String,
    pub payload: serde_json::Value,
    /// Enqueueing a second job with the same key is a no-op.
    pub dedupe_key: Option<String>,
    /// Earliest start; `None` means now.
    pub run_at: Option<OffsetDateTime>,
    pub max_attempts: i32,
}

impl NewJob {
    #[must_use]
    pub fn new(queue: &str, kind: &str, payload: serde_json::Value) -> Self {
        Self {
            queue: queue.to_owned(),
            kind: kind.to_owned(),
            payload,
            dedupe_key: None,
            run_at: None,
            max_attempts: 8,
        }
    }

    #[must_use]
    pub fn dedupe_key(mut self, key: impl Into<String>) -> Self {
        self.dedupe_key = Some(key.into());
        self
    }

    #[must_use]
    pub fn run_at(mut self, at: OffsetDateTime) -> Self {
        self.run_at = Some(at);
        self
    }

    #[must_use]
    pub fn max_attempts(mut self, n: i32) -> Self {
        self.max_attempts = n;
        self
    }
}

/// A job leased by a worker.
#[derive(Debug, Clone)]
pub struct LeasedJob {
    pub id: i64,
    pub queue: String,
    pub kind: String,
    pub payload: serde_json::Value,
    /// Including the current attempt.
    pub attempts: i32,
    pub max_attempts: i32,
}

/// What happened to a failed job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailOutcome {
    Retry {
        run_at: OffsetDateTime,
    },
    Dead,
    /// The worker no longer held the lease; nothing was changed.
    LeaseLost,
}

fn interval(d: Duration) -> PgInterval {
    PgInterval {
        months: 0,
        days: 0,
        microseconds: i64::try_from(d.as_micros()).unwrap_or(i64::MAX),
    }
}

/// Retry delay after `attempts` failed attempts: 5 s, 10 s, 20 s, … capped at 1 h.
#[must_use]
pub fn backoff(attempts: i32) -> Duration {
    let exp = u32::try_from(attempts.saturating_sub(1))
        .unwrap_or(0)
        .min(20);
    BASE_BACKOFF.saturating_mul(1 << exp).min(MAX_BACKOFF)
}

/// Enqueues a job, in the caller's transaction if one is passed. Returns the
/// new ID, or `None` if a job with the same dedupe key already exists.
/// Workers are woken by `NOTIFY` when the transaction commits.
pub async fn enqueue<'e>(exec: impl PgExecutor<'e>, job: &NewJob) -> Result<Option<i64>, DbError> {
    let id = sqlx::query_scalar!(
        r#"
        WITH inserted AS (
            INSERT INTO jobs (queue, kind, payload, dedupe_key, run_at, max_attempts)
            VALUES ($1, $2, $3, $4, COALESCE($5, now()), $6)
            ON CONFLICT (dedupe_key) DO NOTHING
            RETURNING id, queue
        )
        SELECT id AS "id!" FROM inserted, pg_notify($7, inserted.queue)
        "#,
        job.queue,
        job.kind,
        job.payload,
        job.dedupe_key,
        job.run_at,
        job.max_attempts,
        NOTIFY_CHANNEL,
    )
    .fetch_optional(exec)
    .await?;
    Ok(id)
}

/// Leases the next runnable job of `queue`: a ready job whose `run_at` has
/// passed, or a running job whose lease expired with attempts left.
/// `SKIP LOCKED` makes concurrent workers pick different jobs.
pub async fn lease(
    pool: &sqlx::PgPool,
    queue: &str,
    worker_id: &str,
    lease_for: Duration,
) -> Result<Option<LeasedJob>, DbError> {
    let job = sqlx::query_as!(
        LeasedJob,
        r#"
        UPDATE jobs SET
            state = 'running',
            attempts = attempts + 1,
            locked_by = $2,
            locked_until = now() + $3
        WHERE id = (
            SELECT id FROM jobs
            WHERE queue = $1
              AND attempts < max_attempts
              AND ((state = 'ready' AND run_at <= now())
                OR (state = 'running' AND locked_until < now()))
            ORDER BY run_at, id
            FOR UPDATE SKIP LOCKED
            LIMIT 1
        )
        RETURNING id, queue, kind, payload, attempts, max_attempts
        "#,
        queue,
        worker_id,
        interval(lease_for),
    )
    .fetch_optional(pool)
    .await?;
    Ok(job)
}

/// Extends the lease. Returns false if the worker lost it (it expired and
/// another worker took the job, or the job was finished elsewhere).
pub async fn heartbeat(
    pool: &sqlx::PgPool,
    id: i64,
    worker_id: &str,
    lease_for: Duration,
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "UPDATE jobs SET locked_until = now() + $3
         WHERE id = $1 AND state = 'running' AND locked_by = $2",
        id,
        worker_id,
        interval(lease_for),
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// Marks a job done. Returns false if the worker no longer held the lease.
pub async fn complete(pool: &sqlx::PgPool, id: i64, worker_id: &str) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "UPDATE jobs SET state = 'done', finished_at = now(), locked_by = NULL,
                locked_until = NULL, last_error = NULL
         WHERE id = $1 AND state = 'running' AND locked_by = $2",
        id,
        worker_id,
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// Records a failed attempt: retry with backoff, or `dead` if this was the
/// last attempt or `permanent` is set.
pub async fn fail(
    pool: &sqlx::PgPool,
    id: i64,
    worker_id: &str,
    error: &str,
    permanent: bool,
) -> Result<FailOutcome, DbError> {
    let error: String = error.chars().take(4096).collect();
    let row = sqlx::query!(
        r#"
        UPDATE jobs SET
            state = CASE WHEN $4 OR attempts >= max_attempts THEN 'dead' ELSE 'ready' END,
            run_at = CASE WHEN $4 OR attempts >= max_attempts THEN run_at
                          ELSE now() + make_interval(secs => LEAST($5 * power(2, attempts - 1), $6)) END,
            finished_at = CASE WHEN $4 OR attempts >= max_attempts THEN now() END,
            locked_by = NULL,
            locked_until = NULL,
            last_error = $3
        WHERE id = $1 AND state = 'running' AND locked_by = $2
        RETURNING state AS "state!", run_at AS "run_at!"
        "#,
        id,
        worker_id,
        error,
        permanent,
        BASE_BACKOFF.as_secs_f64(),
        MAX_BACKOFF.as_secs_f64(),
    )
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        None => FailOutcome::LeaseLost,
        Some(r) if r.state == "dead" => FailOutcome::Dead,
        Some(r) => FailOutcome::Retry { run_at: r.run_at },
    })
}

/// Moves jobs whose lease expired on their final attempt to `dead`; they'd
/// otherwise never be picked up again. Returns the number of jobs changed.
pub async fn reap_expired(pool: &sqlx::PgPool) -> Result<u64, DbError> {
    let r = sqlx::query!(
        "UPDATE jobs SET state = 'dead', finished_at = now(), locked_by = NULL,
                locked_until = NULL, last_error = 'lease expired on final attempt'
         WHERE state = 'running' AND locked_until < now() AND attempts >= max_attempts",
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

/// Jobs per queue and state, for the health page.
pub async fn stats(exec: impl PgExecutor<'_>) -> Result<Vec<(String, String, i64)>, DbError> {
    let rows = sqlx::query!(r#"SELECT queue, state, count(*) AS "n!" FROM jobs GROUP BY queue, state ORDER BY queue, state"#)
        .fetch_all(exec)
        .await?;
    Ok(rows.into_iter().map(|r| (r.queue, r.state, r.n)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(1), Duration::from_secs(5));
        assert_eq!(backoff(2), Duration::from_secs(10));
        assert_eq!(backoff(4), Duration::from_secs(40));
        assert_eq!(backoff(30), MAX_BACKOFF);
        assert_eq!(backoff(0), Duration::from_secs(5));
    }
}
