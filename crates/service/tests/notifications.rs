// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Notifications (ISSUE-02): who hears about what, never the actor, never
//! anyone who can't read the repository, and hidden once access is lost.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use slath_core::authz::{Action, Role, Subject, Visibility};
use slath_service::Services;
use slath_service::notifications::{NOTIFY_FANOUT, Subscription};
use slath_service::repos::NewRepoInput;
use sqlx::PgPool;

/// Runs every queued fan-out job.
async fn fan_out(svc: &Services) {
    while let Some(job) = slath_db::jobs::lease(
        &svc.pool,
        "default",
        "test",
        std::time::Duration::from_secs(60),
    )
    .await
    .unwrap()
    {
        if job.kind == NOTIFY_FANOUT {
            svc.run_fanout(&job).await.unwrap();
        }
        slath_db::jobs::complete(&svc.pool, job.id, "test")
            .await
            .unwrap();
    }
}

async fn inbox(svc: &Services, who: &Subject) -> Vec<(String, String, bool)> {
    svc.inbox(who, false, 0, 50)
        .await
        .unwrap()
        .into_iter()
        .map(|i| {
            (
                format!("{}#{}", i.repo, i.number),
                i.notification.reason,
                i.notification.unread,
            )
        })
        .collect()
}

async fn setup(pool: PgPool) -> (common::Env, Subject, Subject, Subject) {
    let env = common::services(pool, "");
    let (_, alice) = common::user(&env.svc, "alice").await;
    let (_, bob) = common::user(&env.svc, "bob").await;
    let (_, carol) = common::user(&env.svc, "carol").await;
    env.svc
        .create_repo(
            &alice,
            NewRepoInput {
                namespace: "alice".into(),
                name: "secret".into(),
                description: String::new(),
                visibility: Visibility::Private,
                default_branch: "main".into(),
            },
        )
        .await
        .unwrap();
    let a = env
        .svc
        .repo_access(&alice, "alice", "secret", Action::RepoRead)
        .await
        .unwrap();
    env.svc
        .set_collaborator(&alice, &a, "bob", Role::Read)
        .await
        .unwrap();
    (env, alice, bob, carol)
}

#[sqlx::test(migrations = "../../migrations")]
async fn conf_issue_02_notifications_follow_access(pool: PgPool) {
    let (env, alice, bob, carol) = setup(pool).await;
    let svc = &env.svc;

    // bob opens an issue: alice watches her repository; bob is the actor.
    svc.create_issue(&bob, "alice", "secret", "Crash", "It crashes. cc @carol")
        .await
        .unwrap();
    fan_out(svc).await;
    assert_eq!(
        inbox(svc, &alice).await,
        [("alice/secret#1".into(), "watching".into(), true)]
    );
    assert!(inbox(svc, &bob).await.is_empty(), "no self-notifications");
    assert!(
        inbox(svc, &carol).await.is_empty(),
        "mentioned without read access: nothing"
    );

    // alice comments: bob hears as the author; alice doesn't.
    svc.add_issue_comment(&alice, "alice", "secret", 1, "Thanks @bob")
        .await
        .unwrap();
    fan_out(svc).await;
    let b = inbox(svc, &bob).await;
    assert_eq!(b, [("alice/secret#1".into(), "mentioned".into(), true)]);
    let (_, issue) = svc.issue(&bob, "alice", "secret", 1).await.unwrap();
    svc.mark_issue_read(&bob, &issue).await;
    assert!(!inbox(svc, &bob).await[0].2);

    // bob loses access: no new notification, and the old row is hidden.
    let a = svc
        .repo_access(&alice, "alice", "secret", Action::RepoRead)
        .await
        .unwrap();
    svc.remove_collaborator(&alice, &a, "bob").await.unwrap();
    svc.add_issue_comment(&alice, "alice", "secret", 1, "More")
        .await
        .unwrap();
    fan_out(svc).await;
    assert!(inbox(svc, &bob).await.is_empty());
    let unread: bool = sqlx::query_scalar(
        "SELECT unread FROM notifications n JOIN users u ON u.id = n.user_id
          JOIN namespaces s ON s.id = u.namespace_id WHERE s.name = 'bob'",
    )
    .fetch_one(&svc.pool)
    .await
    .unwrap();
    assert!(!unread, "the row wasn't bumped after access was lost");
}

#[sqlx::test(migrations = "../../migrations")]
async fn ignoring_and_unsubscribing_silence_a_thread(pool: PgPool) {
    let (env, alice, bob, _) = setup(pool).await;
    let svc = &env.svc;
    svc.create_issue(&alice, "alice", "secret", "One", "")
        .await
        .unwrap();
    svc.create_issue(&alice, "alice", "secret", "Two", "")
        .await
        .unwrap();
    // bob watches the repository and ignores issue 1.
    svc.set_repo_subscription(&bob, "alice", "secret", Subscription::Watching)
        .await
        .unwrap();
    svc.set_issue_subscription(&bob, "alice", "secret", 1, Subscription::Ignoring)
        .await
        .unwrap();
    svc.add_issue_comment(&alice, "alice", "secret", 1, "a")
        .await
        .unwrap();
    svc.add_issue_comment(&alice, "alice", "secret", 2, "b")
        .await
        .unwrap();
    fan_out(svc).await;
    let b = inbox(svc, &bob).await;
    assert_eq!(b, [("alice/secret#2".into(), "watching".into(), true)]);

    // The unsubscribe link from an email ignores issue 2 without a session.
    let (_, two) = svc.issue(&bob, "alice", "secret", 2).await.unwrap();
    let Subject::User { id: bob_id, .. } = bob else {
        unreachable!()
    };
    let token = svc.unsubscribe_token(bob_id, "issue", two.id);
    assert_eq!(
        svc.unsubscribe_target(&token).await.unwrap().as_deref(),
        Some("alice/secret#2")
    );
    svc.unsubscribe(&token).await.unwrap();
    assert_eq!(
        svc.issue_subscription(&bob, &two).await,
        Subscription::Ignoring
    );
    let forged = format!("{}x", &token[..token.len() - 1]);
    assert!(svc.unsubscribe(&forged).await.is_err());
    svc.add_issue_comment(&alice, "alice", "secret", 2, "c")
        .await
        .unwrap();
    fan_out(svc).await;
    assert!(inbox(svc, &bob).await.iter().all(|r| !r.2));
}

#[sqlx::test(migrations = "../../migrations")]
async fn assignment_notifies_the_assignee(pool: PgPool) {
    let (env, alice, bob, _) = setup(pool).await;
    let svc = &env.svc;
    svc.create_issue(&alice, "alice", "secret", "Task", "")
        .await
        .unwrap();
    fan_out(svc).await;
    svc.set_issue_assignees(&alice, "alice", "secret", 1, &["bob".into()])
        .await
        .unwrap();
    fan_out(svc).await;
    assert_eq!(
        inbox(svc, &bob).await,
        [("alice/secret#1".into(), "assigned".into(), true)]
    );
    assert!(inbox(svc, &alice).await.iter().all(|r| r.1 == "watching"));
}
