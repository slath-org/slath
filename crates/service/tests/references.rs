// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! References per viewer (labels, milestones, teams) and reference
//! autocomplete: nothing leaks to people who can't read the target, no
//! emails, and no account enumeration on open instances.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use slath_core::authz::{Subject, Visibility};
use slath_markup::refs::RefContext;
use slath_service::ServiceError;
use slath_service::repos::NewRepoInput;
use sqlx::PgPool;

async fn repo(
    svc: &slath_service::Services,
    owner: &Subject,
    ns: &str,
    name: &str,
    vis: Visibility,
) {
    svc.create_repo(
        owner,
        NewRepoInput {
            namespace: ns.into(),
            name: name.into(),
            description: String::new(),
            visibility: vis,
            default_branch: "main".into(),
        },
    )
    .await
    .unwrap();
}

fn ctx(ns: &str, repo: &str) -> RefContext {
    RefContext {
        ns: ns.into(),
        repo: repo.into(),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn conf_editor_02_rendering_leaks_nothing(pool: PgPool) {
    let env = common::services(pool, "");
    let svc = &env.svc;
    let (_, alice) = common::user(svc, "alice").await;
    let (_, bob) = common::user(svc, "bob").await;
    repo(svc, &alice, "alice", "secret", Visibility::Private).await;
    svc.save_label(
        &alice,
        "alice",
        "secret",
        None,
        "needs review",
        "ff0000",
        "Waiting for a look",
    )
    .await
    .unwrap();
    svc.save_milestone(&alice, "alice", "secret", None, "Q3 cleanup", "", "", true)
        .await
        .unwrap();
    let org = svc.create_org(&alice, "acme", "").await.unwrap();
    svc.create_team(&alice, org, "core", "Core team")
        .await
        .unwrap();

    let src = "~\"needs review\" %\"Q3 cleanup\" ~missing @acme/core alice/secret#1";
    let c = ctx("alice", "secret");
    let mine = svc.render_markdown(&alice, Some(&c), src).await.unwrap();
    assert!(
        mine.contains("href=\"/alice/secret/issues?labels=needs%20review\"")
            && mine.contains("title=\"Waiting for a look\""),
        "{mine}"
    );
    assert!(
        mine.contains("/alice/secret/issues?milestone=Q3%20cleanup"),
        "{mine}"
    );
    assert!(mine.contains("href=\"/acme/-/teams/core\""), "{mine}");
    assert!(!mine.contains("~missing</a>"), "unknown labels stay text");

    for viewer in [&bob, &Subject::Anonymous] {
        let theirs = svc.render_markdown(viewer, Some(&c), src).await.unwrap();
        assert!(!theirs.contains("<a"), "{theirs}");
        assert!(!theirs.contains("Waiting for a look"), "{theirs}");
        assert!(!theirs.contains("Core team"), "{theirs}");
        assert!(
            theirs.contains("~&quot;needs review&quot;") || theirs.contains("~\"needs review\"")
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn conf_editor_02_autocomplete_follows_read_access(pool: PgPool) {
    let env = common::services(pool, "");
    let svc = &env.svc;
    let (_, alice) = common::user(svc, "alice").await;
    let (_, bob) = common::user(svc, "bob").await;
    common::user(svc, "carol").await;
    repo(svc, &alice, "alice", "secret", Visibility::Private).await;
    svc.create_issue(&alice, "alice", "secret", "Hidden plans", "")
        .await
        .unwrap();
    svc.save_label(&alice, "alice", "secret", None, "bug", "ff0000", "")
        .await
        .unwrap();

    for kind in ["user", "item", "label", "milestone", "team"] {
        let err = svc
            .reference_suggestions(&bob, "alice", "secret", kind, "", None)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound), "{kind:?}: {err:?}");
        let err = svc
            .reference_suggestions(&Subject::Anonymous, "alice", "secret", kind, "", None)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::Unauthenticated), "{err:?}");
    }
    let items = svc
        .reference_suggestions(&alice, "alice", "secret", "item", "1", None)
        .await
        .unwrap();
    assert_eq!(items[0].value, "#1");
    assert_eq!(items[0].label, "Hidden plans");
    let items = svc
        .reference_suggestions(&alice, "alice", "secret", "item", "plan", None)
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    let labels = svc
        .reference_suggestions(&alice, "alice", "secret", "label", "~b", None)
        .await
        .unwrap();
    assert_eq!(labels[0].value, "~bug");
    assert_eq!(labels[0].color.as_deref(), Some("ff0000"));
    // People: no email anywhere, ranked by access.
    let users = svc
        .reference_suggestions(&alice, "alice", "secret", "user", "", None)
        .await
        .unwrap();
    let names: Vec<&str> = users.iter().map(|u| u.label.as_str()).collect();
    assert_eq!(names, ["alice", "bob", "carol"]);
    assert!(
        users
            .iter()
            .all(|u| !format!("{u:?}").contains("example.org"))
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn conf_editor_02_open_registration_limits_enumeration(pool: PgPool) {
    let env = common::services(pool, "[auth]\nregistration = \"open\"\n");
    let svc = &env.svc;
    let (_, alice) = common::user(svc, "alice").await;
    let (_, bob) = common::user(svc, "bob").await;
    let (_, carol) = common::user(svc, "carol").await;
    common::user(svc, "dave").await;
    repo(svc, &alice, "alice", "open", Visibility::Public).await;
    svc.create_issue(&alice, "alice", "open", "One", "")
        .await
        .unwrap();

    // bob has no grant: only people who took part in the repository.
    let names = |v: Vec<slath_service::references::Suggestion>| {
        v.into_iter().map(|s| s.label).collect::<Vec<_>>()
    };
    let got = svc
        .reference_suggestions(&bob, "alice", "open", "user", "", None)
        .await
        .unwrap();
    assert_eq!(names(got), ["alice"]);
    svc.add_issue_comment(&carol, "alice", "open", 1, "me too")
        .await
        .unwrap();
    let got = svc
        .reference_suggestions(&bob, "alice", "open", "user", "", Some(1))
        .await
        .unwrap();
    // carol takes part in issue 1, so she ranks first.
    assert_eq!(names(got), ["alice", "carol"]);
    let got = svc
        .reference_suggestions(&bob, "alice", "open", "user", "d", None)
        .await
        .unwrap();
    assert!(got.is_empty(), "dave isn't revealed");
    // The owner sees everyone.
    let got = svc
        .reference_suggestions(&alice, "alice", "open", "user", "d", None)
        .await
        .unwrap();
    assert_eq!(names(got), ["dave"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn suggestions_are_rate_limited(pool: PgPool) {
    let env = common::services(pool, "");
    let svc = &env.svc;
    let (_, alice) = common::user(svc, "alice").await;
    repo(svc, &alice, "alice", "r", Visibility::Public).await;
    for _ in 0..60 {
        svc.reference_suggestions(&alice, "alice", "r", "label", "", None)
            .await
            .unwrap();
    }
    let err = svc
        .reference_suggestions(&alice, "alice", "r", "label", "", None)
        .await
        .unwrap_err();
    assert!(matches!(err, ServiceError::RateLimited { .. }), "{err:?}");
}
