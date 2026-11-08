// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merge request lifecycle (REVIEW-01) through Git and the API: the push
//! hint, creation, shared numbers, one open MR per source and target,
//! revisions from pushes, drafts, and closing when the source goes away.

use std::path::Path;

use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

fn work(git: &GitClient, dir: &Path, url: &str) {
    std::fs::create_dir_all(dir).unwrap();
    git.ok(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git.ok(dir, &["add", "."]);
    git.ok(dir, &["commit", "-q", "-m", "first"]);
    git.ok(dir, &["push", "-q", url, "main"]);
}

fn commit(git: &GitClient, dir: &Path, file: &str, text: &str) -> String {
    std::fs::write(dir.join(file), text).unwrap();
    git.ok(dir, &["add", "."]);
    git.ok(dir, &["commit", "-q", "-m", &format!("change {file}")]);
    git.ok(dir, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// Waits until `f` holds (jobs run in the background).
fn eventually(s: &TestServer, what: &str, mut f: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !f() {
        assert!(start.elapsed().as_secs() < 30, "{what}\n{}", s.log());
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[test]
fn conf_review_01_lifecycle_through_git_and_the_api() {
    let s = TestServer::start();
    let mut alice = s.login("alice", PASSWORD);
    let token = slath_tests::http::create_token(
        &mut alice,
        "mr",
        &["repo:write", "issues:write", "mrs:write"],
    );
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    let url = s.url(&DEMO);
    work(&git, &w, &url);
    git.ok(&w, &["switch", "-q", "-c", "topic"]);
    let first = commit(&git, &w, "b.txt", "b\n");

    // The push prints the link to a prefilled compare page.
    let out = git.cmd(&w).args(["push", &url, "topic"]).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("Create a merge request for topic"), "{err}");
    assert!(err.contains("/alice/demo/compare?source=topic"), "{err}");

    // An issue takes number 1, so the MR gets 2.
    let mut api = s.client();
    let issue = api.api(
        "POST",
        "/api/v1/repos/alice/demo/issues",
        Some(&token),
        Some(&serde_json::json!({"title": "bug"})),
    );
    assert_eq!(issue.status, 201, "{}", issue.body);
    let body = serde_json::json!({"source_branch": "topic", "title": "Add b", "description": "Adds b.", "draft": true});
    let r = api.api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&token),
        Some(&body),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let mr = r.json();
    assert_eq!(mr["number"], 2);
    assert_eq!(mr["target_branch"], "main");
    assert_eq!(mr["draft"], true);
    assert_eq!(mr["revisions"], 1);
    assert_eq!(mr["head_oid"], first.as_str());
    // A second open MR for the same source and target is refused.
    let r = api.api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&token),
        Some(&body),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("already open"), "{}", r.body);
    // Its head is fetchable at refs/merge-requests/2/head.
    git.ok(&w, &["fetch", "-q", &url, "refs/merge-requests/2/head"]);
    assert_eq!(git.ok(&w, &["rev-parse", "FETCH_HEAD"]).trim(), first);

    // The next push prints the MR and makes a revision.
    let second = commit(&git, &w, "c.txt", "c\n");
    let out = git.cmd(&w).args(["push", &url, "topic"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("merge request !2 for topic"));
    eventually(&s, "a second revision", || {
        api.api(
            "GET",
            "/api/v1/repos/alice/demo/merge-requests/2",
            Some(&token),
            None,
        )
        .json()["revisions"]
            == 2
    });
    let got = api
        .api(
            "GET",
            "/api/v1/repos/alice/demo/merge-requests/2",
            Some(&token),
            None,
        )
        .json();
    assert_eq!(got["head_oid"], second.as_str());
    git.ok(&w, &["fetch", "-q", &url, "refs/merge-requests/2/head"]);
    assert_eq!(git.ok(&w, &["rev-parse", "FETCH_HEAD"]).trim(), second);

    // Ready, comment, close, reopen.
    let patch = |b: serde_json::Value| {
        let mut c = s.client();
        c.api(
            "PATCH",
            "/api/v1/repos/alice/demo/merge-requests/2",
            Some(&token),
            Some(&b),
        )
    };
    assert_eq!(
        patch(serde_json::json!({"draft": false})).json()["draft"],
        false
    );
    let c = api.api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests/2/comments",
        Some(&token),
        Some(&serde_json::json!({"body": "Looks good"})),
    );
    assert_eq!(c.status, 201, "{}", c.body);
    assert_eq!(
        patch(serde_json::json!({"state": "closed"})).json()["state"],
        "closed"
    );
    assert_eq!(
        patch(serde_json::json!({"state": "open"})).json()["state"],
        "open"
    );
    let list = api
        .api(
            "GET",
            "/api/v1/repos/alice/demo/merge-requests",
            Some(&token),
            None,
        )
        .json();
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");

    // Deleting the source branch closes the MR; reopening needs the branch.
    git.ok(&w, &["push", "-q", &url, ":topic"]);
    eventually(&s, "closed after the branch was deleted", || {
        api.api(
            "GET",
            "/api/v1/repos/alice/demo/merge-requests/2",
            Some(&token),
            None,
        )
        .json()["state"]
            == "closed"
    });
    let r = patch(serde_json::json!({"state": "open"}));
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("source branch was deleted"), "{}", r.body);
}

#[test]
fn conf_review_01_only_people_who_can_push_open_merge_requests() {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w, &s.url(&DEMO));
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "main:topic"]);
    // bob can't see the private repository at all.
    let mut b = s.login("bob", PASSWORD);
    let token = slath_tests::http::create_token(&mut b, "mrs", &["repo:write", "mrs:write"]);
    let r = b.api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&token),
        Some(&serde_json::json!({"source_branch": "topic", "title": "x"})),
    );
    assert_eq!(r.status, 404, "{}", r.body);
    // A token without mrs:write can't open one.
    let mut a = s.login("alice", PASSWORD);
    let read = slath_tests::http::create_token(&mut a, "read", &["repo:read"]);
    let r = a.api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&read),
        Some(&serde_json::json!({"source_branch": "topic", "title": "x"})),
    );
    assert!(
        r.status == 403 || r.status == 404,
        "{}: {}",
        r.status,
        r.body
    );
}
