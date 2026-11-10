// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deploy keys over a real sshd (RBAC-01): a read-only key clones its
//! repository and can't push; a write key pushes as branch protection
//! allows; neither reaches any other repository, public ones and forks
//! included, and a removed key stops working. A key in use by a person is
//! refused as a deploy key.

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestRepo, TestServer};
use slath_tests::sshd::{self, Sshd};

const OTHER: TestRepo = TestRepo {
    namespace: "bob",
    name: "other",
};

struct Env {
    // Field order matters: stop sshd before the server and its temp dir go away.
    sshd: Sshd,
    s: TestServer,
    keys: tempfile::TempDir,
    git: GitClient,
}

impl Env {
    fn git_with(&self, key: &Path, cwd: &Path, args: &[&str]) -> Output {
        self.git
            .cmd(cwd)
            .env("GIT_SSH_COMMAND", self.sshd.git_ssh_command(key))
            .args(args)
            .output()
            .unwrap()
    }

    fn api(
        &self,
        t: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> slath_tests::http::Resp {
        self.s.client().api(
            method,
            &format!("/api/v1/repos{path}"),
            Some(t),
            body.as_ref(),
        )
    }
}

fn ok(out: &Output, env: &Env, what: &str) {
    assert!(
        out.status.success(),
        "{what}: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        env.sshd.logs()
    );
}

fn denied(out: &Output, what: &str, expect: &str) {
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{what} must be denied");
    assert!(
        err.contains(expect),
        "{what}: expected {expect:?} in:\n{err}"
    );
}

#[test]
fn conf_rbac_01_deploy_keys_reach_their_repository_only() {
    if sshd::skipped() {
        eprintln!("SLATH_SKIP_SSH_TESTS=1: skipping");
        return;
    }
    let keys = tempfile::tempdir().unwrap();
    let s = TestServer::start_with(&[DEMO, OTHER]);
    let sshd = Sshd::start(&s);
    let env = Env {
        sshd,
        s,
        keys,
        git: GitClient::new(),
    };
    let key = |name: &str| -> (PathBuf, String) { sshd::keygen(env.keys.path(), name) };
    let (read_key, read_pub) = key("reader");
    let (write_key, write_pub) = key("writer");
    let (_, alice_pub) = key("alice");
    env.s.add_ssh_key("alice", &alice_pub);
    let mut alice = env.s.login("alice", PASSWORD);
    let alice_t = slath_tests::http::create_token(&mut alice, "a", &["repo:write"]);
    let w = env.keys.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    env.git.ok(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "demo\n").unwrap();
    env.git.ok(&w, &["add", "."]);
    env.git.ok(&w, &["commit", "-q", "-m", "init"]);
    env.git.ok(&w, &["push", "-q", &env.s.url(&DEMO), "main"]);

    // Adding: admins only; a key in use by a person is refused.
    let add = |body: Value| env.api(&alice_t, "POST", "/alice/demo/keys", Some(body));
    let r = add(json!({"key": alice_pub, "title": "taken"}));
    assert_eq!(r.status, 422, "{}", r.body);
    let r = add(json!({"key": read_pub}));
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["can_push"], false);
    assert_eq!(
        r.json()["title"],
        "reader",
        "the comment is the default title"
    );
    let r = add(json!({"key": write_pub, "title": "deploy", "can_push": true}));
    assert_eq!(r.status, 201, "{}", r.body);
    let write_id = r.json()["id"].as_str().unwrap().to_owned();
    let r = add(json!({"key": write_pub}));
    assert_eq!(r.status, 422, "the same key twice: {}", r.body);
    let mut bob = env.s.login("bob", PASSWORD);
    let bob_t = slath_tests::http::create_token(&mut bob, "b", &["repo:write"]);
    assert_eq!(env.api(&bob_t, "GET", "/alice/demo/keys", None).status, 404);
    let list = env.api(&alice_t, "GET", "/alice/demo/keys", None);
    assert_eq!(list.status, 200, "{}", list.body);
    assert_eq!(list.json().as_array().unwrap().len(), 2);

    // Read-only: clones, can't push.
    let url = env.sshd.url("alice", "demo");
    let work = tempfile::tempdir().unwrap();
    let r = env.git_with(&read_key, work.path(), &["clone", "-q", &url, "r"]);
    ok(&r, &env, "read key clone");
    let r = work.path().join("r");
    std::fs::write(r.join("x"), "x\n").unwrap();
    env.git.ok(&r, &["add", "."]);
    env.git.ok(&r, &["commit", "-q", "-m", "x"]);
    let out = env.git_with(&read_key, &r, &["push", "-q", &url, "HEAD:refs/heads/x"]);
    assert!(!out.status.success(), "a read-only key pushed");

    // Write: pushes, but branch protection still applies.
    let out = env.git_with(&write_key, &r, &["push", "-q", &url, "HEAD:refs/heads/x"]);
    ok(&out, &env, "write key push");
    let p = env.api(
        &alice_t,
        "POST",
        "/alice/demo/protections",
        Some(json!({"pattern": "main", "require_mr": true})),
    );
    assert!(p.status < 300, "{}", p.body);
    let out = env.git_with(
        &write_key,
        &r,
        &["push", "-q", &url, "HEAD:refs/heads/main"],
    );
    denied(&out, "push to a protected main", "requires merge requests");
    // Tags can be pushed, not moved.
    let out = env.git_with(&write_key, &r, &["push", "-q", &url, "HEAD:refs/tags/v1"]);
    ok(&out, &env, "new tag");
    let out = env.git_with(
        &write_key,
        &r,
        &["push", "-q", "-f", &url, "HEAD~1:refs/tags/v1"],
    );
    denied(&out, "moving a tag", "maintain role");

    // Nothing else: a public repository and a fork of this one don't exist.
    let vis = env.api(
        &alice_t,
        "PATCH",
        "/alice/demo",
        Some(json!({"visibility": "public"})),
    );
    assert!(vis.status < 300, "{}", vis.body);
    let other = env.api(
        &bob_t,
        "PATCH",
        "/bob/other",
        Some(json!({"visibility": "public"})),
    );
    assert!(other.status < 300, "{}", other.body);
    let f = env.api(
        &bob_t,
        "POST",
        "/alice/demo/forks",
        Some(json!({"namespace": "bob"})),
    );
    assert_eq!(f.status, 202, "{}", f.body);
    let start = std::time::Instant::now();
    while env.api(&bob_t, "GET", "/bob/demo/fork", None).json()["state"] != "done" {
        assert!(start.elapsed().as_secs() < 30, "fork not done");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    for (k, name) in [(&read_key, "read"), (&write_key, "write")] {
        for (ns, repo) in [("bob", "demo"), ("bob", "other"), ("alice", "nope")] {
            let out = env.git_with(
                k,
                work.path(),
                &[
                    "clone",
                    "-q",
                    &env.sshd.url(ns, repo),
                    &format!("{name}-{ns}-{repo}"),
                ],
            );
            denied(
                &out,
                &format!("{name} key cloning {ns}/{repo}"),
                "not found",
            );
        }
    }
    // Over HTTP, the public fork is readable, so the denial is the key's.
    let anon = env.s.client().get("/bob/demo");
    assert_eq!(anon.status, 200);

    // Removed: the next connection is refused.
    let r = env.api(
        &alice_t,
        "DELETE",
        &format!("/alice/demo/keys/{write_id}"),
        None,
    );
    assert_eq!(r.status, 204, "{}", r.body);
    let out = env.git_with(&write_key, work.path(), &["clone", "-q", &url, "gone"]);
    assert!(!out.status.success(), "a removed key still works");
    let audit: i64 = env
        .s
        .db
        .query_one("SELECT count(*)::text FROM audit_events WHERE action LIKE 'deploy_key.%'")
        .parse()
        .unwrap();
    assert_eq!(audit, 3);
}
