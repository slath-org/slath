// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Update branch (REVIEW-09): the server merges the target into a merge
//! request's branch or rebases the branch onto it, and the new head becomes
//! a revision. Rebased commits keep their change IDs for Git and jj alike;
//! the source's branch protection applies; on a fork, target writers may
//! do it only while edits by maintainers are allowed.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::fork_merge_requests::{api, commit, eventually, fork, http_url, token};
use crate::jj::{Jj, jj_binaries};

fn mr(s: &TestServer, t: &str, n: i64) -> Value {
    api(
        s,
        t,
        "GET",
        &format!("/alice/demo/merge-requests/{n}"),
        None,
    )
    .json()
}

fn update(s: &TestServer, t: &str, n: i64, body: Value) -> slath_tests::http::Resp {
    api(
        s,
        t,
        "POST",
        &format!("/alice/demo/merge-requests/{n}/update-branch"),
        Some(body),
    )
}

fn up_to_date(s: &TestServer, t: &str, n: i64) -> bool {
    api(
        s,
        t,
        "GET",
        &format!("/alice/demo/merge-requests/{n}/mergeability"),
        None,
    )
    .json()["up_to_date"]
        .as_bool()
        .unwrap()
}

/// Opens a merge request for `branch` and waits for its first revision.
fn open(s: &TestServer, t: &str, branch: &str) -> (i64, String) {
    let r = api(
        s,
        t,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_branch": branch, "title": format!("Merge {branch}")})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    eventually(s, "no first revision", || mr(s, t, n)["revisions"] == 1);
    (n, mr(s, t, n)["head_oid"].as_str().unwrap().to_owned())
}

fn server(s: &TestServer, git: &GitClient, args: &[&str]) -> String {
    git.ok(&s.repo_path(&DEMO), args).trim().to_owned()
}

/// Moves `main` on the server with a new commit made in `w`.
fn advance_main(git: &GitClient, w: &Path, url: &str, file: &str) {
    git.ok(w, &["switch", "-q", "main"]);
    git.ok(w, &["pull", "-q", "--rebase", url, "main"]);
    commit(git, w, file);
    git.ok(w, &["push", "-q", url, "main"]);
}

#[test]
fn conf_review_09_update_branch_merges_or_rebases_with_git_and_jj() {
    let s = TestServer::start();
    let alice = token(&s, "alice");
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    let url = s.url(&DEMO);
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    commit(&git, &w, "README");
    git.ok(&w, &["push", "-q", &url, "main"]);
    for b in ["merged", "rebased", "kept"] {
        git.ok(&w, &["switch", "-q", "-c", b, "main"]);
        commit(&git, &w, &format!("{b}-1.txt"));
        commit(&git, &w, &format!("{b}-2.txt"));
        git.ok(&w, &["push", "-q", &url, b]);
    }
    let (merged, head) = open(&s, &alice, "merged");
    let (rebased, rebased_head) = open(&s, &alice, "rebased");
    let (kept, kept_head) = open(&s, &alice, "kept");
    assert!(up_to_date(&s, &alice, merged));
    let r = update(&s, &alice, merged, json!({"method": "merge"}));
    assert_eq!(r.status, 409, "already up to date: {}", r.body);
    advance_main(&git, &w, &url, "m1.txt");
    let main = server(&s, &git, &["rev-parse", "main"]);
    assert!(!up_to_date(&s, &alice, merged));

    // Bad method, stale head.
    assert_eq!(
        update(&s, &alice, merged, json!({"method": "squash"})).status,
        422
    );
    let r = update(
        &s,
        &alice,
        merged,
        json!({"method": "merge", "expected_head_oid": "0".repeat(40)}),
    );
    assert_eq!(r.status, 409, "{}", r.body);

    // Merge: the target merged into the branch, as a new revision.
    let r = update(
        &s,
        &alice,
        merged,
        json!({"method": "merge", "expected_head_oid": head}),
    );
    assert_eq!(r.status, 200, "{}\n{}", r.body, s.log());
    let m = r.json();
    assert_eq!(m["revisions"], 2, "{m}");
    let new = server(&s, &git, &["rev-parse", "merged"]);
    assert_eq!(m["head_oid"], new.as_str());
    assert_eq!(
        server(&s, &git, &["rev-parse", "merged^1", "merged^2"]),
        format!("{head}\n{main}")
    );
    assert!(up_to_date(&s, &alice, merged));
    let page = s
        .login("alice", PASSWORD)
        .get(&format!("/alice/demo/merge-requests/{merged}"))
        .body;
    assert!(
        page.contains("updated the branch by merging main into it"),
        "{page}"
    );

    // Rebase: the same change IDs on new commits on top of main.
    let ids: Vec<String> = api(
        &s,
        &alice,
        "GET",
        &format!("/alice/demo/merge-requests/{rebased}/stack"),
        None,
    )
    .json()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["change_id"].as_str().unwrap().to_owned())
        .collect();
    let r = update(
        &s,
        &alice,
        rebased,
        json!({"method": "rebase", "expected_head_oid": rebased_head}),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(server(&s, &git, &["rev-parse", "rebased~2"]), main);
    let header = |rev: &str| {
        server(&s, &git, &["cat-file", "commit", rev])
            .lines()
            .find_map(|l| l.strip_prefix("change-id ").map(str::to_owned))
    };
    assert_eq!(header("rebased~1").as_deref(), Some(ids[0].as_str()));
    assert_eq!(header("rebased").as_deref(), Some(ids[1].as_str()));
    let stack: Vec<String> = api(
        &s,
        &alice,
        "GET",
        &format!("/alice/demo/merge-requests/{rebased}/stack"),
        None,
    )
    .json()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["change_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(stack, ids, "the changes survive the rebase");

    // Branch protection on the source: no rebase where force pushes are
    // blocked, but merging is a fast-forward.
    let r = api(
        &s,
        &alice,
        "POST",
        "/alice/demo/protections",
        Some(
            json!({"pattern": "kept", "block_force_push": true, "block_deletion": true, "push_role": "write"}),
        ),
    );
    assert!(r.status < 300, "{}", r.body);
    let r = update(&s, &alice, kept, json!({"method": "rebase"}));
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("force pushes"), "{}", r.body);
    assert_eq!(server(&s, &git, &["rev-parse", "kept"]), kept_head);
    // In the UI: the merge box offers the update to those who may push.
    let page = format!("/alice/demo/merge-requests/{kept}");
    s.create_user("bob", PASSWORD, false);
    let r = api(
        &s,
        &alice,
        "PATCH",
        "/alice/demo",
        Some(json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);
    assert!(
        !s.login("bob", PASSWORD)
            .get(&page)
            .body
            .contains("id=\"update-branch\"")
    );
    let mut web = s.login("alice", PASSWORD);
    let body = web.get(&page).body;
    assert!(body.contains("id=\"update-branch\""), "{body}");
    let r = slath_tests::http::submit(
        &mut web,
        &page,
        &format!("{page}/update-branch"),
        &[("method", "merge"), ("expected_head_oid", &kept_head)],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let body = web.get(&format!("{page}?status=updated")).body;
    assert!(body.contains("Branch updated."), "{body}");
    assert!(!body.contains("id=\"update-branch\""), "{body}");
    assert_eq!(server(&s, &git, &["rev-parse", "kept^1"]), kept_head);

    // jj: after a server-side rebase, a fetch shows the same change.
    for (version, bin) in jj_binaries() {
        let jj = Jj::new(&version, &bin);
        let v = version.replace(['.', ' '], "-");
        let j = d.path().join(format!("jj-{v}"));
        jj.ok(d.path(), &["git", "clone", &url, j.to_str().unwrap()]);
        let topic = format!("jj-topic-{v}");
        jj.ok(&j, &["new", "main@origin", "-m", "Add j"]);
        std::fs::write(j.join(format!("{topic}.txt")), "j\n").unwrap();
        jj.ok(&j, &["bookmark", "create", &topic, "-r", "@"]);
        jj.ok(&j, &["git", "push", "--bookmark", &topic]);
        let change = jj.show(&j, &topic, "change_id");
        let (n, head) = open(&s, &alice, &topic);
        advance_main(&git, &w, &url, &format!("{topic}-main.txt"));
        let r = update(
            &s,
            &alice,
            n,
            json!({"method": "rebase", "expected_head_oid": head}),
        );
        assert_eq!(r.status, 200, "{version}: {}", r.body);
        jj.ok(&j, &["git", "fetch"]);
        let rev = format!("{topic}@origin");
        assert_eq!(jj.show(&j, &rev, "change_id"), change, "{version}");
        assert_eq!(
            jj.show(&j, &format!("{rev}-"), "commit_id"),
            server(&s, &git, &["rev-parse", "main"]),
            "{version}"
        );
    }
    git.ok(&s.repo_path(&DEMO), &["fsck", "--strict", "--no-dangling"]);
}

#[test]
fn conf_review_06_target_writers_update_a_fork_branch_only_with_maintainer_edits() {
    let s = TestServer::start();
    for u in ["bob", "carol", "dave"] {
        s.create_user(u, PASSWORD, false);
    }
    let alice = token(&s, "alice");
    let (bob, carol, dave) = (token(&s, "bob"), token(&s, "carol"), token(&s, "dave"));
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("alice");
    let url = s.url(&DEMO);
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    commit(&git, &w, "README");
    git.ok(&w, &["push", "-q", &url, "main"]);
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
    eventually(&s, "no first revision", || {
        mr(&s, &bob, n)["revisions"] == 1
    });
    advance_main(&git, &w, &url, "m1.txt");
    let fork_dir = s.repo_path(&slath_tests::server::TestRepo {
        namespace: "bob",
        name: "demo",
    });

    // Dave can read both repositories but push to neither.
    let r = update(&s, &dave, n, json!({"method": "merge"}));
    assert_eq!(r.status, 403, "{}", r.body);
    // Carol writes to the target: with maintainer edits she may.
    let r = update(
        &s,
        &carol,
        n,
        json!({"method": "merge", "expected_head_oid": head}),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let new = git.ok(&fork_dir, &["rev-parse", "topic"]).trim().to_owned();
    assert_eq!(r.json()["head_oid"], new.as_str());
    assert_eq!(
        git.ok(&fork_dir, &["rev-parse", "topic^1"]).trim(),
        head,
        "the fork's branch moved on from its head"
    );
    assert_eq!(mr(&s, &bob, n)["revisions"], 2);
    // No temporary refs stay behind in the fork.
    assert_eq!(
        git.ok(&fork_dir, &["for-each-ref", "refs/slath/sync/"])
            .trim(),
        ""
    );

    // Without maintainer edits, only Bob can.
    advance_main(&git, &w, &url, "m2.txt");
    let r = api(
        &s,
        &bob,
        "PATCH",
        &format!("/alice/demo/merge-requests/{n}"),
        Some(json!({"allow_maintainer_edits": false})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        update(&s, &carol, n, json!({"method": "merge"})).status,
        403
    );
    let r = update(&s, &bob, n, json!({"method": "merge"}));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(mr(&s, &bob, n)["revisions"], 3);
    git.ok(&fork_dir, &["fsck", "--strict", "--no-dangling"]);
}
