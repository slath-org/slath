// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Issue numbering under concurrency (ISSUE-01).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use slath_core::authz::Visibility;
use slath_service::repos::NewRepoInput;
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn conf_issue_01_concurrent_creation_never_duplicates_a_number(pool: PgPool) {
    let env = common::services(pool.clone(), "[database]\nmax_connections = 60\n");
    let (_, alice) = common::user(&env.svc, "alice").await;
    env.svc
        .create_repo(
            &alice,
            NewRepoInput {
                namespace: "alice".into(),
                name: "demo".into(),
                description: String::new(),
                visibility: Visibility::Private,
                default_branch: "main".into(),
            },
        )
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for i in 0..50 {
        let (svc, who) = (env.svc.clone(), alice.clone());
        tasks.push(tokio::spawn(async move {
            svc.create_issue(&who, "alice", "demo", &format!("Issue {i}"), "")
                .await
                .unwrap()
                .number
        }));
    }
    let mut numbers = Vec::new();
    for t in tasks {
        numbers.push(t.await.unwrap());
    }
    numbers.sort_unstable();
    assert_eq!(numbers, (1..=50).collect::<Vec<i32>>());
    let next: i32 = sqlx::query_scalar("SELECT next_item_number FROM repositories")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(next, 51);
}
