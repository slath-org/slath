// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Features that depend on a background migration degrade visibly until it
//! has finished (OPS-07).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use serde_json::Value;
use slath_db::bgmigrations::{BackgroundMigration, Batch, BatchFuture, EstimateFuture};
use slath_service::{ServiceError, Services, requires_bgmigration};
use sqlx::{PgConnection, PgPool};

/// Processes ten items, one batch of `limit` at a time, without effects.
struct Counter;

impl BackgroundMigration for Counter {
    fn name(&self) -> &'static str {
        "test_counter"
    }
    fn introduced_in(&self) -> &'static str {
        "0.2.0"
    }
    fn estimate<'a>(&'a self, _: &'a PgPool) -> EstimateFuture<'a> {
        Box::pin(async { Ok(Some(10)) })
    }
    fn run_batch<'a>(
        &'a self,
        _: &'a mut PgConnection,
        cursor: Option<&'a Value>,
        limit: i64,
    ) -> BatchFuture<'a> {
        Box::pin(async move {
            let done = cursor.and_then(Value::as_i64).unwrap_or(0);
            let next = (done + limit).min(10);
            Ok(Batch {
                cursor: (next < 10).then(|| Value::from(next)),
                processed: next - done,
            })
        })
    }
}

/// A feature that needs the backfill.
async fn feature(svc: &Services) -> Result<&'static str, ServiceError> {
    requires_bgmigration!(svc, "test_counter");
    Ok("complete data")
}

#[sqlx::test(migrations = "../../migrations")]
async fn conf_ops_07_dependent_feature_shows_computing_until_done(pool: PgPool) {
    let env = common::services(
        pool,
        "[limits]\nbackground_migration_batch = 3\nbackground_migration_interval = \"0ms\"\n",
    );
    let svc = env.svc.with_background_migration(Arc::new(Counter));
    // Before registration and while pending: "computing".
    assert!(matches!(
        feature(&svc).await,
        Err(ServiceError::Computing("test_counter"))
    ));
    assert_eq!(
        svc.register_background_migrations().await.unwrap(),
        vec!["repo_config_v1", "test_counter"]
    );
    assert!(
        svc.register_background_migrations()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        feature(&svc).await,
        Err(ServiceError::Computing(_))
    ));
    svc.run_background_migrations(&|| false).await.unwrap();
    assert_eq!(feature(&svc).await.unwrap(), "complete data");
    let status = svc
        .background_migration_status(&slath_core::authz::Subject::System)
        .await
        .unwrap();
    let counter = status.iter().find(|s| s.name == "test_counter").unwrap();
    assert_eq!(counter.processed, 10);
    assert_eq!(counter.percent(), Some(100));
    // The repository config refresh (no repositories here) finished too.
    assert!(
        svc.background_migration_done("repo_config_v1")
            .await
            .unwrap()
    );
}

/// A restarted server ends the maintenance of a finished upgrade to its
/// version (or older), but not of one still running or for a newer version.
#[sqlx::test(migrations = "../../migrations")]
async fn conf_ops_07_server_start_ends_finished_upgrade_maintenance(pool: PgPool) {
    let env = common::services(pool, "");
    let svc = &env.svc;
    let me = env!("CARGO_PKG_VERSION");
    for (until, finished, cleared) in [
        (me, false, false),
        ("99.0.0", true, false),
        (me, true, true),
        ("0.0.1", true, true),
    ] {
        svc.enter_maintenance("upgrade", Some(until), finished)
            .await
            .unwrap();
        svc.on_server_start().await.unwrap();
        assert_eq!(
            svc.maintenance().is_on(),
            !cleared,
            "until={until} finished={finished}"
        );
        svc.leave_maintenance().await.unwrap();
    }
}
