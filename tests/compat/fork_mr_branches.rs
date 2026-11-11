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

/// The authenticated URL of a repository, with a token.
fn auth_url(s: &TestServer, token: &str, repo: &str) -> String {
    format!(
        "http://git:{token}@{}/-/auth/{repo}.git",
        s.base().trim_start_matches("http://")
    )
}

#[test]
fn conf_jj_12_maintainer_edits_through_mr_branches_with_git_and_jj() {
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
    git.ok(&b, &["switch", "-q", "-c", "topic"]);
    commit(&git, &b, "a.txt");
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
    eventually(&s, "no first revision", || {
        api(&s, &bob, "GET", &path, None).json()["revisions"] == 1
    });
    let fork_dir = s.repo_path(&slath_tests::server::TestRepo {
        namespace: "bob",
        name: "demo",
    });
    let fork_head = || git.ok(&fork_dir, &["rev-parse", "topic"]).trim().to_owned();
    let mr_branch = format!("mr/{n}");

    // Carol chooses "Check out": the API gives the branch and the URL that
    // authenticates, and her fetches of this public repository show it.
    let r = api(&s, &carol, "POST", &format!("{path}/checkout"), None);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["branch"], mr_branch.as_str());
    assert!(
        r.json()["fetch_url"]
            .as_str()
            .unwrap()
            .ends_with("/-/auth/alice/demo.git")
    );
    // Anonymous fetches never see it, and the authenticated URL asks.
    let anon = git.ok(
        d.path(),
        &[
            "ls-remote",
            &s.url(&DEMO).replace(&format!("git:{}@", s.token), ""),
        ],
    );
    assert!(!anon.contains("refs/heads/mr/"), "{anon}");

    // With Git, without a remote for the fork: fetch, commit, push.
    let c = d.path().join("carol");
    git.ok(d.path(), &["init", "-q", c.to_str().unwrap()]);
    let carol_url = auth_url(&s, &carol, "alice/demo");
    git.ok(
        &c,
        &[
            "fetch",
            "-q",
            &carol_url,
            &format!("{mr_branch}:{mr_branch}"),
        ],
    );
    git.ok(&c, &["switch", "-q", &mr_branch]);
    let edit = commit(&git, &c, "carol.txt");
    let out = git
        .cmd(&c)
        .args(["push", &carol_url, &mr_branch])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}\n{}", s.log());
    assert!(
        err.contains(&format!("{mr_branch}: updated bob/demo:topic")),
        "{err}"
    );
    assert_eq!(fork_head(), edit, "the fork's branch moved");
    eventually(&s, "no revision from the edit", || {
        api(&s, &bob, "GET", &path, None).json()["head_oid"] == edit.as_str()
    });

    // Someone who can't edit is refused; so is a push behind the fork.
    let dd = d.path().join("dave");
    git.ok(d.path(), &["init", "-q", dd.to_str().unwrap()]);
    git.ok(
        &dd,
        &[
            "fetch",
            "-q",
            &carol_url,
            &format!("{mr_branch}:{mr_branch}"),
        ],
    );
    git.ok(&dd, &["switch", "-q", &mr_branch]);
    commit(&git, &dd, "dave.txt");
    let out = git
        .cmd(&dd)
        .args(["push", &auth_url(&s, &dave, "alice/demo"), &mr_branch])
        .output()
        .unwrap();
    assert!(!out.status.success(), "dave pushed to {mr_branch}");
    git.ok(&b, &["pull", "-q", "origin", "topic"]);
    let bob_next = commit(&git, &b, "b.txt");
    git.ok(&b, &["push", "-q", "origin", "topic"]);
    commit(&git, &c, "carol2.txt");
    let out = git
        .cmd(&c)
        .args(["push", &carol_url, &mr_branch])
        .output()
        .unwrap();
    assert!(!out.status.success(), "a push behind the fork went through");
    assert_eq!(fork_head(), bob_next);

    // With jj, the same way.
    if let Some((version, bin)) = crate::jj::jj_binaries().into_iter().last() {
        eventually(&s, "mr branch didn't follow bob", || {
            git.ok(
                &s.repo_path(&DEMO),
                &["rev-parse", &format!("refs/heads/{mr_branch}")],
            )
            .trim()
                == bob_next
        });
        let jj = crate::jj::Jj::new(&version, &bin);
        let j = d.path().join("jj");
        jj.ok(d.path(), &["git", "clone", &carol_url, j.to_str().unwrap()]);
        jj.ok(&j, &["bookmark", "track", &format!("{mr_branch}@origin")]);
        jj.ok(&j, &["new", &mr_branch, "-m", "Edit with jj"]);
        std::fs::write(j.join("jj.txt"), "jj\n").unwrap();
        jj.ok(&j, &["bookmark", "set", &mr_branch, "-r", "@"]);
        jj.ok(&j, &["git", "push", "--bookmark", &mr_branch]);
        let pushed = jj.show(&j, &mr_branch, "commit_id");
        assert_eq!(fork_head(), pushed, "{version}");
    }

    // Without edits by maintainers, carol's pushes are refused.
    let r = api(
        &s,
        &bob,
        "PATCH",
        &path,
        Some(json!({"allow_maintainer_edits": false})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    // (Carol's branch is behind the fork by now; force it, still refused.)
    let head_before = fork_head();
    commit(&git, &c, "carol3.txt");
    let out = git
        .cmd(&c)
        .args(["push", "-f", &carol_url, &mr_branch])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(fork_head(), head_before);
    let audit: i64 = s
        .db
        .query_one("SELECT count(*)::text FROM audit_events WHERE action = 'push.maintainer_edit'")
        .parse()
        .unwrap();
    assert!(audit >= 2, "{audit}");
}
