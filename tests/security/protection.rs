// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Branch protection (GIT-05): force pushes (whose objects are still in
//! quarantine) and deletions are refused, `push_role` limits direct pushes
//! (`nobody` refuses all), merge requests can be required with an audited
//! admin bypass, and a refused push changes no ref and no database state.

use std::path::{Path, PathBuf};

use serde_json::json;
use slath_tests::http::{create_token, submit};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

struct Fixture {
    s: TestServer,
    git: GitClient,
    _work: tempfile::TempDir,
    src: PathBuf,
}

fn commit(git: &GitClient, src: &Path, file: &str, text: &str) {
    std::fs::write(src.join(file), text).unwrap();
    git.ok(src, &["add", "."]);
    git.ok(src, &["commit", "-q", "-m", text]);
}

/// alice/demo with `main` (two commits) and `dev` pushed.
fn fixture() -> Fixture {
    let s = TestServer::start();
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git.ok(&src, &["init", "-q", "-b", "main"]);
    commit(&git, &src, "a.txt", "one");
    commit(&git, &src, "a.txt", "two");
    git.ok(&src, &["push", "-q", &s.url(&DEMO), "main", "main:dev"]);
    Fixture {
        s,
        git,
        _work: work,
        src,
    }
}

fn protect(s: &TestServer, rule: serde_json::Value) {
    let r = s.client().api(
        "POST",
        "/api/v1/repos/alice/demo/protections",
        Some(&s.token),
        Some(&rule),
    );
    assert_eq!(r.status, 200, "{}", r.body);
}

fn remote_ref(f: &Fixture, name: &str) -> String {
    f.git
        .ok(&f.src, &["ls-remote", &f.s.url(&DEMO), name])
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn db_ref(f: &Fixture, name: &str) -> String {
    f.s.db.query_one(&format!(
        "SELECT coalesce((SELECT oid FROM git_refs WHERE name = '{name}'::bytea), 'none')"
    ))
}

#[test]
fn conf_git_05_force_push_with_quarantined_objects_is_refused() {
    let f = fixture();
    protect(&f.s, json!({"pattern": "main"}));
    let before = remote_ref(&f, "refs/heads/main");
    // A fast-forward (new objects only in quarantine) is fine.
    commit(&f.git, &f.src, "a.txt", "three");
    f.git.ok(&f.src, &["push", "-q", &f.s.url(&DEMO), "main"]);
    let ff = remote_ref(&f, "refs/heads/main");
    assert_ne!(ff, before);
    // A rewritten history: objects the server has never seen.
    f.git.ok(&f.src, &["reset", "-q", "--hard", "HEAD~2"]);
    commit(&f.git, &f.src, "b.txt", "rewritten");
    let pushes = f.s.db.query_one("SELECT count(*)::text FROM push_events");
    let err = f
        .git
        .fails(&f.src, &["push", "-q", "--force", &f.s.url(&DEMO), "main"]);
    assert!(
        err.contains("force pushes to this protected branch are not allowed"),
        "{err}"
    );
    assert_eq!(
        remote_ref(&f, "refs/heads/main"),
        ff,
        "the ref is unchanged"
    );
    assert_eq!(
        db_ref(&f, "refs/heads/main"),
        ff,
        "the snapshot is unchanged"
    );
    assert_eq!(
        f.s.db.query_one("SELECT count(*)::text FROM push_events"),
        pushes,
        "no push recorded"
    );
    // Unprotected branches can still be rewritten.
    f.git.ok(
        &f.src,
        &["push", "-q", "--force", &f.s.url(&DEMO), "main:dev"],
    );
}

#[test]
fn conf_git_05_protected_branches_cant_be_deleted() {
    let f = fixture();
    protect(&f.s, json!({"pattern": "d*", "block_force_push": false}));
    let err = f
        .git
        .fails(&f.src, &["push", "-q", &f.s.url(&DEMO), ":dev"]);
    assert!(
        err.contains("deleting this protected branch is not allowed"),
        "{err}"
    );
    assert!(!remote_ref(&f, "refs/heads/dev").is_empty());
    // A rule that allows deletion (and force pushes) doesn't stop it.
    protect(
        &f.s,
        json!({"pattern": "d*", "block_force_push": false, "block_deletion": false}),
    );
    f.git.ok(&f.src, &["push", "-q", &f.s.url(&DEMO), ":dev"]);
}

#[test]
fn conf_git_05_push_role_nobody_blocks_every_direct_push() {
    let f = fixture();
    protect(&f.s, json!({"pattern": "main", "push_role": "nobody"}));
    commit(&f.git, &f.src, "a.txt", "three");
    // Even the owner (admin) can't push directly.
    let err = f
        .git
        .fails(&f.src, &["push", "-q", &f.s.url(&DEMO), "main"]);
    assert!(
        err.contains("can only be changed through merge requests"),
        "{err}"
    );
    // New branches matching no rule are fine.
    f.git
        .ok(&f.src, &["push", "-q", &f.s.url(&DEMO), "main:feature"]);
}

#[test]
fn conf_git_05_push_role_and_required_merge_requests_with_audited_admin_bypass() {
    let f = fixture();
    f.s.create_user("bob", PASSWORD, false);
    let mut alice = f.s.login("alice", PASSWORD);
    let r = submit(
        &mut alice,
        "/alice/demo/settings/access",
        "/alice/demo/settings/access",
        &[("username", "bob"), ("role", "write")],
    );
    assert_eq!(r.status, 303);
    let mut bob = f.s.login("bob", PASSWORD);
    let bob_token = create_token(&mut bob, "push", &["repo:write"]);
    let bob_url = format!("http://bob:{bob_token}@{}/alice/demo.git", f.s.addr);

    protect(&f.s, json!({"pattern": "main", "push_role": "maintain"}));
    commit(&f.git, &f.src, "a.txt", "by bob");
    let err = f.git.fails(&f.src, &["push", "-q", &bob_url, "main"]);
    assert!(err.contains("needs a higher role"), "{err}");

    protect(&f.s, json!({"pattern": "main", "require_mr": true}));
    let err = f.git.fails(&f.src, &["push", "-q", &bob_url, "main"]);
    assert!(err.contains("requires merge requests"), "{err}");
    // The owner may bypass; it's recorded.
    let out = f
        .git
        .cmd(&f.src)
        .args(["push", &f.s.url(&DEMO), "main"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("admin bypass"));
    assert_eq!(
        f.s.db
            .query_one("SELECT data->>'refs' FROM audit_events WHERE action = 'protection.bypass'"),
        r#"["refs/heads/main"]"#
    );
    // Tags aren't affected by branch rules.
    f.git.ok(&f.src, &["tag", "v1"]);
    f.git.ok(&f.src, &["push", "-q", &bob_url, "v1"]);
}

#[test]
fn protection_rules_are_managed_by_admins_only() {
    let f = fixture();
    f.s.create_user("bob", PASSWORD, false);
    let mut alice = f.s.login("alice", PASSWORD);
    let r = submit(
        &mut alice,
        "/alice/demo/settings/protections",
        "/alice/demo/settings/protections",
        &[
            ("pattern", "release/*"),
            ("push_role", "admin"),
            ("block_force_push", "1"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let page = alice.get("/alice/demo/settings/protections");
    assert!(page.body.contains("release/*"));
    let r = submit(
        &mut alice,
        "/alice/demo/settings/protections",
        "/alice/demo/settings/protections",
        &[("pattern", "../x"), ("push_role", "write")],
    );
    assert_eq!(r.status, 422);
    // Others can't see or change the rules.
    let mut bob = f.s.login("bob", PASSWORD);
    assert_eq!(bob.get("/alice/demo/settings/protections").status, 404);
    let bob_token = create_token(&mut bob, "t", &["repo:write"]);
    let r = f.s.client().api(
        "GET",
        "/api/v1/repos/alice/demo/protections",
        Some(&bob_token),
        None,
    );
    assert_eq!(r.status, 404);
    let rules = f.s.client().api(
        "GET",
        "/api/v1/repos/alice/demo/protections",
        Some(&f.s.token),
        None,
    );
    let id = rules.json()[0]["id"].as_str().unwrap().to_owned();
    let r = f.s.client().api(
        "DELETE",
        &format!("/api/v1/repos/alice/demo/protections/{id}"),
        Some(&f.s.token),
        None,
    );
    assert_eq!(r.status, 204);
    assert_eq!(
        f.s.db
            .query_one("SELECT count(*)::text FROM branch_protections"),
        "0"
    );
    assert_eq!(
        f.s.db
            .query_one("SELECT count(*)::text FROM audit_events WHERE action LIKE 'protection.%'"),
        "2"
    );
}
