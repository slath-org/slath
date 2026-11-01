// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Labels, milestones, assignees, and list filters.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use slath_core::authz::{Role, Visibility};
use slath_service::ServiceError;
use slath_service::labels::FilterInput;
use slath_service::repos::NewRepoInput;
use sqlx::PgPool;

async fn setup(pool: PgPool) -> (common::Env, slath_core::authz::Subject) {
    let env = common::services(pool, "");
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
    (env, alice)
}

fn numbers(items: &[slath_db::issues::Issue]) -> Vec<i32> {
    let mut n: Vec<i32> = items.iter().map(|i| i.number).collect();
    n.sort_unstable();
    n
}

#[sqlx::test(migrations = "../../migrations")]
async fn assigning_a_user_without_read_access_is_rejected(pool: PgPool) {
    let (env, alice) = setup(pool).await;
    let svc = &env.svc;
    let (_, bob) = common::user(svc, "bob").await;
    common::user(svc, "carol").await;
    svc.create_issue(&alice, "alice", "demo", "One", "")
        .await
        .unwrap();
    let a = svc
        .repo_access(&alice, "alice", "demo", slath_core::authz::Action::RepoRead)
        .await
        .unwrap();
    svc.set_collaborator(&alice, &a, "bob", Role::Read)
        .await
        .unwrap();

    // carol can't read the private repository.
    let err = svc
        .set_issue_assignees(&alice, "alice", "demo", 1, &["carol".into()])
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ServiceError::Invalid(f) if f[0].field == "assignees"),
        "{err:?}"
    );
    // An unknown name is rejected the same way.
    assert!(
        svc.set_issue_assignees(&alice, "alice", "demo", 1, &["nobody".into()])
            .await
            .is_err()
    );
    // bob can read it, so he can be assigned.
    svc.set_issue_assignees(&alice, "alice", "demo", 1, &["bob".into(), "alice".into()])
        .await
        .unwrap();
    // bob reads but doesn't triage.
    let err = svc
        .set_issue_assignees(&bob, "alice", "demo", 1, &[])
        .await
        .unwrap_err();
    assert!(matches!(err, ServiceError::Forbidden), "{err:?}");

    let (_, issue) = svc.issue(&alice, "alice", "demo", 1).await.unwrap();
    let meta = svc.issue_meta(&[issue.id]).await.unwrap();
    assert_eq!(meta.assignees[&issue.id], vec!["alice", "bob"]);

    // More than ten is rejected.
    let many: Vec<String> = (0..11).map(|i| format!("user{i}")).collect();
    assert!(
        svc.set_issue_assignees(&alice, "alice", "demo", 1, &many)
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn filters_combine_with_and(pool: PgPool) {
    let (env, alice) = setup(pool).await;
    let svc = &env.svc;
    svc.save_label(&alice, "alice", "demo", None, "bug", "#d73a4a", "")
        .await
        .unwrap();
    svc.save_label(&alice, "alice", "demo", None, "ui", "0e8a16", "")
        .await
        .unwrap();
    svc.save_milestone(&alice, "alice", "demo", None, "v1", "", "2026-12-31", true)
        .await
        .unwrap();
    for t in ["bug+ui", "bug", "ui", "bug+ui+v1"] {
        svc.create_issue(&alice, "alice", "demo", t, "")
            .await
            .unwrap();
    }
    svc.set_issue_labels(&alice, "alice", "demo", 1, &["bug".into(), "UI".into()])
        .await
        .unwrap();
    svc.set_issue_labels(&alice, "alice", "demo", 2, &["bug".into()])
        .await
        .unwrap();
    svc.set_issue_labels(&alice, "alice", "demo", 3, &["ui".into()])
        .await
        .unwrap();
    svc.set_issue_labels(&alice, "alice", "demo", 4, &["bug".into(), "ui".into()])
        .await
        .unwrap();
    svc.set_issue_milestone(&alice, "alice", "demo", 4, Some("V1"))
        .await
        .unwrap();
    svc.set_issue_assignees(&alice, "alice", "demo", 2, &["alice".into()])
        .await
        .unwrap();

    let me = &alice;
    let list = |f: FilterInput| async move {
        let (_, items, counts) = svc
            .issue_list(me, "alice", "demo", &f, 0, 50)
            .await
            .unwrap();
        (numbers(&items), counts)
    };
    let labels = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        list(FilterInput {
            labels: labels(&["bug"]),
            ..FilterInput::default()
        })
        .await,
        (vec![1, 2, 4], (3, 0))
    );
    assert_eq!(
        list(FilterInput {
            labels: labels(&["bug", "ui"]),
            ..FilterInput::default()
        })
        .await
        .0,
        vec![1, 4]
    );
    assert_eq!(
        list(FilterInput {
            labels: labels(&["bug", "ui"]),
            milestone: Some("v1".into()),
            ..FilterInput::default()
        })
        .await
        .0,
        vec![4]
    );
    assert_eq!(
        list(FilterInput {
            labels: labels(&["bug"]),
            assignee: Some("alice".into()),
            ..FilterInput::default()
        })
        .await
        .0,
        vec![2]
    );
    // An unknown label matches nothing rather than being ignored.
    assert_eq!(
        list(FilterInput {
            labels: labels(&["bug", "nope"]),
            ..FilterInput::default()
        })
        .await
        .0,
        Vec::<i32>::new()
    );
    assert_eq!(
        list(FilterInput {
            author: Some("nobody".into()),
            ..FilterInput::default()
        })
        .await
        .0,
        Vec::<i32>::new()
    );
    assert_eq!(list(FilterInput::default()).await.0, vec![1, 2, 3, 4]);

    // Changes are recorded on the timeline.
    let (a, issue) = svc.issue(&alice, "alice", "demo", 4).await.unwrap();
    let kinds: Vec<String> = svc
        .issue_timeline(&alice, &a, &issue)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|t| match t {
            slath_service::issues::TimelineItem::Event(e, _) => Some(e.kind),
            slath_service::issues::TimelineItem::Comment { .. } => None,
        })
        .collect();
    assert_eq!(kinds, ["labeled", "labeled", "milestoned"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn labels_and_milestones_are_validated_and_need_maintain(pool: PgPool) {
    let (env, alice) = setup(pool).await;
    let svc = &env.svc;
    let (_, bob) = common::user(svc, "bob").await;
    let a = svc
        .repo_access(&alice, "alice", "demo", slath_core::authz::Action::RepoRead)
        .await
        .unwrap();
    svc.set_collaborator(&alice, &a, "bob", Role::Write)
        .await
        .unwrap();
    for (name, color) in [("", "ffffff"), ("x", "red"), (&"y".repeat(51), "ffffff")] {
        assert!(matches!(
            svc.save_label(&alice, "alice", "demo", None, name, color, "")
                .await,
            Err(ServiceError::Invalid(_))
        ));
    }
    svc.save_label(&alice, "alice", "demo", None, "bug", "fff000", "")
        .await
        .unwrap();
    // Names are unique regardless of case.
    assert!(matches!(
        svc.save_label(&alice, "alice", "demo", None, "Bug", "fff000", "")
            .await,
        Err(ServiceError::Invalid(_))
    ));
    assert!(matches!(
        svc.save_milestone(&alice, "alice", "demo", None, "v1", "", "31.12.2026", true)
            .await,
        Err(ServiceError::Invalid(_))
    ));
    // Write may triage but not manage labels.
    assert!(matches!(
        svc.save_label(&bob, "alice", "demo", None, "ui", "fff000", "")
            .await,
        Err(ServiceError::Forbidden)
    ));
    svc.create_issue(&bob, "alice", "demo", "One", "")
        .await
        .unwrap();
    svc.set_issue_labels(&bob, "alice", "demo", 1, &["bug".into()])
        .await
        .unwrap();
    assert!(matches!(
        svc.set_issue_labels(&bob, "alice", "demo", 1, &["missing".into()])
            .await,
        Err(ServiceError::Invalid(_))
    ));
}
