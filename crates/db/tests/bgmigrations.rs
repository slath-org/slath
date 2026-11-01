// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Background migrations (OPS-07): interrupted batches leave no trace and
//! resume from the persisted cursor, so every item is processed exactly once.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use slath_db::bgmigrations::{
    self as bg, BackgroundMigration, Batch, BatchFuture, Control, EstimateFuture, State, Step,
};
use sqlx::{PgConnection, PgPool};

/// Increments `bg_items.n` for each item. Can be told to fail or hang after
/// doing its work in a given batch (counting down `trap`).
struct Increment {
    /// Batches until the trap fires; negative: never.
    trap: AtomicI64,
    /// true: hang forever (the test aborts the task); false: return an error.
    hang: bool,
}

impl BackgroundMigration for Increment {
    fn name(&self) -> &'static str {
        "test_increment"
    }
    fn introduced_in(&self) -> &'static str {
        "0.2.0"
    }
    fn estimate<'a>(&'a self, pool: &'a PgPool) -> EstimateFuture<'a> {
        Box::pin(async move {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM bg_items")
                .fetch_one(pool)
                .await
                .map(Some)
                .map_err(|e| e.to_string())
        })
    }
    fn run_batch<'a>(
        &'a self,
        tx: &'a mut PgConnection,
        cursor: Option<&'a Value>,
        limit: i64,
    ) -> BatchFuture<'a> {
        Box::pin(async move {
            let after = cursor.and_then(Value::as_i64).unwrap_or(0);
            let ids: Vec<i32> = sqlx::query_scalar(
                "UPDATE bg_items SET n = n + 1 WHERE id IN
                   (SELECT id FROM bg_items WHERE id > $1 ORDER BY id LIMIT $2)
                 RETURNING id",
            )
            .bind(after)
            .bind(limit)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
            if self.trap.fetch_sub(1, Ordering::SeqCst) == 0 {
                if self.hang {
                    std::future::pending::<()>().await;
                }
                return Err("injected failure".to_owned());
            }
            let last = ids.iter().max().copied();
            Ok(Batch {
                cursor: match last {
                    Some(l) if ids.len() as i64 == limit => Some(json!(l)),
                    _ => None,
                },
                processed: ids.len() as i64,
            })
        })
    }
}

async fn setup(pool: &PgPool, items: i32) {
    sqlx::raw_sql("CREATE TABLE bg_items (id int PRIMARY KEY, n int NOT NULL DEFAULT 0)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO bg_items (id) SELECT generate_series(1, $1)")
        .bind(items)
        .execute(pool)
        .await
        .unwrap();
}

async fn counts(pool: &PgPool) -> (i64, i64, i64) {
    sqlx::query_as("SELECT count(*), min(n)::bigint, max(n)::bigint FROM bg_items")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A tiny deterministic generator, so failures reproduce from the seed.
fn rng(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *seed >> 33
}

#[sqlx::test(migrations = "../../migrations")]
async fn conf_ops_07_interrupted_batches_resume_without_duplicate_effects(pool: PgPool) {
    let mut seed = 0x5eed_u64;
    for case in 0..12 {
        sqlx::raw_sql("DROP TABLE IF EXISTS bg_items; DELETE FROM background_migrations")
            .execute(&pool)
            .await
            .unwrap();
        let items = 1 + (rng(&mut seed) % 200) as i32;
        let limit = 1 + (rng(&mut seed) % 40) as i64;
        setup(&pool, items).await;
        let m = Arc::new(Increment {
            trap: AtomicI64::new(-1),
            hang: false,
        });
        assert!(bg::register(&pool, m.as_ref()).await.unwrap());
        assert!(
            !bg::register(&pool, m.as_ref()).await.unwrap(),
            "idempotent"
        );
        let mut interruptions = 0;
        let mut idle = 0;
        loop {
            // Interrupt roughly every third batch, alternating error and kill.
            let interrupt = rng(&mut seed).is_multiple_of(3);
            let hang = rng(&mut seed).is_multiple_of(2);
            let m2 = Arc::new(Increment {
                trap: AtomicI64::new(if interrupt { 0 } else { -1 }),
                hang,
            });
            if interrupt && hang {
                let p = pool.clone();
                let t = tokio::spawn(async move { bg::step(&p, m2.as_ref(), limit).await });
                tokio::time::sleep(Duration::from_millis(20)).await;
                t.abort();
                let _ = t.await;
                interruptions += 1;
                continue;
            }
            match bg::step(&pool, m2.as_ref(), limit).await.unwrap() {
                Step::Progress { done: true, .. } => break,
                Step::Progress { .. } => {}
                Step::Failed(e) => {
                    assert_eq!(e, "injected failure");
                    interruptions += 1;
                    let s = bg::get(&pool, "test_increment").await.unwrap().unwrap();
                    assert_eq!(s.state, State::Failed);
                    // A failed migration doesn't run until retried.
                    assert_eq!(
                        bg::step(&pool, m2.as_ref(), limit).await.unwrap(),
                        Step::Idle
                    );
                    assert!(
                        bg::control(&pool, "test_increment", Control::Retry)
                            .await
                            .unwrap()
                    );
                }
                // The killed batch's connection may still hold the row lock
                // until its rollback completes.
                Step::Idle => {
                    idle += 1;
                    assert!(idle < 200, "case {case}: stuck");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
        assert_eq!(
            counts(&pool).await,
            (i64::from(items), 1, 1),
            "case {case}: items={items} limit={limit} interruptions={interruptions}"
        );
        let s = bg::get(&pool, "test_increment").await.unwrap().unwrap();
        assert_eq!(s.state, State::Done);
        assert_eq!(s.processed, i64::from(items));
        assert_eq!(s.percent(), Some(100));
        assert!(bg::is_done(&pool, "test_increment").await.unwrap());
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_runners_and_pause(pool: PgPool) {
    setup(&pool, 500).await;
    let m = Arc::new(Increment {
        trap: AtomicI64::new(-1),
        hang: false,
    });
    bg::register(&pool, m.as_ref()).await.unwrap();
    // Paused: nothing runs.
    assert!(
        bg::control(&pool, "test_increment", Control::Pause)
            .await
            .unwrap()
    );
    assert_eq!(bg::step(&pool, m.as_ref(), 10).await.unwrap(), Step::Idle);
    assert!(
        !bg::control(&pool, "test_increment", Control::Retry)
            .await
            .unwrap()
    );
    assert!(
        bg::control(&pool, "test_increment", Control::Resume)
            .await
            .unwrap()
    );
    assert!(!bg::is_done(&pool, "test_increment").await.unwrap());
    // Four runners at once: each batch runs exactly once.
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let (p, m) = (pool.clone(), Arc::clone(&m));
        tasks.push(tokio::spawn(async move {
            loop {
                match bg::step(&p, m.as_ref(), 7).await.unwrap() {
                    Step::Progress { done: true, .. } => return,
                    Step::Idle => {
                        if bg::is_done(&p, "test_increment").await.unwrap() {
                            return;
                        }
                        tokio::task::yield_now().await;
                    }
                    _ => {}
                }
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(counts(&pool).await, (500, 1, 1));
}
