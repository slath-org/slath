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

#[test]
fn conf_review_06_fork_content_meets_the_targets_push_protection() {
    use slath_core::secret_scan::samples;
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
    fork(&s, &bob, json!({"namespace": "bob"}), "/bob/demo/fork");
    // The fork doesn't protect itself; the target still does.
    s.db.execute(
        "UPDATE repositories r SET push_protection = 'off' FROM namespaces n \
          WHERE n.id = r.namespace_id AND n.name = 'bob'",
    );
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
    let r = api(
        &s,
        &bob,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_repo": "bob/demo", "source_branch": "topic", "title": "Add a"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    let path = format!("/alice/demo/merge-requests/{n}");

    let (_, key) = samples()
        .into_iter()
        .find(|(r, _)| *r == "private-key")
        .unwrap();
    std::fs::write(b.join("key.pem"), key).unwrap();
    git.ok(&b, &["add", "."]);
    git.ok(&b, &["commit", "-q", "-m", "Add a key"]);
    let secret = git.ok(&b, &["rev-parse", "HEAD"]).trim().to_owned();
    git.ok(&b, &["push", "-q", "origin", "topic"]);
    let mut held = Value::Null;
    eventually(&s, "the source wasn't held back", || {
        held = api(&s, &bob, "GET", &path, None).json();
        !held["held_source"].is_null()
    });
    assert_eq!(held["revisions"], 1, "{held}");
    assert_eq!(held["head_oid"], first.as_str(), "{held}");
    let h = &held["held_source"];
    assert_eq!(h["head"], secret.as_str(), "{held}");
    let f = &h["findings"][0];
    assert_eq!(f["rule"], "private-key", "{held}");
    assert_eq!(f["path"], "key.pem", "{held}");
    assert!(
        f["excerpt"].as_str().unwrap().len() < 80,
        "only a redacted excerpt: {held}"
    );
    let parent = s.repo_path(&DEMO);
    assert!(
        !has_object(&git, &parent, &secret),
        "a held-back head isn't fetched"
    );

    // The page names the findings; only target writers get allow links.
    let fp = f["fingerprint"].as_str().unwrap().to_owned();
    let mut bob_web = s.login("bob", PASSWORD);
    let page = bob_web.get(&format!("/alice/demo/merge-requests/{n}"));
    assert!(page.body.contains("wasn't taken over"), "{}", page.body);
    assert!(!page.body.contains(&fp), "{}", page.body);
    let mut alice_web = s.login("alice", PASSWORD);
    let page = alice_web.get(&format!("/alice/demo/merge-requests/{n}"));
    let link = format!("/alice/demo/-/secret-scan/allow?fingerprint={fp}&#38;rule=private-key");
    assert!(page.body.contains(&link), "{}", page.body);

    // Allowing it fetches the held-back head.
    let r = slath_tests::http::submit(
        &mut alice_web,
        &link.replace("&#38;", "&"),
        "/alice/demo/-/secret-scan/allow",
        &[
            ("fingerprint", &fp),
            ("rule", "private-key"),
            ("days", "1"),
            ("reason", "test fixture"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    eventually(&s, "the held-back head wasn't fetched", || {
        let m = api(&s, &bob, "GET", &path, None).json();
        m["revisions"] == 2 && m["held_source"].is_null() && m["head_oid"] == secret.as_str()
    });
    assert!(has_object(&git, &parent, &secret));
}

#[test]
fn conf_review_06_maintainer_edits() {
    let s = TestServer::start();
    for u in ["bob", "carol", "dave"] {
        s.create_user(u, PASSWORD, false);
    }
    let alice = token(&s, "alice");
    let (bob, carol, dave) = (token(&s, "bob"), token(&s, "carol"), token(&s, "dave"));
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("alice");
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    commit(&git, &w, "README");
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "main"]);
    for (method, path, body) in [
        ("PATCH", "/alice/demo", json!({"visibility": "public"})),
        (
            "PUT",
            "/alice/demo/collaborators/carol",
            json!({"role": "write"}),
        ),
    ] {
        let r = api(&s, &alice, method, path, Some(body));
        assert!(r.status < 300, "{}", r.body);
    }
    fork(&s, &bob, json!({"namespace": "bob"}), "/bob/demo/fork");
    let b = d.path().join("bob");
    git.ok(
        d.path(),
        &["clone", "-q", &http_url(&s, &bob, "bob/demo"), "bob"],
    );
    git.ok(&b, &["config", "user.name", "Bob"]);
    git.ok(&b, &["config", "user.email", "bob@example.org"]);
    git.ok(&b, &["switch", "-q", "-c", "topic"]);
    commit(&git, &b, "a.txt");
    git.ok(&b, &["push", "-q", "origin", "topic", "main:other"]);
    let r = api(
        &s,
        &bob,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_repo": "bob/demo", "source_branch": "topic", "title": "Add a"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    let path = format!("/alice/demo/merge-requests/{n}");

    // Carol writes to the target: she can update the MR's branch in the fork.
    let c = d.path().join("carol");
    git.ok(
        d.path(),
        &[
            "clone",
            "-q",
            "-b",
            "topic",
            &http_url(&s, &carol, "bob/demo"),
            "carol",
        ],
    );
    git.ok(&c, &["config", "user.name", "Carol"]);
    git.ok(&c, &["config", "user.email", "carol@example.org"]);
    let fork_url = http_url(&s, &carol, "bob/demo");
    let edit = commit(&git, &c, "carol.txt");
    git.ok(&c, &["push", "-q", &fork_url, "topic"]);
    eventually(&s, "no revision from the maintainer edit", || {
        api(&s, &bob, "GET", &path, None).json()["head_oid"] == edit.as_str()
    });
    assert_eq!(
        s.db.query_one(
            "SELECT count(*)::text FROM audit_events WHERE action = 'push.maintainer_edit'"
        ),
        "1"
    );
    // Only that branch, and no deletion or tags.
    let err = git.fails(&c, &["push", &fork_url, "topic:other"]);
    assert!(err.contains("edits by maintainers"), "{err}");
    git.fails(&c, &["push", &fork_url, "topic:refs/heads/new"]);
    git.fails(&c, &["push", &fork_url, ":topic"]);
    git.ok(&c, &["tag", "t1"]);
    git.fails(&c, &["push", &fork_url, "t1"]);

    // Nobody else: not a stranger, and the fork's owner gains nothing on
    // the target.
    git.ok(&c, &["switch", "-q", "-c", "dave"]);
    commit(&git, &c, "dave.txt");
    git.fails(
        &c,
        &["push", &http_url(&s, &dave, "bob/demo"), "HEAD:topic"],
    );
    git.fails(
        &b,
        &["push", &http_url(&s, &bob, "alice/demo"), "topic:main"],
    );

    // Alice can't change the setting (she can't push to the fork); Bob can.
    let toggle = |t: &str, allow: bool| {
        api(
            &s,
            t,
            "PATCH",
            &path,
            Some(json!({"allow_maintainer_edits": allow})),
        )
    };
    assert_eq!(toggle(&alice, false).status, 403);
    let r = toggle(&bob, false);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["allow_maintainer_edits"], false);
    git.ok(&c, &["switch", "-q", "topic"]);
    commit(&git, &c, "carol2.txt");
    git.fails(&c, &["push", &fork_url, "topic"]);
    assert_eq!(toggle(&bob, true).status, 200);
    git.ok(&c, &["push", "-q", &fork_url, "topic"]);
    // Closing the MR ends it.
    let r = api(&s, &bob, "PATCH", &path, Some(json!({"state": "closed"})));
    assert_eq!(r.status, 200, "{}", r.body);
    commit(&git, &c, "carol3.txt");
    git.fails(&c, &["push", &fork_url, "topic"]);

    // A "reverse" MR (from the parent into the fork) gives the fork's
    // writers nothing upstream.
    let r = api(
        &s,
        &alice,
        "POST",
        "/bob/demo/merge-requests",
        Some(json!({"source_repo": "alice/demo", "source_branch": "main",
                    "target_branch": "other", "title": "Reverse"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    git.ok(&b, &["switch", "-q", "main"]);
    commit(&git, &b, "bob-upstream.txt");
    git.fails(&b, &["push", &http_url(&s, &bob, "alice/demo"), "main"]);
}

#[test]
fn conf_review_06_fork_merge_requests_in_the_ui() {
    use slath_tests::http::submit;
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
    fork(&s, &bob, json!({"namespace": "bob"}), "/bob/demo/fork");
    fork(
        &s,
        &bob,
        json!({"namespace": "bob", "name": "hidden", "visibility": "private"}),
        "/bob/hidden/fork",
    );
    let mut web = s.login("bob", PASSWORD);
    let home = web.get("/bob/demo");
    assert!(
        home.body
            .contains("href=\"/alice/demo/compare?source_repo=bob%2Fdemo\">Contribute</a>"),
        "{}",
        home.body
    );

    // A push to the fork links to a merge request into the original.
    let b = d.path().join("bob");
    git.ok(
        d.path(),
        &["clone", "-q", &http_url(&s, &bob, "bob/demo"), "bob"],
    );
    git.ok(&b, &["config", "user.name", "Bob"]);
    git.ok(&b, &["config", "user.email", "bob@example.org"]);
    git.ok(&b, &["switch", "-q", "-c", "topic"]);
    commit(&git, &b, "a.txt");
    let out = git
        .cmd(&b)
        .args(["push", "origin", "topic"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("/alice/demo/compare?source_repo=bob%2Fdemo&source=topic"),
        "{err}"
    );
    let banner = web.get("/bob/demo");
    assert!(
        banner
            .body
            .contains("/alice/demo/compare?source_repo=bob%2Fdemo&amp;source=topic"),
        "{}",
        banner.body
    );

    // The compare page picks the fork and offers edits by maintainers.
    let page = "/alice/demo/compare?source_repo=bob%2Fdemo&source=topic";
    let p = web.get(page);
    assert_eq!(p.status, 200, "{}", p.body);
    assert!(
        p.body.contains("<option value=\"bob/demo\" selected>"),
        "{}",
        p.body
    );
    assert!(p.body.contains("Allow edits by maintainers"), "{}", p.body);
    assert!(!p.body.contains("Make these commits readable here"));
    let r = submit(
        &mut web,
        page,
        "/alice/demo/merge-requests",
        &[
            ("source_repo", "bob/demo"),
            ("source_branch", "topic"),
            ("target_branch", "main"),
            ("title", "Add a"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let mr = r.headers["location"].clone();
    let m = web.get(&mr);
    assert!(m.body.contains("bob/demo:topic"), "{}", m.body);
    assert!(m.body.contains("Allow edits by maintainers"), "{}", m.body);
    let n = mr.rsplit('/').next().unwrap();
    assert_eq!(
        api(
            &s,
            &bob,
            "GET",
            &format!("/alice/demo/merge-requests/{n}"),
            None
        )
        .json()["allow_maintainer_edits"],
        false,
        "the box was left unchecked"
    );
    let r = submit(
        &mut web,
        &mr,
        &format!("{mr}/maintainer-edits"),
        &[("allow", "true")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(
        web.get(&mr)
            .body
            .contains("Stop allowing edits by maintainers")
    );

    // A private fork needs the confirmation.
    let h = d.path().join("hidden");
    git.ok(
        d.path(),
        &["clone", "-q", &http_url(&s, &bob, "bob/hidden"), "hidden"],
    );
    git.ok(&h, &["config", "user.name", "Bob"]);
    git.ok(&h, &["config", "user.email", "bob@example.org"]);
    git.ok(&h, &["switch", "-q", "-c", "secret-topic"]);
    commit(&git, &h, "b.txt");
    git.ok(&h, &["push", "-q", "origin", "secret-topic"]);
    let page = "/alice/demo/compare?source_repo=bob%2Fhidden&source=secret-topic";
    let p = web.get(page);
    assert!(
        p.body.contains("Make these commits readable here"),
        "{}",
        p.body
    );
    let fields = [
        ("source_repo", "bob/hidden"),
        ("source_branch", "secret-topic"),
        ("target_branch", "main"),
        ("title", "Add b"),
        ("allow_maintainer_edits", "1"),
    ];
    let r = submit(&mut web, page, "/alice/demo/merge-requests", &fields);
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("publish_source-error"), "{}", r.body);
    let mut confirmed = fields.to_vec();
    confirmed.push(("publish_source", "1"));
    let r = submit(&mut web, page, "/alice/demo/merge-requests", &confirmed);
    assert_eq!(r.status, 303, "{}", r.body);
    let m = web.get(&r.headers["location"]);
    assert!(m.body.contains("become readable here"), "{}", m.body);
}
