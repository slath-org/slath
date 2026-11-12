// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reviews and requirements (REVIEW-01): who may approve, approvals that
//! survive a pure rebase and go stale after a content change, requests
//! for changes, drafts, and the review requests inbox. Approval coverage
//! (REVIEW-14): what an approver had viewed, snapshot at approval.

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
    slath_tests::http::create_token(&mut c, user, &["repo:write", "mrs:write", "statuses:write"])
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
        (
            "met".into(),
            "Approved by bob (r1, 0 of 1 file viewed).".into()
        )
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

/// alice/demo with `main` and a merge request from `topic`, a protection
/// requiring `ci/build`, and the head commit.
fn status_setup(protection: Value) -> (Users, i64, String, String) {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    s.create_user("carol", PASSWORD, false);
    let u = Users {
        alice: token(&s, "alice"),
        bob: token(&s, "bob"),
        carol: token(&s, "carol"),
        s,
    };
    for who in ["bob", "carol"] {
        let r = api(
            &u,
            &u.alice,
            "PUT",
            &format!("/collaborators/{who}"),
            Some(json!({"role": "write"})),
        );
        assert!(r.status < 300, "{}", r.body);
    }
    let r = api(&u, &u.alice, "POST", "/protections", Some(protection));
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
    let main = git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned();
    git.ok(w, &["switch", "-q", "-c", "topic"]);
    std::fs::write(w.join("t.txt"), "t\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "Topic"]);
    git.ok(w, &["push", "-q", &url, "topic"]);
    let head = git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned();
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
    (u, n, head, main)
}

fn report(u: &Users, token: &str, oid: &str, state: &str) -> slath_tests::http::Resp {
    api(
        u,
        token,
        "POST",
        &format!("/statuses/{oid}"),
        Some(
            json!({"state": state, "context": "ci/build", "target_url": "https://ci.example.org/1"}),
        ),
    )
}

#[test]
fn conf_review_04_only_statuses_on_the_landing_commit_count() {
    let (u, n, head, main) = status_setup(
        json!({"pattern": "main", "required_approvals": 0, "required_status_contexts": ["ci/build"]}),
    );
    assert_eq!(
        requirement(&u, n, "statuses"),
        ("pending".into(), "ci/build: no status yet.".into())
    );
    // A token without statuses:write can't report.
    let mut c = u.s.login("bob", PASSWORD);
    let read_only = slath_tests::http::create_token(&mut c, "ro", &["repo:read"]);
    assert_eq!(report(&u, &read_only, &head, "success").status, 403);
    // Unknown commits, and other repositories, are refused.
    assert_eq!(report(&u, &u.bob, &"0".repeat(40), "success").status, 404);
    // A success on another commit doesn't count.
    assert_eq!(report(&u, &u.bob, &main, "success").status, 201);
    assert_eq!(requirement(&u, n, "statuses").0, "pending");
    // On the head it does; the latest report wins.
    assert_eq!(report(&u, &u.bob, &head, "failure").status, 201);
    assert_eq!(
        requirement(&u, n, "statuses"),
        ("unmet".into(), "ci/build: failure, reported by bob.".into())
    );
    assert_eq!(report(&u, &u.bob, &head, "success").status, 201);
    assert_eq!(
        requirement(&u, n, "statuses"),
        ("met".into(), "ci/build: success, reported by bob.".into())
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
    // The statuses show their reporter and evidence class.
    let list = api(
        &u,
        &u.alice,
        "GET",
        &format!("/commits/{head}/statuses"),
        None,
    )
    .json();
    assert_eq!(list[0]["reporter"], "bob");
    assert_eq!(list[0]["evidence"], "reported");
    assert_eq!(list.as_array().unwrap().len(), 1);
    // Bad input.
    for body in [
        json!({"state": "done", "context": "ci/build"}),
        json!({"state": "success", "context": "bad\ncontext"}),
        json!({"state": "success", "context": "ci", "target_url": "http://ci.example.org"}),
    ] {
        assert_eq!(
            api(&u, &u.bob, "POST", &format!("/statuses/{head}"), Some(body)).status,
            422
        );
    }
    // The merge request shows who reported it.
    let page =
        u.s.login("alice", PASSWORD)
            .get(&format!("/alice/demo/merge-requests/{n}"))
            .body;
    assert!(page.contains("Reported by bob"));
}

#[test]
fn conf_review_11_who_may_satisfy_required_checks() {
    let (u, n, head, _) = status_setup(
        json!({"pattern": "main", "required_approvals": 0, "required_status_contexts": ["ci/build"]}),
    );
    // The author's own report never counts.
    assert_eq!(report(&u, &u.alice, &head, "success").status, 201);
    let (st, reason) = requirement(&u, n, "statuses");
    assert_eq!(st, "unmet");
    assert!(
        reason.contains(
            "the report by alice doesn't count: the author can't report their own checks"
        ),
        "{reason}"
    );
    // Nor a bot the author owns.
    let (ok, out, err) = u.s.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            "alice",
            "--name",
            "alicebot",
            "--token-scopes",
            "repo:write,statuses:write",
            "--repo",
            "alice/demo",
            "--output",
            "json",
        ],
        None,
    );
    assert!(ok, "{out}{err}");
    let bot = serde_json::from_str::<Value>(&out).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = api(
        &u,
        &u.alice,
        "PUT",
        "/collaborators/alicebot",
        Some(json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    assert_eq!(report(&u, &bot, &head, "success").status, 201);
    let reason = requirement(&u, n, "statuses").1;
    assert!(
        reason.contains(
            "the report by alicebot doesn't count: bots of the author can't report its checks"
        ),
        "{reason}"
    );
    // With a reporter list, only listed reporters count.
    let r = api(
        &u,
        &u.alice,
        "POST",
        "/protections",
        Some(json!({
            "pattern": "main", "required_approvals": 0,
            "required_status_contexts": ["ci/build"],
            "status_reporters": {"ci/build": ["carol"]},
        })),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["status_reporters"]["ci/build"], json!(["carol"]));
    assert_eq!(report(&u, &u.bob, &head, "success").status, 201);
    let reason = requirement(&u, n, "statuses").1;
    assert!(
        reason.contains("the report by bob doesn't count: only the listed reporters count"),
        "{reason}"
    );
    assert_eq!(report(&u, &u.carol, &head, "success").status, 201);
    assert_eq!(
        requirement(&u, n, "statuses"),
        ("met".into(), "ci/build: success, reported by carol.".into())
    );
}

#[test]
fn conf_review_13_returning_reviewer_and_the_strict_rule() {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let u = Users {
        alice: token(&s, "alice"),
        bob: token(&s, "bob"),
        carol: String::new(),
        s,
    };
    let r = api(
        &u,
        &u.alice,
        "PUT",
        "/collaborators/bob",
        Some(json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
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
    let w = d.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    let url = u.s.url(&DEMO);
    git.ok(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "x\n").unwrap();
    git.ok(&w, &["add", "."]);
    git.ok(&w, &["commit", "-q", "-m", "init"]);
    git.ok(&w, &["push", "-q", &url, "main"]);
    git.ok(&w, &["switch", "-q", "-c", "topic"]);
    std::fs::write(w.join("a.txt"), lines(None)).unwrap();
    git.ok(&w, &["add", "."]);
    git.ok(&w, &["commit", "-q", "-m", "Add a"]);
    git.ok(&w, &["push", "-q", &url, "topic"]);
    let n = api(
        &u,
        &u.alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Topic"})),
    )
    .json()["number"]
        .as_i64()
        .unwrap();
    wait_revisions(&u, n, 1);
    assert_eq!(
        api(
            &u,
            &u.bob,
            "POST",
            &format!("/merge-requests/{n}/reviews"),
            Some(json!({"verdict": "approve"}))
        )
        .status,
        201
    );
    let mut bob = u.s.login("bob", PASSWORD);
    let page = bob.get(&format!("/alice/demo/merge-requests/{n}")).body;
    assert!(
        page.contains(r#"id="readiness-title" class="readiness-title">Ready to merge"#),
        "{page}"
    );

    // Git, then jj, change the content.
    std::fs::write(w.join("a.txt"), lines(Some(2))).unwrap();
    git.ok(&w, &["commit", "-qa", "--amend", "--no-edit"]);
    git.ok(&w, &["push", "-q", "--force", &url, "topic"]);
    wait_revisions(&u, n, 2);
    let mut revisions = 2;
    if let Some((version, bin)) = crate::jj::jj_binaries().into_iter().last() {
        let jj = crate::jj::Jj::new(&version, &bin);
        let j = d.path().join("jj");
        jj.ok(d.path(), &["git", "clone", &url, j.to_str().unwrap()]);
        jj.ok(&j, &["bookmark", "track", "topic@origin"]);
        jj.ok(&j, &["edit", "topic"]);
        std::fs::write(j.join("a.txt"), lines(Some(5))).unwrap();
        jj.ok(&j, &["git", "push", "--bookmark", "topic"]);
        revisions = 3;
        wait_revisions(&u, n, 3);
    }
    // Bob returns: the headline, his next step, each requirement's tone,
    // and what's new since his review, marked in the timeline.
    let page = bob.get(&format!("/alice/demo/merge-requests/{n}")).body;
    assert!(
        page.contains(r#"class="readiness-title">Approval required"#),
        "{page}"
    );
    assert!(
        page.contains(&format!(
            r#"href="/alice/demo/merge-requests/{n}#review-form""#
        )),
        "{page}"
    );
    // Every revision since counts, plus the note that the approval went stale.
    let new_since: usize = page
        .split(" new since your review")
        .next()
        .and_then(|before| before.rsplit('>').next())
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0);
    assert!(
        new_since >= usize::try_from(revisions - 1).unwrap(),
        "{new_since}: {page}"
    );
    assert!(page.contains(r#"id="since""#));
    assert!(page.contains(r#"<span class="visually-hidden">Waiting: </span>"#));
    assert!(page.contains(r#"<span class="visually-hidden">Done: </span>"#));
    assert_eq!(
        page.matches("button button-primary").count(),
        1,
        "one primary action"
    );

    // The strict rule: even a pure rebase makes the approval stale.
    let r = api(
        &u,
        &u.alice,
        "POST",
        "/protections",
        Some(json!({
            "pattern": "main", "required_approvals": 1, "block_force_push": false,
            "dismiss_approvals_on_new_revision": true,
        })),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["dismiss_approvals_on_new_revision"], true);
    assert_eq!(
        api(
            &u,
            &u.bob,
            "POST",
            &format!("/merge-requests/{n}/reviews"),
            Some(json!({"verdict": "approve"}))
        )
        .status,
        201
    );
    assert_eq!(requirement(&u, n, "approvals").0, "met");
    git.ok(&w, &["fetch", "-q", &url, "topic"]);
    git.ok(&w, &["reset", "-q", "--hard", "FETCH_HEAD"]);
    git.ok(&w, &["switch", "-q", "main"]);
    std::fs::write(w.join("m.txt"), "m\n").unwrap();
    git.ok(&w, &["add", "."]);
    git.ok(&w, &["commit", "-q", "-m", "Main"]);
    git.ok(&w, &["push", "-q", &url, "main"]);
    git.ok(&w, &["switch", "-q", "topic"]);
    git.ok(&w, &["reset", "-q", "--hard", "FETCH_HEAD"]);
    git.ok(&w, &["rebase", "-q", "main"]);
    git.ok(&w, &["push", "-q", "--force", &url, "topic"]);
    wait_revisions(&u, n, revisions + 1);
    assert_eq!(requirement(&u, n, "approvals").0, "unmet");
    let ledger = api(
        &u,
        &u.alice,
        "GET",
        &format!("/merge-requests/{n}/ledger"),
        None,
    )
    .json();
    assert!(
        ledger
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "approval_stale" && e["reason"] == "new-revision"),
        "{ledger}"
    );
}

/// Marks `path` viewed (or not) as the signed-in user, as the page does.
fn mark_viewed(ui: &mut slath_tests::http::Client, n: i64, path: &str, viewed: bool) {
    let changes = format!("/alice/demo/merge-requests/{n}/changes");
    let r = slath_tests::http::submit(
        ui,
        &changes,
        &format!("/alice/demo/merge-requests/{n}/viewed"),
        &[
            ("path", path),
            ("viewed", if viewed { "true" } else { "false" }),
            ("back", &changes),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
}

#[test]
fn conf_review_14_approval_coverage_is_a_snapshot() {
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
    std::fs::write(w.join("gone.txt"), "bye\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "-c", "topic"]);
    for f in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(w.join(f), format!("{f}\n")).unwrap();
    }
    git.ok(w, &["rm", "-q", "gone.txt"]);
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "Add files"]);
    git.ok(w, &["push", "-q", &url, "topic"]);
    let r = api(
        &u,
        &u.alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Add files"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    wait_revisions(&u, n, 1);
    let coverage = |token: &str| {
        api(
            &u,
            token,
            "GET",
            &format!("/merge-requests/{n}/mergeability"),
            None,
        )
        .json()["coverage"]
            .clone()
    };
    assert_eq!(coverage(&u.alice), Value::Null, "no approvals, no coverage");

    // Bob views everything at r1, then c.txt changes in r2: only the
    // views of r2's blobs count (the deleted file's view still does).
    let mut bob_ui = u.s.login("bob", PASSWORD);
    for f in ["a.txt", "b.txt", "c.txt", "gone.txt"] {
        mark_viewed(&mut bob_ui, n, f, true);
    }
    std::fs::write(w.join("c.txt"), "c, changed\n").unwrap();
    git.ok(w, &["commit", "-qa", "--amend", "--no-edit"]);
    git.ok(w, &["push", "-q", "--force", &url, "topic"]);
    wait_revisions(&u, n, 2);
    let r = api(
        &u,
        &u.bob,
        "POST",
        &format!("/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "approve"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let expected = json!({
        "approvals": [{"reviewer": "bob", "revision": 2, "files_viewed": 3, "files_total": 4}],
        "unviewed_files": ["c.txt"],
    });
    assert_eq!(coverage(&u.alice), expected);
    assert_eq!(
        requirement(&u, n, "approvals"),
        (
            "met".into(),
            "Approved by bob (r2, 3 of 4 files viewed).".into()
        )
    );

    // Later views don't change it.
    mark_viewed(&mut bob_ui, n, "c.txt", true);
    mark_viewed(&mut bob_ui, n, "a.txt", false);
    assert_eq!(coverage(&u.alice), expected);

    // A pure rebase carries the approval with its coverage.
    git.ok(w, &["switch", "-q", "main"]);
    std::fs::write(w.join("other.txt"), "o\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "Other"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "topic"]);
    git.ok(w, &["rebase", "-q", "main"]);
    git.ok(w, &["push", "-q", "--force", &url, "topic"]);
    wait_revisions(&u, n, 3);
    assert_eq!(requirement(&u, n, "approvals").0, "met");
    assert_eq!(coverage(&u.alice), expected);

    // Readers see the snapshot, nothing more of Bob's viewed state.
    assert_eq!(coverage(&u.carol), expected);
    let mut carol_ui = u.s.login("carol", PASSWORD);
    let page = carol_ui
        .get(&format!("/alice/demo/merge-requests/{n}"))
        .body;
    assert!(
        page.contains("Approved by bob (r2, 3 of 4 files viewed).")
            && page.contains("Not viewed by an approver when approving")
            && page.contains("<li>c.txt</li>"),
        "{page}"
    );
    let changes = carol_ui
        .get(&format!("/alice/demo/merge-requests/{n}/changes"))
        .body;
    assert!(
        !changes.contains("You marked this file viewed")
            && !changes.contains(r#"aria-pressed="true""#),
        "{changes}"
    );
    // Bob's own viewed state is his (a.txt unmarked, c.txt marked).
    let changes = bob_ui
        .get(&format!("/alice/demo/merge-requests/{n}/changes"))
        .body;
    assert_eq!(
        changes.matches(r#"aria-pressed="true""#).count(),
        3,
        "{changes}"
    );
}
