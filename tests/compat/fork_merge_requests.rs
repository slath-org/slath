// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merge requests from forks (REVIEW-06): a non-member forks a public
//! repository, pushes, and opens a merge request against the parent. The
//! parent holds nothing of the fork until then; afterwards it fetches only
//! the source branch's history, a revision per fork push, and merges like a
//! same-repository merge request. A less visible source needs the
//! author's confirmation, which the timeline records.

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestRepo, TestServer};

fn token(s: &TestServer, user: &str) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, user, &["repo:write", "mrs:write"])
}

fn api(
    s: &TestServer,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> slath_tests::http::Resp {
    s.client().api(
        method,
        &format!("/api/v1/repos{path}"),
        Some(token),
        body.as_ref(),
    )
}

/// Waits until `f` holds (jobs run in the background).
fn eventually(s: &TestServer, what: &str, mut f: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !f() {
        assert!(start.elapsed().as_secs() < 30, "{what}\n{}", s.log());
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

fn fork(s: &TestServer, token: &str, body: Value, path: &str) {
    let r = api(s, token, "POST", "/alice/demo/forks", Some(body));
    assert_eq!(r.status, 202, "{}", r.body);
    eventually(s, "fork not done", || {
        api(s, token, "GET", path, None).json()["state"] == "done"
    });
}

fn http_url(s: &TestServer, token: &str, repo: &str) -> String {
    format!(
        "http://git:{token}@{}/{repo}.git",
        s.base().trim_start_matches("http://")
    )
}

fn commit(git: &GitClient, dir: &std::path::Path, file: &str) -> String {
    std::fs::write(dir.join(file), format!("{file}\n")).unwrap();
    git.ok(dir, &["add", "."]);
    git.ok(dir, &["commit", "-q", "-m", &format!("Add {file}")]);
    git.ok(dir, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn has_object(git: &GitClient, repo: &std::path::Path, oid: &str) -> bool {
    git.cmd(repo)
        .args(["cat-file", "-e", oid])
        .status()
        .unwrap()
        .success()
}

#[test]
fn conf_review_06_fork_merge_requests_with_git() {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let (alice, bob) = (token(&s, "alice"), token(&s, "bob"));
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("alice");
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    commit(&git, &w, "README");
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "main"]);
    let r = api(
        &s,
        &alice,
        "PATCH",
        "/alice/demo",
        Some(json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);

    // Bob isn't a member; he forks and pushes a topic branch.
    fork(&s, &bob, json!({"namespace": "bob"}), "/bob/demo/fork");
    let b = d.path().join("bob");
    git.ok(
        d.path(),
        &["clone", "-q", &http_url(&s, &bob, "bob/demo"), "bob"],
    );
    git.ok(&b, &["config", "user.name", "Bob"]);
    git.ok(&b, &["config", "user.email", "bob@example.org"]);
    git.ok(&b, &["switch", "-q", "-c", "topic"]);
    let first = commit(&git, &b, "a.txt");
    git.ok(&b, &["push", "-q", "origin", "topic"]);
    // Something else of the fork that no merge request names.
    git.ok(&b, &["switch", "-q", "-c", "other", "main"]);
    let other = commit(&git, &b, "other.txt");
    git.ok(&b, &["push", "-q", "origin", "other"]);
    let parent = s.repo_path(&DEMO);
    assert!(
        !has_object(&git, &parent, &first),
        "the parent holds no fork objects before a merge request exists"
    );

    // Only repositories of the network that the author can push to.
    let r = api(
        &s,
        &bob,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_repo": "bob/nope", "source_branch": "topic", "title": "A"})),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("source_repo"), "{}", r.body);
    let r = api(
        &s,
        &alice,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_repo": "bob/demo", "source_branch": "topic", "title": "A"})),
    );
    assert_eq!(r.status, 422, "alice can't push to bob's fork: {}", r.body);

    let r = api(
        &s,
        &bob,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_repo": "bob/demo", "source_branch": "topic", "title": "Add a"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let m = r.json();
    let n = m["number"].as_i64().unwrap();
    assert_eq!(m["source_repo"], "bob/demo", "{m}");
    assert_eq!(m["allow_maintainer_edits"], true, "{m}");
    assert_eq!(m["head_oid"], first.as_str(), "{m}");
    assert!(has_object(&git, &parent, &first));
    assert!(
        !has_object(&git, &parent, &other),
        "only the source branch's history comes along"
    );
    let refs = git.ok(&parent, &["for-each-ref", "--format=%(refname)"]);
    assert!(refs.contains("/incoming"), "{refs}");
    assert!(
        refs.contains(&format!("refs/merge-requests/{n}/head")),
        "{refs}"
    );
    assert!(!refs.contains("refs/heads/topic"), "{refs}");

    // A second open MR for the same source and target is refused.
    let r = api(
        &s,
        &bob,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_repo": "bob/demo", "source_branch": "topic", "title": "Again"})),
    );
    assert_eq!(r.status, 422, "{}", r.body);

    // Each fork push makes a revision.
    git.ok(&b, &["switch", "-q", "topic"]);
    let second = commit(&git, &b, "b.txt");
    git.ok(&b, &["push", "-q", "origin", "topic"]);
    eventually(&s, "no second revision", || {
        let m = api(
            &s,
            &bob,
            "GET",
            &format!("/alice/demo/merge-requests/{n}"),
            None,
        )
        .json();
        m["revisions"] == 2 && m["head_oid"] == second.as_str()
    });

    // It merges like a same-repository MR, and the fork's branch stays.
    let r = api(
        &s,
        &alice,
        "POST",
        &format!("/alice/demo/merge-requests/{n}/merge"),
        Some(json!({"strategy": "fast-forward", "expected_head_oid": second})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        git.ok(&parent, &["rev-parse", "refs/heads/main"]).trim(),
        second
    );
    let fork_dir = s.repo_path(&TestRepo {
        namespace: "bob",
        name: "demo",
    });
    assert_eq!(
        git.ok(&fork_dir, &["rev-parse", "refs/heads/topic"]).trim(),
        second,
        "merging leaves the fork's branch alone"
    );
}

#[test]
fn conf_review_06_less_visible_sources_need_confirmation() {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let (alice, bob) = (token(&s, "alice"), token(&s, "bob"));
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("alice");
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    commit(&git, &w, "README");
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "main"]);
    let r = api(
        &s,
        &alice,
        "PATCH",
        "/alice/demo",
        Some(json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);
    fork(
        &s,
        &bob,
        json!({"namespace": "bob", "name": "hidden", "visibility": "private"}),
        "/bob/hidden/fork",
    );
    let b = d.path().join("bob");
    git.ok(
        d.path(),
        &["clone", "-q", &http_url(&s, &bob, "bob/hidden"), "bob"],
    );
    git.ok(&b, &["config", "user.name", "Bob"]);
    git.ok(&b, &["config", "user.email", "bob@example.org"]);
    git.ok(&b, &["switch", "-q", "-c", "topic"]);
    let oid = commit(&git, &b, "a.txt");
    git.ok(&b, &["push", "-q", "origin", "topic"]);

    let body = |publish: bool| {
        json!({"source_repo": "bob/hidden", "source_branch": "topic", "title": "Add a",
               "publish_source": publish})
    };
    let r = api(
        &s,
        &bob,
        "POST",
        "/alice/demo/merge-requests",
        Some(body(false)),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("publish_source"), "{}", r.body);
    assert!(
        !has_object(&git, &s.repo_path(&DEMO), &oid),
        "a refused merge request fetches nothing"
    );
    let r = api(
        &s,
        &bob,
        "POST",
        "/alice/demo/merge-requests",
        Some(body(true)),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let ev = s.db.query_one(
        "SELECT kind || ' ' || (data->>'source') FROM timeline_events \
          WHERE subject_kind = 'merge_request' AND kind = 'source_published'",
    );
    assert_eq!(ev, "source_published bob/hidden");
}
