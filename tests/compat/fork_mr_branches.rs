// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fork merge requests as branches (JJ-12, D-44): the target shows each
//! open fork merge request as `mr/<n>`, only to those involved; nobody
//! pushes branches under `mr/`; closing removes it.

use serde_json::json;
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::fork_merge_requests::{api, commit, eventually, fork, http_url, token};

/// The `mr/` branches `token`'s user sees in the target.
fn ls_mr(git: &GitClient, s: &TestServer, token: &str) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    git.ok(
        dir.path(),
        &["ls-remote", "--heads", &http_url(s, token, "alice/demo")],
    )
    .lines()
    .filter_map(|l| l.split('\t').nth(1))
    .filter(|r| r.starts_with("refs/heads/mr/"))
    .map(str::to_owned)
    .collect()
}

#[test]
fn conf_jj_12_fork_merge_requests_show_as_branches_to_those_involved() {
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
    // Private, so every fetch authenticates (over HTTPS, Git sends
    // credentials only when asked).
    for (method, path, body) in [
        (
            "PUT",
            "/alice/demo/collaborators/bob",
            json!({"role": "read"}),
        ),
        (
            "PUT",
            "/alice/demo/collaborators/carol",
            json!({"role": "write"}),
        ),
        (
            "PUT",
            "/alice/demo/collaborators/dave",
            json!({"role": "read"}),
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
    git.ok(&b, &["switch", "-q", "-c", "topic"]);
    let head = commit(&git, &b, "a.txt");
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
    let branch = format!("refs/heads/mr/{n}");
    let target = s.repo_path(&DEMO);
    eventually(&s, "no mr branch", || {
        git.cmd(&target)
            .args(["rev-parse", "--verify", "-q", &branch])
            .output()
            .unwrap()
            .stdout
            .starts_with(head.as_bytes())
    });

    // Hidden from everyone not involved, and from branch lists.
    for t in [&alice, &bob, &carol, &dave] {
        assert!(ls_mr(&git, &s, t).is_empty());
    }
    let list = api(&s, &alice, "GET", "/alice/demo/branches", None).body;
    assert!(!list.contains("mr/"), "{list}");
    // Requested reviewers and assignees see it.
    let r = api(
        &s,
        &alice,
        "PUT",
        &format!("/alice/demo/merge-requests/{n}/reviewers"),
        Some(json!({"reviewers": ["carol"]})),
    );
    assert!(r.status < 300, "{}", r.body);
    assert_eq!(ls_mr(&git, &s, &carol), vec![branch.clone()]);
    assert!(ls_mr(&git, &s, &dave).is_empty());

    // Nobody pushes branches under mr/, not even the owner.
    let out = git
        .cmd(&w)
        .args(["push", &s.url(&DEMO), "main:refs/heads/mr/99"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("branches under mr/"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The branch follows the fork's pushes and goes away on closing.
    let next = commit(&git, &b, "b.txt");
    git.ok(&b, &["push", "-q", "origin", "topic"]);
    eventually(&s, "mr branch didn't follow", || {
        git.ok(&target, &["rev-parse", &branch]).trim() == next
    });
    let r = api(
        &s,
        &bob,
        "PATCH",
        &format!("/alice/demo/merge-requests/{n}"),
        Some(json!({"state": "closed"})),
    );
    assert!(r.status < 300, "{}", r.body);
    eventually(&s, "mr branch stayed after closing", || {
        !git.cmd(&target)
            .args(["rev-parse", "--verify", "-q", &branch])
            .status()
            .unwrap()
            .success()
    });
    assert!(ls_mr(&git, &s, &carol).is_empty());
}
