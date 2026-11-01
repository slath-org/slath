// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search (RBAC-01, ISSUE-02): private matches never appear for people who
//! can't read them, and pages stay full because filtering happens in SQL.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use slath_core::authz::{Action, Role, Subject, Visibility};
use slath_service::repos::NewRepoInput;
use sqlx::PgPool;

async fn repo(svc: &slath_service::Services, owner: &Subject, name: &str, vis: Visibility) {
    svc.create_repo(
        owner,
        NewRepoInput {
            namespace: "alice".into(),
            name: name.into(),
            description: format!("The {name} project"),
            visibility: vis,
            default_branch: "main".into(),
        },
    )
    .await
    .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn conf_rbac_01_search_filters_in_sql(pool: PgPool) {
    let env = common::services(pool, "");
    let svc = &env.svc;
    let (_, alice) = common::user(svc, "alice").await;
    let (_, bob) = common::user(svc, "bob").await;
    let (_, carol) = common::user(svc, "carol").await;
    repo(svc, &alice, "open", Visibility::Public).await;
    repo(svc, &alice, "hidden", Visibility::Private).await;
    // Private matches outnumber and outrank public ones.
    for i in 0..30 {
        svc.create_issue(
            &alice,
            "alice",
            "hidden",
            &format!("zebra zebra zebra {i}"),
            "zebra",
        )
        .await
        .unwrap();
    }
    for i in 0..15 {
        svc.create_issue(
            &alice,
            "alice",
            "open",
            &format!("plain {i}"),
            "a zebra here",
        )
        .await
        .unwrap();
    }
    let a = svc
        .repo_access(&alice, "alice", "hidden", Action::RepoRead)
        .await
        .unwrap();
    svc.set_collaborator(&alice, &a, "carol", Role::Read)
        .await
        .unwrap();

    for who in [&bob, &Subject::Anonymous] {
        let page = svc.search_issues(who, "zebra", None, 0, 10).await.unwrap();
        assert_eq!(page.len(), 10, "pages stay full");
        assert!(page.iter().all(|h| h.repo == "alice/open"));
        let next = svc.search_issues(who, "zebra", None, 10, 10).await.unwrap();
        assert_eq!(next.len(), 5);
        assert!(next.iter().all(|h| h.repo == "alice/open"));
        assert!(
            svc.search_issues(who, "zebra", Some(("alice", "hidden")), 0, 10)
                .await
                .is_err(),
            "scoping to an invisible repository is 'not found'"
        );
        let repos = svc.search_repos(who, "project", 0, 10).await.unwrap();
        assert_eq!(
            repos.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["open"]
        );
    }
    // A reader of the private repository finds it, ranked first.
    let page = svc
        .search_issues(&carol, "zebra", None, 0, 10)
        .await
        .unwrap();
    assert_eq!(page.len(), 10);
    assert!(page.iter().all(|h| h.repo == "alice/hidden"));
    assert_eq!(
        svc.search_repos(&carol, "hid", 0, 10).await.unwrap().len(),
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn comments_match_but_hidden_ones_dont(pool: PgPool) {
    let env = common::services(pool, "");
    let svc = &env.svc;
    let (_, alice) = common::user(svc, "alice").await;
    repo(svc, &alice, "open", Visibility::Public).await;
    svc.create_issue(&alice, "alice", "open", "First", "")
        .await
        .unwrap();
    svc.create_issue(&alice, "alice", "open", "Second", "")
        .await
        .unwrap();
    svc.add_issue_comment(&alice, "alice", "open", 1, "the quokka appears")
        .await
        .unwrap();
    let c = svc
        .add_issue_comment(&alice, "alice", "open", 2, "another quokka")
        .await
        .unwrap();
    svc.delete_issue_comment(&alice, "alice", "open", c.id)
        .await
        .unwrap();
    let hits = svc
        .search_issues(&Subject::Anonymous, "quokka", None, 0, 10)
        .await
        .unwrap();
    assert_eq!(hits.iter().map(|h| h.number).collect::<Vec<_>>(), [1]);
    // Phrases and exclusions (websearch syntax).
    assert_eq!(
        svc.search_issues(&Subject::Anonymous, "\"quokka appears\"", None, 0, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        svc.search_issues(&Subject::Anonymous, "quokka -appears", None, 0, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        svc.search_issues(&Subject::Anonymous, "   ", None, 0, 10)
            .await
            .unwrap()
            .is_empty()
    );
}
