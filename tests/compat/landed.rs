// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Landing by push: a merge request whose commits reach its target branch
//! by `git push` or `jj git push` of that branch is merged by the pusher,
//! with no step in Slath. Merge requests whose commits didn't land stay
//! open.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::jj::{Jj, jj_binaries};

struct Ctx {
    s: TestServer,
    token: String,
    url: String,
    git: GitClient,
}

impl Ctx {
    fn start() -> Self {
        let s = TestServer::start();
        let mut alice = s.login("alice", PASSWORD);
        let token = slath_tests::http::create_token(&mut alice, "l", &["repo:write", "mrs:write"]);
        Self {
            url: s.url(&DEMO),
            s,
            token,
            git: GitClient::new(),
        }
    }

    fn api(&self, method: &str, path: &str, body: Option<Value>) -> slath_tests::http::Resp {
        self.s.client().api(
            method,
            &format!("/api/v1/repos/alice/demo{path}"),
            Some(&self.token),
            body.as_ref(),
        )
    }

    fn open_mr(&self, branch: &str) -> i64 {
        let r = self.api(
            "POST",
            "/merge-requests",
            Some(json!({"source_branch": branch, "title": format!("Merge {branch}")})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        r.json()["number"].as_i64().unwrap()
    }

    fn mr(&self, n: i64) -> Value {
        self.api("GET", &format!("/merge-requests/{n}"), None)
            .json()
    }

    /// Waits until merge request `n` is in `state`.
    fn wait_state(&self, n: i64, state: &str) -> Value {
        let start = Instant::now();
        loop {
            let m = self.mr(n);
            if m["state"] == state {
                return m;
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "!{n} is {}, not {state}\n{}",
                m["state"],
                self.s.log()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn page(&self, n: i64) -> String {
        let mut alice = self.s.login("alice", PASSWORD);
        let r = alice.get(&format!("/alice/demo/merge-requests/{n}"));
        assert_eq!(r.status, 200);
        r.body
    }

    fn push(&self, w: &Path, refspecs: &[&str]) {
        let mut args = vec!["push", "-q", self.url.as_str()];
        args.extend_from_slice(refspecs);
        self.git.ok(w, &args);
    }
}

fn seed(c: &Ctx, w: &Path) {
    std::fs::create_dir_all(w).unwrap();
    c.git.ok(w, &["init", "-q", "-b", "main"]);
    commit(c, w, "README", "demo\n", "init");
    c.push(w, &["main"]);
}

fn commit(c: &Ctx, w: &Path, file: &str, text: &str, msg: &str) {
    std::fs::write(w.join(file), text).unwrap();
    c.git.ok(w, &["add", "-A"]);
    c.git.ok(w, &["commit", "-q", "-m", msg]);
}

#[test]
fn conf_review_07_git_push_of_the_target_merges() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    seed(&c, &w);

    // Two topics; only the first reaches main.
    c.git.ok(&w, &["switch", "-q", "-c", "topic", "main"]);
    commit(&c, &w, "a.txt", "a\n", "Add a");
    commit(&c, &w, "b.txt", "b\n", "Add b");
    c.push(&w, &["topic"]);
    let landed = c.open_mr("topic");
    c.git.ok(&w, &["switch", "-q", "-c", "other", "main"]);
    commit(&c, &w, "o.txt", "o\n", "Other");
    c.push(&w, &["other"]);
    let other = c.open_mr("other");

    // Fast-forward main locally and push it.
    c.git.ok(&w, &["switch", "-q", "main"]);
    c.git.ok(&w, &["merge", "-q", "--ff-only", "topic"]);
    c.push(&w, &["main"]);
    let m = c.wait_state(landed, "merged");
    assert!(m["merged_at"].is_string(), "{m}");
    assert!(
        c.page(landed).contains("merged this by pushing to main"),
        "the timeline names the push"
    );
    // The unrelated merge request stays open; so does it after more pushes.
    commit(&c, &w, "c.txt", "c\n", "More on main");
    c.push(&w, &["main"]);
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(c.mr(other)["state"], "open");
}

#[test]
fn conf_review_07_merge_commit_pushed_with_the_source_deleted() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    seed(&c, &w);
    c.git.ok(&w, &["switch", "-q", "-c", "topic", "main"]);
    commit(&c, &w, "a.txt", "a\n", "Add a");
    c.push(&w, &["topic"]);
    let n = c.open_mr("topic");
    // main moved meanwhile, so a fast-forward isn't possible.
    c.git.ok(&w, &["switch", "-q", "main"]);
    commit(&c, &w, "m.txt", "m\n", "Main moves");
    c.push(&w, &["main"]);

    // The source gains a commit, then everything lands in one push that
    // also deletes the branch: merged with that last commit, not closed.
    c.git.ok(&w, &["switch", "-q", "topic"]);
    commit(&c, &w, "b.txt", "b\n", "Add b");
    let head = c.git.ok(&w, &["rev-parse", "HEAD"]).trim().to_owned();
    c.git.ok(&w, &["switch", "-q", "main"]);
    c.git.ok(
        &w,
        &["merge", "-q", "--no-ff", "-m", "Merge topic", "topic"],
    );
    c.push(&w, &["topic", "main"]);
    c.push(&w, &[":topic"]);
    let m = c.wait_state(n, "merged");
    assert_eq!(m["head_oid"], head.as_str(), "{m}");
    assert_eq!(m["revisions"], 2, "{m}");
}

#[test]
fn conf_review_07_target_and_source_deleted_in_one_push() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    seed(&c, &w);
    c.git.ok(&w, &["switch", "-q", "-c", "topic", "main"]);
    commit(&c, &w, "a.txt", "a\n", "Add a");
    c.push(&w, &["topic"]);
    let n = c.open_mr("topic");
    c.git.ok(&w, &["switch", "-q", "main"]);
    c.git.ok(&w, &["merge", "-q", "--ff-only", "topic"]);
    c.push(&w, &["main", ":topic"]);
    c.wait_state(n, "merged");
}

#[test]
fn conf_review_07_jj_push_of_the_target_bookmark_merges() {
    for (version, bin) in jj_binaries() {
        let c = Ctx::start();
        let d = tempfile::tempdir().unwrap();
        let w = d.path().join("w");
        seed(&c, &w);
        let jj = Jj::new(&version, &bin);
        let j = d.path().join("jj");
        jj.ok(d.path(), &["git", "clone", &c.url, j.to_str().unwrap()]);
        jj.ok(&j, &["new", "main"]);
        std::fs::write(j.join("a.txt"), "a\n").unwrap();
        jj.ok(&j, &["commit", "-m", "Add a"]);
        jj.ok(&j, &["bookmark", "create", "topic", "-r", "@-"]);
        jj.ok(&j, &["git", "push", "--bookmark", "topic"]);
        let n = c.open_mr("topic");
        c.wait_state(n, "open");
        jj.ok(&j, &["bookmark", "set", "main", "-r", "topic"]);
        jj.ok(&j, &["git", "push", "--bookmark", "main"]);
        c.wait_state(n, "merged");
    }
}
