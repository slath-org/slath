// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Migration and schema tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use slath_db::{DbError, MIGRATOR, REQUIRED_SCHEMA};
use sqlx::PgPool;

#[test]
fn required_schema_is_the_newest_migration() {
    let newest = MIGRATOR.iter().map(|m| m.version).max().unwrap();
    assert_eq!(newest, i64::from(REQUIRED_SCHEMA));
}

#[test]
fn every_migration_sets_schema_info() {
    for m in MIGRATOR.iter() {
        let sql = m.sql.as_ref();
        let sets = sql.contains(&format!(
            "INSERT INTO schema_info (version) VALUES ({})",
            m.version
        )) || sql.contains(&format!("UPDATE schema_info SET version = {}", m.version));
        assert!(
            sets,
            "migration {} must set schema_info.version to {}",
            m.version, m.version
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn schema_check_passes_after_migrations(pool: PgPool) {
    slath_db::check_schema(&pool).await.unwrap();
    assert_eq!(
        slath_db::schema_version(&pool).await.unwrap(),
        Some(REQUIRED_SCHEMA)
    );
}

#[sqlx::test(migrations = false)]
async fn schema_check_fails_on_empty_and_newer_databases(pool: PgPool) {
    assert_eq!(slath_db::schema_version(&pool).await.unwrap(), None);
    assert!(matches!(
        slath_db::check_schema(&pool).await,
        Err(DbError::SchemaBehind { found: 0, .. })
    ));
    slath_db::migrate(&pool).await.unwrap();
    sqlx::query("UPDATE schema_info SET version = version + 1")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        slath_db::check_schema(&pool).await,
        Err(DbError::SchemaAhead { .. })
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn reserved_names_match_core(pool: PgPool) {
    let mut db: Vec<String> =
        sqlx::query_scalar!(r#"SELECT name::text AS "name!" FROM reserved_names"#)
            .fetch_all(&pool)
            .await
            .unwrap();
    db.sort();
    let core: Vec<String> = slath_core::names::RESERVED_NAMES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(db, core);
}

#[sqlx::test(migrations = "../../migrations")]
async fn namespace_names_are_case_insensitively_unique(pool: PgPool) {
    let insert = "INSERT INTO namespaces (id, name, kind) VALUES (gen_random_uuid(), $1, 'user')";
    sqlx::query(insert)
        .bind("Alice")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query(insert)
            .bind("alice")
            .execute(&pool)
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn audit_events_are_append_only(pool: PgPool) {
    sqlx::query(
        "INSERT INTO audit_events (id, action, target_kind) VALUES (gen_random_uuid(), 'test', 'user')",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        sqlx::query("UPDATE audit_events SET action = 'x'")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM audit_events")
            .execute(&pool)
            .await
            .is_err()
    );
    // Retention purges opt in explicitly.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL slath.audit_purge = 'on'")
        .execute(&mut *tx)
        .await
        .unwrap();
    let r = sqlx::query("DELETE FROM audit_events")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(r.rows_affected(), 1);
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn bots_need_owners_and_have_no_password(pool: PgPool) {
    let ns = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO namespaces (id, name, kind) VALUES (gen_random_uuid(), 'bot1', 'user') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let r = sqlx::query(
        "INSERT INTO users (id, namespace_id, kind) VALUES (gen_random_uuid(), $1, 'bot')",
    )
    .bind(ns)
    .execute(&pool)
    .await;
    assert!(r.is_err(), "bot without owner must be rejected");
}
