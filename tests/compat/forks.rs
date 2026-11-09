// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Forks (GIT-10): only branches and tags are copied, with the parent's
//! packs hardlinked; the fork stands alone when its parent is
//! garbage-collected or deleted; objects only the parent's deleted refs
//! reached (loose or in a cruft pack) can't be fetched from the fork; and a
//! fork never becomes more visible than its parent.

use std::os::unix::fs::MetadataExt;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestRepo, TestServer};

const FORK: TestRepo = TestRepo {
    namespace: "bob",
    name: "demo",
};

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

fn refs(git: &GitClient, dir: &std::path::Path) -> Vec<String> {
    git.ok(dir, &["for-each-ref", "--format=%(refname)"])
        .lines()
        .map(str::to_owned)
        .collect()
}

fn wait_done(s: &TestServer, token: &str, path: &str) {
    let start = std::time::Instant::now();
    loop {
        let f = api(s, token, "GET", path, None).json();
        if f["state"] == "done" {
            return;
        }
        assert_ne!(f["state"], "failed", "{f}");
        assert!(
            start.elapsed().as_secs() < 30,
            "fork not done: {f}\n{}",
            s.log()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Whether some object file of the repository is shared with another.
fn has_hardlinks(dir: &std::path::Path) -> bool {
    fn walk(d: &std::path::Path) -> bool {
        std::fs::read_dir(d).unwrap().flatten().any(|e| {
            let m = e.metadata().unwrap();
            if m.is_dir() {
                walk(&e.path())
            } else {
                m.nlink() > 1
            }
        })
    }
    walk(&dir.join("objects"))
}

#[test]
fn conf_git_10_forks_copy_branches_and_tags_and_stand_alone() {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let (alice, bob) = (token(&s, "alice"), token(&s, "bob"));
    let r = api(
        &s,
        &alice,
        "PUT",
        "/alice/demo/collaborators/bob",
        Some(json!({"role": "read"})),
    );
    assert!(r.status < 300, "{}", r.body);

    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let url = s.url(&DEMO);
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "x\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["tag", "-a", "v1", "-m", "v1"]);
    git.ok(w, &["push", "-q", &url, "main", "v1"]);
    git.ok(w, &["switch", "-q", "-c", "topic"]);
    std::fs::write(w.join("a.txt"), "a\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "Add a"]);
    git.ok(w, &["push", "-q", &url, "topic"]);
    // A merge request adds internal and merge request refs to the parent.
    let r = api(
        &s,
        &alice,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Add a"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    // An object only a deleted branch reached.
    git.ok(w, &["switch", "-q", "-c", "secret", "main"]);
    std::fs::write(w.join("secret.txt"), "hidden\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "Secret"]);
    let secret = git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned();
    git.ok(w, &["push", "-q", &url, "secret"]);
    git.ok(w, &["push", "-q", &url, ":secret"]);
    let parent = s.repo_path(&DEMO);
    let start = std::time::Instant::now();
    while !refs(&git, &parent)
        .iter()
        .any(|r| r.starts_with("refs/merge-requests/"))
    {
        assert!(
            start.elapsed().as_secs() < 30,
            "no merge request ref\n{}",
            s.log()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Packed, as maintenance leaves it: the deleted branch's commit goes to a
    // cruft pack, which a fork doesn't link.
    git.ok(&parent, &["gc", "-q"]);

    // A private parent's fork can't be public.
    let r = api(
        &s,
        &bob,
        "POST",
        "/alice/demo/forks",
        Some(json!({"namespace": "bob", "visibility": "public"})),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    let r = api(
        &s,
        &bob,
        "POST",
        "/alice/demo/forks",
        Some(json!({"namespace": "bob"})),
    );
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.json()["parent"], "alice/demo");
    wait_done(&s, &bob, "/bob/demo/fork");
    let fork = s.repo_path(&FORK);
    assert_eq!(
        refs(&git, &fork),
        ["refs/heads/main", "refs/heads/topic", "refs/tags/v1"]
    );
    assert!(has_hardlinks(&fork), "the fork's objects are hardlinked");
    let r = api(
        &s,
        &bob,
        "PATCH",
        "/bob/demo",
        Some(json!({"visibility": "public"})),
    );
    assert_eq!(r.status, 422, "{}", r.body);

    // A taken name gets a suggestion; the default branch can come alone.
    let r = api(
        &s,
        &bob,
        "POST",
        "/alice/demo/forks",
        Some(json!({"namespace": "bob"})),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("demo-1"), "{}", r.body);
    let r = api(
        &s,
        &bob,
        "POST",
        "/alice/demo/forks",
        Some(json!({"namespace": "bob", "name": "main-only", "default_branch_only": true})),
    );
    assert_eq!(r.status, 202, "{}", r.body);
    wait_done(&s, &bob, "/bob/main-only/fork");
    let only = s.repo_path(&TestRepo {
        namespace: "bob",
        name: "main-only",
    });
    assert_eq!(refs(&git, &only), ["refs/heads/main", "refs/tags/v1"]);

    // What only the parent could reach isn't fetchable from the fork.
    let base = s.base();
    let fork_url = format!(
        "http://git:{bob}@{}/bob/demo.git",
        base.trim_start_matches("http://")
    );
    let c = tempfile::tempdir().unwrap();
    git.ok(c.path(), &["init", "-q"]);
    git.fails(c.path(), &["fetch", "-q", &fork_url, &secret]);
    git.ok(c.path(), &["fetch", "-q", &fork_url, "main"]);

    // Sync: a fast-forward, then a diverged branch that changes only when
    // the fork's commits are discarded.
    let head = |dir: &std::path::Path| {
        git.ok(dir, &["rev-parse", "refs/heads/main"])
            .trim()
            .to_owned()
    };
    git.ok(w, &["switch", "-q", "main"]);
    std::fs::write(w.join("more.txt"), "1\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "More"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    let f = api(&s, &bob, "GET", "/bob/demo/fork", None).json();
    assert_eq!(
        (f["ahead"].as_u64(), f["behind"].as_u64()),
        (Some(0), Some(1)),
        "{f}"
    );
    let sync = |discard: bool| {
        api(
            &s,
            &bob,
            "POST",
            "/bob/demo/sync",
            Some(json!({"branch": "main", "discard": discard})),
        )
    };
    let r = sync(false);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["result"], "fast_forwarded");
    assert_eq!(head(&fork), head(&parent));
    assert_eq!(sync(false).json()["result"], "up_to_date");
    let b = tempfile::tempdir().unwrap();
    git.ok(b.path(), &["clone", "-q", &fork_url, "."]);
    git.ok(b.path(), &["config", "user.name", "Bob"]);
    git.ok(b.path(), &["config", "user.email", "bob@example.org"]);
    std::fs::write(b.path().join("bob.txt"), "b\n").unwrap();
    git.ok(b.path(), &["add", "."]);
    git.ok(b.path(), &["commit", "-q", "-m", "Bob's"]);
    git.ok(b.path(), &["push", "-q", "origin", "main"]);
    std::fs::write(w.join("more.txt"), "2\n").unwrap();
    git.ok(w, &["commit", "-q", "-am", "Even more"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    let before = head(&fork);
    let r = sync(false).json();
    assert_eq!(r["result"], "diverged", "{r}");
    assert_eq!(
        (r["ahead"].as_u64(), r["behind"].as_u64()),
        (Some(1), Some(1))
    );
    assert_eq!(head(&fork), before, "a diverged branch stays as it is");
    let r = sync(true);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["result"], "discarded");
    assert_eq!(head(&fork), head(&parent));
    assert!(
        !git.ok(&fork, &["for-each-ref", "refs/slath/sync"])
            .contains("refs/slath"),
        "no temporary refs stay behind"
    );

    // The fork stands alone after the parent's gc and deletion.
    git.ok(&parent, &["gc", "-q", "--prune=now"]);
    let r = api(&s, &alice, "DELETE", "/alice/demo", None);
    assert!(r.status < 300, "{}", r.body);
    git.ok(&fork, &["fsck", "--strict", "--no-progress"]);
    let f = api(&s, &bob, "GET", "/bob/demo/fork", None).json();
    assert_eq!(f["parent_deleted"], true, "{f}");
    let clone = tempfile::tempdir().unwrap();
    git.ok(clone.path(), &["clone", "-q", &fork_url, "."]);
    assert!(clone.path().join("README").exists());
}
