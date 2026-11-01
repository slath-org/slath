// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The audit log is append-only; only the retention purge deletes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sqlx::PgPool;

async fn insert(pool: &PgPool, age_days: i32) {
    sqlx::query(
        "INSERT INTO audit_events (id, action, target_kind, created_at)
         VALUES ($1, 'test', 'none', now() - make_interval(days => $2))",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(age_days)
    .execute(pool)
    .await
    .unwrap();
}

async fn count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_events")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn events_cannot_be_changed_or_deleted(pool: PgPool) {
    insert(&pool, 0).await;
    let update = sqlx::query("UPDATE audit_events SET action = 'forged'")
        .execute(&pool)
        .await;
    assert!(update.is_err());
    let delete = sqlx::query("DELETE FROM audit_events").execute(&pool).await;
    assert!(delete.is_err());
    assert_eq!(count(&pool).await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn purge_deletes_only_events_past_retention(pool: PgPool) {
    insert(&pool, 400).await;
    insert(&pool, 31).await;
    insert(&pool, 29).await;
    insert(&pool, 0).await;
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(slath_db::audit::purge(&mut tx, 30).await.unwrap(), 2);
    tx.commit().await.unwrap();
    assert_eq!(count(&pool).await, 2);
    // The purge permission ended with its transaction.
    assert!(
        sqlx::query("DELETE FROM audit_events")
            .execute(&pool)
            .await
            .is_err()
    );
}
