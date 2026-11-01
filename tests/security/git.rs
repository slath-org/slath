// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Git over HTTPS: authentication, authorization, and the push policy.

use std::path::Path;

use slath_tests::http::{create_repo, create_token, submit};
use slath_tests::server::{GitClient, PASSWORD, TestServer};

fn url(s: &TestServer, token: Option<&str>, ns: &str, repo: &str) -> String {
    match token {
        Some(t) => format!("http://git:{t}@{}/{ns}/{repo}.git", s.addr),
        None => format!("http://{}/{ns}/{repo}.git", s.addr),
    }
}

/// A work tree with one commit on `main`.
fn work(git: &GitClient, dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git.ok(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git.ok(dir, &["add", "a.txt"]);
    git.ok(dir, &["commit", "-q", "-m", "first"]);
}

/// Server with alice (owner of alice/proj, private, and alice/open, public)
/// and bob (no access); returns alice's write token.
fn setup() -> (TestServer, String) {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PASSWORD, false);
    s.create_user("bob", PASSWORD, false);
    let mut a = s.login("alice", PASSWORD);
    create_repo(&mut a, "alice", "proj", "private");
    create_repo(&mut a, "alice", "open", "public");
    let t = create_token(&mut a, "rw", &["repo:write"]);
    (s, t)
}

#[test]
fn conf_sec_01_reserved_and_non_branch_refs_are_denied() {
    let (s, t) = setup();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    let u = url(&s, Some(&t), "alice", "proj");
    for target in [
        "refs/slath/x",
        "refs/merge-requests/1/head",
        "refs/notes/commits",
        "refs/foo",
        "refs/pull/1/head",
    ] {
        let err = git.fails(&w, &["push", &u, &format!("HEAD:{target}")]);
        assert!(err.contains("remote: slath:"), "{target}: {err}");
    }
    // Branches and tags are fine, and recorded.
    git.ok(
        &w,
        &[
            "push",
            "-q",
            &u,
            "HEAD:refs/heads/main",
            "HEAD:refs/tags/v1",
        ],
    );
    assert_eq!(s.db.query_one("SELECT count(*)::text FROM git_refs"), "2");
    assert_eq!(s.db.query_one("SELECT transport FROM push_events"), "http");
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM ref_updates"),
        "2"
    );
}

#[test]
fn conf_sec_01_tag_moves_need_the_maintain_role() {
    let (s, t) = setup();
    let mut a = s.login("alice", PASSWORD);
    submit(
        &mut a,
        "/alice/proj/settings/access",
        "/alice/proj/settings/access",
        &[("username", "bob"), ("role", "write")],
    );
    let mut b = s.login("bob", PASSWORD);
    let bt = create_token(&mut b, "bob", &["repo:write"]);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    git.ok(
        &w,
        &[
            "push",
            "-q",
            &url(&s, Some(&t), "alice", "proj"),
            "HEAD:refs/heads/main",
            "HEAD:refs/tags/v1",
        ],
    );
    std::fs::write(w.join("b.txt"), "b\n").unwrap();
    git.ok(&w, &["add", "b.txt"]);
    git.ok(&w, &["commit", "-q", "-m", "second"]);
    let bu = url(&s, Some(&bt), "alice", "proj");
    // A writer pushes branches and new tags ...
    git.ok(
        &w,
        &[
            "push",
            "-q",
            &bu,
            "HEAD:refs/heads/main",
            "HEAD:refs/tags/v2",
        ],
    );
    // ... but can't move or delete existing tags.
    let err = git.fails(&w, &["push", "--force", &bu, "HEAD:refs/tags/v1"]);
    assert!(err.contains("maintain role"), "{err}");
    let err = git.fails(&w, &["push", &bu, ":refs/tags/v1"]);
    assert!(err.contains("maintain role"), "{err}");
    // The whole push fails if one update is refused.
    let err = git.fails(
        &w,
        &[
            "push",
            "--force",
            &bu,
            "HEAD:refs/heads/feature",
            "HEAD:refs/tags/v1",
        ],
    );
    assert!(err.contains("push rejected"), "{err}");
    assert!(
        !git.ok(&w, &["ls-remote", &bu])
            .contains("refs/heads/feature")
    );
    // The owner (admin) may move tags.
    git.ok(
        &w,
        &[
            "push",
            "-q",
            "--force",
            &url(&s, Some(&t), "alice", "proj"),
            "HEAD:refs/tags/v1",
        ],
    );
}

#[test]
fn conf_git_03_anonymous_access_follows_visibility() {
    let (s, t) = setup();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    git.ok(
        &w,
        &["push", "-q", &url(&s, Some(&t), "alice", "open"), "main"],
    );
    git.ok(
        &w,
        &["push", "-q", &url(&s, Some(&t), "alice", "proj"), "main"],
    );
    // Public: anonymous clone works, anonymous push is asked for credentials.
    git.ok(
        d.path(),
        &["clone", "-q", &url(&s, None, "alice", "open"), "c1"],
    );
    let err = git.fails(
        &w,
        &["push", &url(&s, None, "alice", "open"), "HEAD:refs/heads/x"],
    );
    assert!(
        err.contains("Authentication failed") || err.contains("could not read Username"),
        "{err}"
    );
    // Private: anonymous clone looks like a missing repository.
    let err = git.fails(
        d.path(),
        &["clone", "-q", &url(&s, None, "alice", "proj"), "c2"],
    );
    let err_missing = git.fails(
        d.path(),
        &["clone", "-q", &url(&s, None, "alice", "missing"), "c3"],
    );
    assert_eq!(
        err.replace("proj", "X"),
        err_missing.replace("missing", "X")
    );
}

#[test]
fn conf_auth_05_token_scopes_and_restrictions_apply_to_git() {
    let (s, t) = setup();
    let mut a = s.login("alice", PASSWORD);
    let read = create_token(&mut a, "ro", &["repo:read"]);
    let user_only = create_token(&mut a, "user", &["user:read"]);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    git.ok(
        &w,
        &["push", "-q", &url(&s, Some(&t), "alice", "proj"), "main"],
    );
    // repo:read clones but can't push.
    git.ok(
        d.path(),
        &["clone", "-q", &url(&s, Some(&read), "alice", "proj"), "c1"],
    );
    let err = git.fails(
        &w,
        &[
            "push",
            &url(&s, Some(&read), "alice", "proj"),
            "HEAD:refs/heads/x",
        ],
    );
    assert!(err.contains("403") || err.contains("permission"), "{err}");
    // A token without a repo scope can't even read.
    let err = git.fails(
        d.path(),
        &[
            "clone",
            "-q",
            &url(&s, Some(&user_only), "alice", "proj"),
            "c2",
        ],
    );
    assert!(err.contains("403") || err.contains("not found"), "{err}");
    // Bob's token doesn't reach alice's private repository: 404.
    let mut b = s.login("bob", PASSWORD);
    let bt = create_token(&mut b, "bob", &["repo:write"]);
    let err = git.fails(
        d.path(),
        &["clone", "-q", &url(&s, Some(&bt), "alice", "proj"), "c3"],
    );
    assert!(err.contains("not found"), "{err}");
    // Restricted to another repository: alice/proj is out of reach.
    let open_id =
        s.db.query_one("SELECT id::text FROM repositories WHERE name = 'open'");
    s.db.execute(&format!(
        "UPDATE access_tokens SET repo_ids = ARRAY['{open_id}'::uuid] WHERE name = 'ro'"
    ));
    let err = git.fails(
        d.path(),
        &["clone", "-q", &url(&s, Some(&read), "alice", "proj"), "c4"],
    );
    assert!(!err.is_empty());
    git.ok(
        d.path(),
        &["clone", "-q", &url(&s, Some(&read), "alice", "open"), "c5"],
    );
    // Revoked tokens stop working at once.
    s.db.execute("UPDATE access_tokens SET revoked_at = now() WHERE name = 'rw'");
    let err = git.fails(
        d.path(),
        &["clone", "-q", &url(&s, Some(&t), "alice", "proj"), "c6"],
    );
    assert!(
        err.contains("Authentication failed") || err.contains("401"),
        "{err}"
    );
}

#[test]
fn conf_git_04_archived_repositories_refuse_pushes() {
    let (s, t) = setup();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    let u = url(&s, Some(&t), "alice", "proj");
    git.ok(&w, &["push", "-q", &u, "main"]);
    let mut a = s.login("alice", PASSWORD);
    submit(
        &mut a,
        "/alice/proj/settings/general",
        "/alice/proj/settings/state/archive",
        &[],
    );
    let err = git.fails(&w, &["push", &u, "HEAD:refs/heads/x"]);
    assert!(
        err.contains("403") || err.contains("permission") || err.contains("archived"),
        "{err}"
    );
    git.ok(d.path(), &["clone", "-q", &u, "c"]);
}

#[test]
fn conf_git_03_renamed_repositories_redirect_git_clients() {
    let (s, t) = setup();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    git.ok(
        &w,
        &["push", "-q", &url(&s, Some(&t), "alice", "proj"), "main"],
    );
    let mut a = s.login("alice", PASSWORD);
    submit(
        &mut a,
        "/alice/proj/settings/general",
        "/alice/proj/settings/rename",
        &[("name", "renamed")],
    );
    let out = git
        .cmd(d.path())
        .args(["clone", &url(&s, Some(&t), "alice", "proj"), "c"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("redirecting to"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Others still get "not found", without the new name.
    let mut b = s.login("bob", PASSWORD);
    let bt = create_token(&mut b, "bob", &["repo:read"]);
    let err = git.fails(
        d.path(),
        &["clone", &url(&s, Some(&bt), "alice", "proj"), "c2"],
    );
    assert!(!err.contains("renamed"), "{err}");
}

#[test]
fn conf_git_04_push_options_are_not_accepted() {
    let (s, t) = setup();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    let err = git.fails(
        &w,
        &[
            "push",
            "-o",
            "visibility=public",
            &url(&s, Some(&t), "alice", "proj"),
            "main",
        ],
    );
    assert!(err.contains("push options"), "{err}");
    assert_eq!(s.client().get("/alice/proj").status, 404, "still private");
}

#[test]
fn conf_git_03_push_to_create_is_not_supported() {
    let (s, t) = setup();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    let err = git.fails(
        &w,
        &["push", &url(&s, Some(&t), "alice", "brand-new"), "main"],
    );
    assert!(err.contains("not found"), "{err}");
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM repositories"),
        "2"
    );
}

#[test]
fn conf_git_07_push_size_is_limited() {
    let s = TestServer::start_full(&[], "[limits]\npush_max_bytes = \"1KiB\"\n");
    s.create_user("alice", PASSWORD, false);
    let mut a = s.login("alice", PASSWORD);
    create_repo(&mut a, "alice", "proj", "private");
    let t = create_token(&mut a, "rw", &["repo:write"]);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    // 64 KiB of incompressible data.
    let mut data = Vec::with_capacity(64 * 1024);
    let mut x: u32 = 0x1234_5678;
    for _ in 0..64 * 1024 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        data.push(x as u8);
    }
    std::fs::write(w.join("big.bin"), data).unwrap();
    git.ok(&w, &["add", "big.bin"]);
    git.ok(&w, &["commit", "-q", "-m", "big"]);
    let err = git.fails(&w, &["push", &url(&s, Some(&t), "alice", "proj"), "main"]);
    assert!(err.contains("exceeds maximum allowed size"), "{err}");
    assert_eq!(s.db.query_one("SELECT count(*)::text FROM git_refs"), "0");
}
