// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Job queue tests against a real PostgreSQL (`DATABASE_URL`, see docs/dev/setup.md).
//! Each test gets a fresh database with all migrations applied.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use slath_db::jobs::{self, FailOutcome, NewJob};
use slath_db::worker::{JobError, WorkerPool, WorkerPoolConfig};
use sqlx::PgPool;

const LEASE: Duration = Duration::from_secs(30);

async fn state(pool: &PgPool, id: i64) -> (String, i32) {
    let r = sqlx::query!("SELECT state, attempts FROM jobs WHERE id = $1", id)
        .fetch_one(pool)
        .await
        .unwrap();
    (r.state, r.attempts)
}

/// Makes a retried job runnable now instead of after its backoff.
async fn make_due(pool: &PgPool, id: i64) {
    sqlx::query!("UPDATE jobs SET run_at = now() WHERE id = $1", id)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn duplicate_dedupe_key_is_a_noop(pool: PgPool) {
    let job = NewJob::new("default", "ref.updated", json!({"push": 1})).dedupe_key("push:1");
    let first = jobs::enqueue(&pool, &job).await.unwrap();
    let second = jobs::enqueue(&pool, &job).await.unwrap();
    assert!(first.is_some());
    assert_eq!(second, None);
    let n = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM jobs"#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    // Jobs without a key never collide.
    let plain = NewJob::new("default", "x", json!({}));
    assert!(jobs::enqueue(&pool, &plain).await.unwrap().is_some());
    assert!(jobs::enqueue(&pool, &plain).await.unwrap().is_some());
}

#[sqlx::test(migrations = "../../migrations")]
async fn enqueue_joins_the_callers_transaction(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();
    jobs::enqueue(&mut *tx, &NewJob::new("default", "x", json!({})))
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert!(
        jobs::lease(&pool, "default", "w", LEASE)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_leases_never_return_the_same_job(pool: PgPool) {
    for i in 0..50 {
        jobs::enqueue(&pool, &NewJob::new("default", "x", json!({ "i": i })))
            .await
            .unwrap();
    }
    let mut handles = Vec::new();
    for w in 0..10 {
        let pool = pool.clone();
        handles.push(tokio::spawn(async move {
            let mut got = Vec::new();
            while let Some(job) = jobs::lease(&pool, "default", &format!("w{w}"), LEASE)
                .await
                .unwrap()
            {
                got.push(job.id);
            }
            got
        }));
    }
    let mut all = Vec::new();
    for h in handles {
        all.extend(h.await.unwrap());
    }
    let len = all.len();
    all.sort_unstable();
    all.dedup();
    assert_eq!(len, 50);
    assert_eq!(all.len(), 50, "a job was leased twice");
}

#[sqlx::test(migrations = "../../migrations")]
async fn worker_pools_run_each_job_exactly_once(pool: PgPool) {
    const N: i64 = 200;
    for i in 0..N {
        jobs::enqueue(&pool, &NewJob::new("default", "count", json!({ "i": i })))
            .await
            .unwrap();
    }
    let runs: Arc<Mutex<HashMap<i64, u32>>> = Arc::default();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let mut pools = Vec::new();
    for p in 0..2 {
        let mut cfg = WorkerPoolConfig::new("default");
        cfg.worker_id_prefix = format!("pool{p}");
        cfg.concurrency = 6;
        cfg.poll_interval = Duration::from_millis(50);
        let runs = Arc::clone(&runs);
        let wp = WorkerPool::new(pool.clone(), cfg).handle("count", move |job: jobs::LeasedJob| {
            let runs = Arc::clone(&runs);
            async move {
                *runs.lock().unwrap().entry(job.id).or_default() += 1;
                tokio::task::yield_now().await;
                Ok(())
            }
        });
        pools.push(tokio::spawn(wp.run(stop_rx.clone())));
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let done = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM jobs WHERE state = 'done'"#)
            .fetch_one(&pool)
            .await
            .unwrap();
        if done == N {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "only {done}/{N} jobs done"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop_tx.send(true).unwrap();
    for p in pools {
        p.await.unwrap();
    }
    let runs = runs.lock().unwrap();
    assert_eq!(runs.len() as i64, N);
    assert!(runs.values().all(|&n| n == 1), "a job ran more than once");
}

#[sqlx::test(migrations = "../../migrations")]
async fn expired_lease_is_released_to_another_worker(pool: PgPool) {
    let id = jobs::enqueue(&pool, &NewJob::new("default", "x", json!({})))
        .await
        .unwrap()
        .unwrap();
    let first = jobs::lease(&pool, "default", "w1", Duration::from_millis(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.id, id);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = jobs::lease(&pool, "default", "w2", LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.id, id);
    assert_eq!(second.attempts, 2);
    // The first worker lost the lease: its results are ignored.
    assert!(!jobs::heartbeat(&pool, id, "w1", LEASE).await.unwrap());
    assert!(!jobs::complete(&pool, id, "w1").await.unwrap());
    assert_eq!(
        jobs::fail(&pool, id, "w1", "late", false).await.unwrap(),
        FailOutcome::LeaseLost
    );
    assert!(jobs::heartbeat(&pool, id, "w2", LEASE).await.unwrap());
    assert!(jobs::complete(&pool, id, "w2").await.unwrap());
    assert_eq!(state(&pool, id).await.0, "done");
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_lease_is_not_released(pool: PgPool) {
    jobs::enqueue(&pool, &NewJob::new("default", "x", json!({})))
        .await
        .unwrap();
    assert!(
        jobs::lease(&pool, "default", "w1", LEASE)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        jobs::lease(&pool, "default", "w2", LEASE)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn failures_retry_with_backoff_then_die(pool: PgPool) {
    let job = NewJob::new("default", "x", json!({})).max_attempts(3);
    let id = jobs::enqueue(&pool, &job).await.unwrap().unwrap();
    for attempt in 1..=3 {
        let leased = jobs::lease(&pool, "default", "w", LEASE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(leased.attempts, attempt);
        let outcome = jobs::fail(&pool, id, "w", "boom", false).await.unwrap();
        if attempt < 3 {
            let FailOutcome::Retry { run_at } = outcome else {
                panic!("{outcome:?}")
            };
            let delay = run_at - time::OffsetDateTime::now_utc();
            let expected = jobs::backoff(attempt).as_secs_f64();
            assert!(
                (delay.as_seconds_f64() - expected).abs() < 2.0,
                "delay {delay} vs {expected}"
            );
            // Not runnable before the backoff elapses.
            assert!(
                jobs::lease(&pool, "default", "w", LEASE)
                    .await
                    .unwrap()
                    .is_none()
            );
            make_due(&pool, id).await;
        } else {
            assert_eq!(outcome, FailOutcome::Dead);
        }
    }
    assert_eq!(state(&pool, id).await, ("dead".into(), 3));
    let err = sqlx::query_scalar!("SELECT last_error FROM jobs WHERE id = $1", id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(err.as_deref(), Some("boom"));
    make_due(&pool, id).await;
    assert!(
        jobs::lease(&pool, "default", "w", LEASE)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn permanent_failure_dies_immediately(pool: PgPool) {
    let id = jobs::enqueue(&pool, &NewJob::new("default", "x", json!({})))
        .await
        .unwrap()
        .unwrap();
    jobs::lease(&pool, "default", "w", LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        jobs::fail(&pool, id, "w", "bad payload", true)
            .await
            .unwrap(),
        FailOutcome::Dead
    );
    assert_eq!(state(&pool, id).await, ("dead".into(), 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn expired_lease_on_final_attempt_is_reaped_as_dead(pool: PgPool) {
    let job = NewJob::new("default", "x", json!({})).max_attempts(1);
    let id = jobs::enqueue(&pool, &job).await.unwrap().unwrap();
    jobs::lease(&pool, "default", "w1", Duration::from_millis(1))
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        jobs::lease(&pool, "default", "w2", LEASE)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(jobs::reap_expired(&pool).await.unwrap(), 1);
    assert_eq!(state(&pool, id).await.0, "dead");
}

#[sqlx::test(migrations = "../../migrations")]
async fn queues_and_run_at_are_respected(pool: PgPool) {
    let later = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    jobs::enqueue(&pool, &NewJob::new("other", "x", json!({})))
        .await
        .unwrap();
    jobs::enqueue(&pool, &NewJob::new("default", "x", json!({})).run_at(later))
        .await
        .unwrap();
    assert!(
        jobs::lease(&pool, "default", "w", LEASE)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        jobs::lease(&pool, "other", "w", LEASE)
            .await
            .unwrap()
            .is_some()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn notify_wakes_idle_workers(pool: PgPool) {
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut cfg = WorkerPoolConfig::new("default");
    cfg.concurrency = 1;
    cfg.poll_interval = Duration::from_secs(3600); // only NOTIFY can wake it in time
    let wp = WorkerPool::new(pool.clone(), cfg).handle("ping", move |job: jobs::LeasedJob| {
        let done_tx = done_tx.clone();
        async move {
            done_tx.send(job.id).unwrap();
            Ok(())
        }
    });
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(wp.run(stop_rx));
    // Let the worker go idle and the listener subscribe.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let id = jobs::enqueue(&pool, &NewJob::new("default", "ping", json!({})))
        .await
        .unwrap()
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(10), done_rx.recv())
        .await
        .unwrap();
    assert_eq!(got, Some(id));
    stop_tx.send(true).unwrap();
    handle.await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_kind_and_handler_errors_are_recorded(pool: PgPool) {
    let unknown = jobs::enqueue(&pool, &NewJob::new("default", "nope", json!({})))
        .await
        .unwrap()
        .unwrap();
    let retry = jobs::enqueue(&pool, &NewJob::new("default", "flaky", json!({})))
        .await
        .unwrap()
        .unwrap();
    let mut cfg = WorkerPoolConfig::new("default");
    cfg.poll_interval = Duration::from_millis(50);
    let wp = WorkerPool::new(pool.clone(), cfg).handle("flaky", |_job: jobs::LeasedJob| async {
        Err(JobError::Retry("try later".into()))
    });
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(wp.run(stop_rx));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while state(&pool, unknown).await.0 != "dead"
        || state(&pool, retry).await != ("ready".into(), 1)
    {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop_tx.send(true).unwrap();
    handle.await.unwrap();
}
