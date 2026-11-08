// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reviews and requirements (REVIEW-01): who may approve, approvals that
//! survive a pure rebase and go stale after a content change, requests
//! for changes, drafts, and the review requests inbox.

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

struct Users {
    s: TestServer,
    alice: String,
    bob: String,
    carol: String,
}

fn token(s: &TestServer, user: &str) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, user, &["repo:write", "mrs:write"])
}

fn api(
    u: &Users,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> slath_tests::http::Resp {
    u.s.client().api(
        method,
        &format!("/api/v1/repos/alice/demo{path}"),
        Some(token),
        body.as_ref(),
    )
}

fn requirement(u: &Users, n: i64, key: &str) -> (String, String) {
    let m = api(
        u,
        &u.alice,
        "GET",
        &format!("/merge-requests/{n}/mergeability"),
        None,
    )
    .json();
    let r = m["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == key)
        .unwrap_or_else(|| panic!("no {key} in {m}"));
    (
        r["state"].as_str().unwrap().to_owned(),
        r["reason"].as_str().unwrap().to_owned(),
    )
}

fn wait_revisions(u: &Users, n: i64, k: i64) {
    let start = std::time::Instant::now();
    while api(u, &u.alice, "GET", &format!("/merge-requests/{n}"), None).json()["revisions"] != k {
        assert!(
            start.elapsed().as_secs() < 30,
            "no revision {k}\n{}",
            u.s.log()
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

fn lines(edit: Option<usize>) -> String {
    (0..20)
        .map(|i| {
            if Some(i) == edit {
                format!("line {i}, edited\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect()
}

#[test]
fn conf_review_01_approvals_rebases_and_requirements() {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    s.create_user("carol", PASSWORD, false);
    let u = Users {
        alice: token(&s, "alice"),
        bob: token(&s, "bob"),
        carol: token(&s, "carol"),
        s,
    };
    for (who, role) in [("bob", "write"), ("carol", "read")] {
        let r = api(
            &u,
            &u.alice,
            "PUT",
            &format!("/collaborators/{who}"),
            Some(json!({"role": role})),
        );
        assert!(r.status < 300, "{}", r.body);
    }
    let r = api(
        &u,
        &u.alice,
        "POST",
        "/protections",
        Some(json!({"pattern": "main", "required_approvals": 1, "block_force_push": false})),
    );
    assert_eq!(r.status, 200, "{}", r.body);

    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let url = u.s.url(&DEMO);
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "x\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "-c", "topic"]);
    std::fs::write(w.join("a.txt"), lines(None)).unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "Add a"]);
    git.ok(w, &["push", "-q", &url, "topic"]);
    let r = api(
        &u,
        &u.alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Add a"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    wait_revisions(&u, n, 1);
    assert_eq!(
        requirement(&u, n, "approvals"),
        ("unmet".into(), "0 of 1 required approvals.".into())
    );

    // The author can't approve; a reader's approval doesn't count.
    let r = api(
        &u,
        &u.alice,
        "POST",
        &format!("/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "approve"})),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    let r = api(
        &u,
        &u.carol,
        "POST",
        &format!("/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "approve"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(requirement(&u, n, "approvals").0, "unmet");

    // Requested reviewers find it in their inbox until they review.
    let r = api(
        &u,
        &u.alice,
        "PUT",
        &format!("/merge-requests/{n}/reviewers"),
        Some(json!({"reviewers": ["bob"]})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let mut bob_ui = u.s.login("bob", PASSWORD);
    assert!(bob_ui.get("/-/review-requests").body.contains("Add a"));
    let r = api(
        &u,
        &u.bob,
        "POST",
        &format!("/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "approve", "body": "Looks good."})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    assert!(!bob_ui.get("/-/review-requests").body.contains("Add a"));
    assert_eq!(
        requirement(&u, n, "approvals"),
        ("met".into(), "Approved by bob.".into())
    );
    let m = api(
        &u,
        &u.alice,
        "GET",
        &format!("/merge-requests/{n}/mergeability"),
        None,
    )
    .json();
    assert_eq!(m["mergeable"], true, "{m}");

    // A pure rebase keeps the approval.
    git.ok(w, &["switch", "-q", "main"]);
    std::fs::write(w.join("other.txt"), "o\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "Other"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "topic"]);
    git.ok(w, &["rebase", "-q", "main"]);
    git.ok(w, &["push", "-q", "--force", &url, "topic"]);
    wait_revisions(&u, n, 2);
    assert_eq!(requirement(&u, n, "approvals").0, "met");

    // A content change makes it stale.
    std::fs::write(w.join("a.txt"), lines(Some(4))).unwrap();
    git.ok(w, &["commit", "-qa", "--amend", "--no-edit"]);
    git.ok(w, &["push", "-q", "--force", &url, "topic"]);
    wait_revisions(&u, n, 3);
    assert_eq!(
        requirement(&u, n, "approvals"),
        (
            "unmet".into(),
            "0 of 1 required approvals (1 approval is stale after later changes).".into()
        )
    );
    let reviews = api(
        &u,
        &u.alice,
        "GET",
        &format!("/merge-requests/{n}/reviews"),
        None,
    )
    .json();
    let bobs: Vec<&Value> = reviews
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["reviewer"] == "bob")
        .collect();
    assert_eq!(
        (bobs[0]["counts"].clone(), bobs[0]["stale"].clone()),
        (json!(false), json!(true))
    );

    // A request for changes blocks until a later approval.
    let r = api(
        &u,
        &u.bob,
        "POST",
        &format!("/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "request_changes", "body": "Please rename it."})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(
        requirement(&u, n, "changes_requested"),
        ("unmet".into(), "bob requested changes.".into())
    );
    api(
        &u,
        &u.bob,
        "POST",
        &format!("/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "approve"})),
    );
    assert_eq!(requirement(&u, n, "changes_requested").0, "met");
    assert_eq!(requirement(&u, n, "approvals").0, "met");

    // Drafts aren't mergeable; a comment needs a body.
    api(
        &u,
        &u.alice,
        "PATCH",
        &format!("/merge-requests/{n}"),
        Some(json!({"draft": true})),
    );
    assert_eq!(requirement(&u, n, "draft").0, "unmet");
    let r = api(
        &u,
        &u.bob,
        "POST",
        &format!("/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "comment"})),
    );
    assert_eq!(r.status, 422, "{}", r.body);

    // The timeline notes the carried and the stale approvals.
    let page =
        u.s.login("alice", PASSWORD)
            .get(&format!("/alice/demo/merge-requests/{n}"))
            .body;
    assert!(
        page.contains("kept earlier approvals: revision 2 has the same patches"),
        "{page}"
    );
    assert!(page.contains("made earlier approvals stale: revision 3 changed the patches"));
}

#[test]
fn conf_review_01_resolved_threads_and_up_to_date_are_evaluated() {
    let s = TestServer::start();
    let alice = token(&s, "alice");
    let u = Users {
        bob: String::new(),
        carol: String::new(),
        alice,
        s,
    };
    let r = api(
        &u,
        &u.alice,
        "POST",
        "/protections",
        Some(
            json!({"pattern": "main", "required_approvals": 0, "require_resolved_threads": true, "require_up_to_date": true}),
        ),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let url = u.s.url(&DEMO);
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "x\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "-c", "topic"]);
    std::fs::write(w.join("t.txt"), "t\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "Topic"]);
    git.ok(w, &["push", "-q", &url, "topic"]);
    let n = api(
        &u,
        &u.alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": "T"})),
    )
    .json()["number"]
        .as_i64()
        .unwrap();
    wait_revisions(&u, n, 1);
    assert_eq!(requirement(&u, n, "threads").0, "met");
    assert_eq!(requirement(&u, n, "up_to_date").0, "met");
    assert_eq!(
        requirement(&u, n, "approvals"),
        ("met".into(), "No approvals are required.".into())
    );
    git.ok(w, &["switch", "-q", "main"]);
    std::fs::write(w.join("m.txt"), "m\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "Main"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    assert_eq!(
        requirement(&u, n, "up_to_date"),
        (
            "unmet".into(),
            "main has commits the branch doesn't.".into()
        )
    );
}
