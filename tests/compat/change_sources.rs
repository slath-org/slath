// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Change sources: branches under the repository's prefix (`push-` by
//! default, as `jj git push --change` names its bookmarks) that exist only
//! for a merge request. Pushing one opens it without naming a branch
//! anywhere, and branch lists leave them out. The same for Git and jj.

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
        let token = slath_tests::http::create_token(&mut alice, "c", &["repo:write", "mrs:write"]);
        Self {
            url: s.url(&DEMO).replace(&s.token, &token),
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

    fn mrs(&self) -> Vec<Value> {
        self.api("GET", "/merge-requests?state=all", None).json()["items"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn branches(&self) -> Vec<String> {
        self.api("GET", "/branches", None).json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["name"].as_str().unwrap().to_owned())
            .collect()
    }

    fn wait_revisions(&self, n: i64, k: i64) {
        let start = Instant::now();
        while self
            .api("GET", &format!("/merge-requests/{n}"), None)
            .json()["revisions"]
            != k
        {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "!{n} has no revision {k}\n{}",
                self.s.log()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// The UI pages that list branches, as alice sees them.
    fn branch_pages(&self) -> String {
        let mut ui = self.s.login("alice", PASSWORD);
        let mut out = String::new();
        for page in ["/alice/demo/branches", "/alice/demo/compare"] {
            let r = ui.get(page);
            assert_eq!(r.status, 200, "{page}: {}", r.body);
            out.push_str(&r.body);
        }
        out
    }

    fn page(&self, path: &str) -> String {
        let mut ui = self.s.login("alice", PASSWORD);
        let r = ui.get(path);
        assert_eq!(r.status, 200, "{path}: {}", r.body);
        r.body
    }
}

/// The `slath:` lines of a client's push output, spaces collapsed.
fn slath_lines(stderr: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter_map(|l| l.strip_prefix("remote: slath: "))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// A Git clone of the demo repository with `main` holding a README.
fn seed(c: &Ctx, w: &Path) {
    std::fs::create_dir_all(w).unwrap();
    c.git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "demo\n").unwrap();
    c.git.ok(w, &["add", "-A"]);
    c.git.ok(w, &["commit", "-q", "-m", "init"]);
    c.git.ok(w, &["push", "-q", &c.url, "main"]);
}

fn git_push(c: &Ctx, w: &Path, refspec: &str) -> Vec<String> {
    let out = c
        .git
        .cmd(w)
        .args(["push", "--force", &c.url, refspec])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    slath_lines(&out.stderr)
}

#[test]
fn conf_jj_13_jj_push_change_opens_a_merge_request() {
    for (version, bin) in jj_binaries() {
        let c = Ctx::start();
        let d = tempfile::tempdir().unwrap();
        seed(&c, &d.path().join("w"));
        let jj = Jj::new(&version, &bin);
        let j = d.path().join("jj");
        jj.ok(d.path(), &["git", "clone", &c.url, j.to_str().unwrap()]);
        jj.ok(&j, &["new", "main"]);
        std::fs::write(j.join("a.txt"), "a\n").unwrap();
        jj.ok(&j, &["commit", "-m", "Add a\n\nThe first letter."]);
        let id = jj.show(&j, "@-", "change_id");
        let out = jj
            .cmd(&j)
            .args(["git", "push", "--change", "@-"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{version}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let lines = slath_lines(&out.stderr);
        assert_eq!(
            lines[0],
            format!("Opened !1 for change {}:", &id[..8]),
            "{version}: {lines:?}"
        );
        assert!(
            lines[1].ends_with("/alice/demo/merge-requests/1"),
            "{lines:?}"
        );

        let mrs = c.mrs();
        assert_eq!(mrs.len(), 1, "{version}");
        let m = &mrs[0];
        let branch = m["source_branch"].as_str().unwrap().to_owned();
        assert!(
            branch.starts_with("push-") && id.starts_with(&branch[5..]),
            "{branch}"
        );
        assert_eq!(m["title"], "Add a");
        assert_eq!(m["description"], "The first letter.");
        assert_eq!(m["target_branch"], "main");
        assert_eq!(m["change_source"], true);
        // Out of branch lists; the merge request names the change.
        assert!(!c.branches().contains(&branch), "{:?}", c.branches());
        assert!(!c.branch_pages().contains(&branch));
        let page = c.page("/alice/demo/merge-requests/1");
        assert!(
            page.contains(&format!(
                "change <code data-tip=\"{branch}\">{}</code>",
                &id[..8]
            )),
            "{page}"
        );
        assert!(
            c.page("/alice/demo/merge-requests")
                .contains(&format!(">{}</code>", &id[..8]))
        );

        // Pushing the change again updates the same merge request.
        std::fs::write(j.join("a.txt"), "a, edited\n").unwrap();
        jj.ok(&j, &["squash", "--into", "@-"]);
        jj.ok(&j, &["git", "push", "--bookmark", &branch]);
        c.wait_revisions(1, 2);
        assert_eq!(c.mrs().len(), 1, "{version}");
    }
}

#[test]
fn conf_jj_13_git_pushes_of_change_sources() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    seed(&c, &w);
    // A branch at main has nothing to merge: nothing opens.
    let lines = git_push(&c, &w, "HEAD:refs/heads/push-empty");
    assert_eq!(
        lines,
        ["push-empty has no commits that aren't on main yet, so no merge request was opened."]
    );
    assert!(c.mrs().is_empty());

    // A Git commit pushed as a change source opens its merge request; a
    // name that isn't a change ID shows as the branch.
    std::fs::write(w.join("b.txt"), "b\n").unwrap();
    c.git.ok(&w, &["add", "-A"]);
    c.git.ok(&w, &["commit", "-q", "-m", "Add b"]);
    let lines = git_push(&c, &w, "HEAD:refs/heads/push-fix-b");
    assert_eq!(lines[0], "Opened !1 for push-fix-b:", "{lines:?}");
    let m = &c.mrs()[0];
    assert_eq!(
        (&m["title"], &m["change_source"]),
        (&json!("Add b"), &json!(true))
    );
    assert!(!c.branches().contains(&"push-fix-b".to_owned()));
    let page = c.page("/alice/demo/merge-requests/1");
    assert!(page.contains("push-fix-b"), "{page}");
    // The next push reports on it like on any merge request's branch.
    std::fs::write(w.join("b.txt"), "b, edited\n").unwrap();
    c.git
        .ok(&w, &["commit", "-q", "-a", "--amend", "-m", "Add b"]);
    let lines = git_push(&c, &w, "HEAD:refs/heads/push-fix-b");
    assert!(
        lines.iter().any(|l| l.contains("!1 for push-fix-b")),
        "{lines:?}"
    );
    assert_eq!(c.mrs().len(), 1);

    // The push asks for the merge request, so a token that may push opens
    // it without mrs:write.
    std::fs::write(w.join("c.txt"), "c\n").unwrap();
    c.git.ok(&w, &["add", "-A"]);
    c.git.ok(&w, &["commit", "-q", "-m", "Add c"]);
    let out = c
        .git
        .cmd(&w)
        .args(["push", &c.s.url(&DEMO), "HEAD:refs/heads/push-c"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let lines = slath_lines(&out.stderr);
    assert_eq!(lines[0], "Opened !2 for push-c:", "{lines:?}");
    assert_eq!(c.mrs().len(), 2);
    // ...but not through the API: opening one there still needs mrs:write.
    let r = c.s.client().api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&c.s.token),
        Some(&json!({"source_branch": "topic-api", "title": "Via the API"})),
    );
    assert_eq!(r.status, 403, "{}", r.body);

    // Ordinary branches are untouched.
    let lines = git_push(&c, &w, "HEAD:refs/heads/topic");
    assert_eq!(lines[0], "Create a merge request for topic:", "{lines:?}");
    assert!(c.branches().contains(&"topic".to_owned()));
}

#[test]
fn conf_jj_13_change_sources_stay_out_of_branch_lists() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    seed(&c, &w);
    assert_eq!(
        c.api("GET", "", None).json()["change_source_prefix"],
        "push-"
    );
    for bad in [
        "push",
        "-x-",
        "a b-",
        "mr/",
        &format!("{}-", "a".repeat(32)),
    ] {
        let r = c.api("PATCH", "", Some(json!({"change_source_prefix": bad})));
        assert_eq!(r.status, 422, "{bad}: {}", r.body);
    }
    // A writer can't change it; repository settings are the administrators'.
    c.s.create_user("bob", PASSWORD, false);
    let r = c.api("PUT", "/collaborators/bob", Some(json!({"role": "write"})));
    assert!(r.status < 300, "{}", r.body);
    let mut bob = c.s.login("bob", PASSWORD);
    let bob_token = slath_tests::http::create_token(&mut bob, "b", &["repo:write"]);
    let r = c.s.client().api(
        "PATCH",
        "/api/v1/repos/alice/demo",
        Some(&bob_token),
        Some(&json!({"change_source_prefix": ""})),
    );
    assert_eq!(r.status, 403, "{}", r.body);

    std::fs::write(w.join("c.txt"), "c\n").unwrap();
    c.git.ok(&w, &["add", "-A"]);
    c.git.ok(&w, &["commit", "-q", "-m", "Add c"]);
    git_push(&c, &w, "HEAD:refs/heads/push-kxqzlmnopqrs");
    git_push(&c, &w, "HEAD:refs/heads/push-c/2");
    git_push(&c, &w, "HEAD:refs/heads/topic");
    let mut branches = c.branches();
    branches.sort();
    assert_eq!(branches, ["main", "topic"]);
    let pages = c.branch_pages();
    assert!(!pages.contains("push-"), "{pages}");
    // Nor in the banner of recently pushed branches.
    assert!(!c.page("/alice/demo").contains("push-"));
    // The link a push prints still offers the branch.
    let page = c.page("/alice/demo/compare?source=push-kxqzlmnopqrs");
    assert!(
        page.contains("<option value=\"push-kxqzlmnopqrs\" selected"),
        "{page}"
    );

    // Another prefix: review/ branches are hidden, push-* listed.
    let r = c.api(
        "PATCH",
        "",
        Some(json!({"change_source_prefix": "review/"})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    git_push(&c, &w, "HEAD:refs/heads/review/c/1");
    let branches = c.branches();
    assert!(branches.contains(&"push-c/2".to_owned()), "{branches:?}");
    assert!(!branches.contains(&"review/c/1".to_owned()), "{branches:?}");

    // Turned off, every branch is a branch again.
    let r = c.api("PATCH", "", Some(json!({"change_source_prefix": ""})));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(c.branches().len(), 5);

    // The settings form sets it too, and says what's wrong.
    let mut ui = c.s.login("alice", PASSWORD);
    let form = |ui: &mut slath_tests::http::Client, prefix: &str| {
        slath_tests::http::submit(
            ui,
            "/alice/demo/settings/general",
            "/alice/demo/settings/merge-requests",
            &[
                ("stalled_after_days", "14"),
                ("change_source_prefix", prefix),
            ],
        )
    };
    let r = form(&mut ui, "no end");
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("Use letters, digits"), "{}", r.body);
    let r = form(&mut ui, "jj-");
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(c.api("GET", "", None).json()["change_source_prefix"], "jj-");
}

/// Whether the demo repository has `branch`, as Git sees it.
fn has_branch(c: &Ctx, branch: &str) -> bool {
    !c.git
        .ok(
            &c.s.repo_path(&DEMO),
            &["for-each-ref", &format!("refs/heads/{branch}")],
        )
        .trim()
        .is_empty()
}

/// Waits until `branch` is gone from the demo repository.
fn wait_gone(c: &Ctx, branch: &str) {
    let start = Instant::now();
    while has_branch(c, branch) {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "{branch} is still there\n{}",
            c.s.log()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn conf_jj_13_nothing_is_left_after_merge_or_close() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    seed(&c, &w);
    let commit = |file: &str| {
        c.git.ok(&w, &["switch", "-q", "--detach", "main"]);
        std::fs::write(w.join(file), format!("{file}\n")).unwrap();
        c.git.ok(&w, &["add", "-A"]);
        c.git
            .ok(&w, &["commit", "-q", "-m", &format!("Add {file}")]);
    };
    let number = |branch: &str| {
        c.mrs()
            .iter()
            .find(|m| m["source_branch"] == branch)
            .unwrap()["number"]
            .as_i64()
            .unwrap()
    };

    // Closed: the change source goes; reopening asks for it again.
    commit("a.txt");
    git_push(&c, &w, "HEAD:refs/heads/push-a");
    let a = number("push-a");
    let r = c.api(
        "PATCH",
        &format!("/merge-requests/{a}"),
        Some(json!({"state": "closed"})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    wait_gone(&c, "push-a");
    let r = c.api(
        "PATCH",
        &format!("/merge-requests/{a}"),
        Some(json!({"state": "open"})),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.body.contains("The source branch was deleted"),
        "{}",
        r.body
    );

    // Merged in Slath.
    commit("b.txt");
    git_push(&c, &w, "HEAD:refs/heads/push-b");
    let b = number("push-b");
    let head = c.api("GET", &format!("/merge-requests/{b}"), None).json()["head_oid"].clone();
    let r = c.api(
        "POST",
        &format!("/merge-requests/{b}/merge"),
        Some(json!({"strategy": "merge-commit", "expected_head_oid": head})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    wait_gone(&c, "push-b");

    // Merged by pushing its commit to main.
    c.git.ok(&w, &["fetch", "-q", &c.url, "main"]);
    c.git.ok(&w, &["switch", "-q", "--detach", "FETCH_HEAD"]);
    std::fs::write(w.join("c.txt"), "c\n").unwrap();
    c.git.ok(&w, &["add", "-A"]);
    c.git.ok(&w, &["commit", "-q", "-m", "Add c"]);
    git_push(&c, &w, "HEAD:refs/heads/push-c");
    let n = number("push-c");
    git_push(&c, &w, "HEAD:refs/heads/main");
    let start = Instant::now();
    while c.api("GET", &format!("/merge-requests/{n}"), None).json()["state"] != "merged" {
        assert!(start.elapsed() < Duration::from_secs(30), "{}", c.s.log());
        std::thread::sleep(Duration::from_millis(200));
    }
    wait_gone(&c, "push-c");

    // A change source that another open merge request still uses stays.
    git_push(&c, &w, "refs/heads/main:refs/heads/release");
    commit("d.txt");
    git_push(&c, &w, "HEAD:refs/heads/push-d");
    let d1 = number("push-d");
    let r = c.api(
        "POST",
        "/merge-requests",
        Some(
            json!({"source_branch": "push-d", "target_branch": "release", "title": "Into release"}),
        ),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let r = c.api(
        "PATCH",
        &format!("/merge-requests/{d1}"),
        Some(json!({"state": "closed"})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(has_branch(&c, "push-d"));
    // Ordinary branches are never deleted on close.
    commit("e.txt");
    git_push(&c, &w, "HEAD:refs/heads/topic-e");
    let r = c.api(
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic-e", "title": "Add e"})),
    );
    let e = r.json()["number"].as_i64().unwrap();
    let r = c.api(
        "PATCH",
        &format!("/merge-requests/{e}"),
        Some(json!({"state": "closed"})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(has_branch(&c, "topic-e"));
}

/// A Git push expected to fail; its `slath:` lines.
fn git_push_fails(c: &Ctx, w: &Path, refspec: &str) -> Vec<String> {
    let out = c
        .git
        .cmd(w)
        .args(["push", &c.url, refspec])
        .output()
        .unwrap();
    assert!(!out.status.success(), "{refspec} was accepted");
    slath_lines(&out.stderr)
}

#[test]
fn conf_jj_13_git_pushes_to_refs_for_open_and_update() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    seed(&c, &w);
    std::fs::write(w.join("f.txt"), "f\n").unwrap();
    c.git.ok(&w, &["add", "-A"]);
    c.git
        .ok(&w, &["commit", "-q", "-m", "Add f\n\nWith a reason."]);
    let oid = c.git.ok(&w, &["rev-parse", "HEAD"]).trim()[..12].to_owned();
    let out = c
        .git
        .cmd(&w)
        .args(["push", &c.url, "HEAD:refs/for/main"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{err}");
    // Git names the branch it got; Slath says how to push again.
    assert!(err.contains(&format!("HEAD -> push-{oid}")), "{err}");
    let lines = slath_lines(&out.stderr);
    assert_eq!(
        lines[..2],
        [
            format!(
                "Pushed refs/for/main to push-{oid}; push to refs/for/main/{oid} again to update it."
            ),
            format!("Opened !1 for push-{oid}:"),
        ],
        "{lines:?}"
    );
    let m = &c.mrs()[0];
    assert_eq!(m["source_branch"], format!("push-{oid}"));
    assert_eq!(
        (&m["title"], &m["description"]),
        (&json!("Add f"), &json!("With a reason."))
    );
    assert_eq!(
        (&m["target_branch"], &m["change_source"]),
        (&json!("main"), &json!(true))
    );
    assert!(!has_branch(&c, "for/main") && !c.branches().iter().any(|b| b.contains("for/")));

    // An amended commit has another ID: the topic the output named finds
    // the same merge request.
    std::fs::write(w.join("f.txt"), "f, edited\n").unwrap();
    c.git
        .ok(&w, &["commit", "-q", "-a", "--amend", "--no-edit"]);
    git_push(&c, &w, &format!("HEAD:refs/for/main/{oid}"));
    c.wait_revisions(1, 2);
    assert_eq!(c.mrs().len(), 1);

    // A topic of one's own; a target with slashes.
    git_push(&c, &w, "HEAD~1:refs/heads/release/2.0");
    let lines = git_push(&c, &w, "HEAD:refs/for/release/2.0/hot-fix");
    assert_eq!(lines[1], "Opened !2 for push-hot-fix:", "{lines:?}");
    let m = c
        .mrs()
        .into_iter()
        .find(|m| m["source_branch"] == "push-hot-fix")
        .unwrap();
    assert_eq!(m["target_branch"], "release/2.0");

    // Someone who can only read can't open one this way either.
    c.s.create_user("bob", PASSWORD, false);
    let r = c.api("PUT", "/collaborators/bob", Some(json!({"role": "read"})));
    assert!(r.status < 300, "{}", r.body);
    let mut bob = c.s.login("bob", PASSWORD);
    let bob_token = slath_tests::http::create_token(&mut bob, "b", &["repo:write", "mrs:write"]);
    let bob_url = c.url.replace(&c.token, &bob_token);
    let out = c
        .git
        .cmd(&w)
        .args(["push", &bob_url, "HEAD:refs/for/main/bob"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!has_branch(&c, "push-bob"));
    assert_eq!(c.mrs().len(), 2);

    // What can't be pushed says why and leaves nothing.
    let lines = git_push_fails(&c, &w, "HEAD:refs/for/nowhere");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("no branch nowhere to open a merge request into")),
        "{lines:?}"
    );
    let lines = git_push_fails(&c, &w, ":refs/for/main");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("refs/for/ can't be deleted")),
        "{lines:?}"
    );
    let r = c.api("PATCH", "", Some(json!({"change_source_prefix": ""})));
    assert_eq!(r.status, 200, "{}", r.body);
    let lines = git_push_fails(&c, &w, "HEAD:refs/for/main/other");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("pushes to refs/for/ are turned off in this repository")),
        "{lines:?}"
    );
    assert_eq!(c.mrs().len(), 2);
    assert!(!has_branch(&c, "push-other"));
}

#[test]
fn conf_jj_13_refs_for_and_jj_push_change_meet_in_one_merge_request() {
    for (version, bin) in jj_binaries() {
        let c = Ctx::start();
        let d = tempfile::tempdir().unwrap();
        seed(&c, &d.path().join("w"));
        let jj = Jj::new(&version, &bin);
        let j = d.path().join("jj");
        jj.ok(d.path(), &["git", "clone", &c.url, j.to_str().unwrap()]);
        jj.ok(&j, &["new", "main"]);
        std::fs::write(j.join("g.txt"), "g\n").unwrap();
        jj.ok(&j, &["commit", "-m", "Add g"]);
        let id = jj.show(&j, "@-", "change_id");
        let oid = jj.show(&j, "@-", "commit_id");
        // Git pushes jj's commit to refs/for/main: the change source is
        // named by its change ID, as jj would name it.
        let store = crate::jj::git_store(&j);
        let out = c
            .git
            .cmd(&store)
            .args(["push", &c.url, &format!("{oid}:refs/for/main")])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let lines = slath_lines(&out.stderr);
        assert_eq!(
            lines[..2],
            [
                format!(
                    "Pushed refs/for/main to push-{}; push to refs/for/main again to update it.",
                    &id[..12]
                ),
                format!("Opened !1 for change {}:", &id[..8]),
            ],
            "{version}: {lines:?}"
        );
        // jj takes the bookmark over as any other pushed bookmark and
        // updates the same merge request with its own command.
        jj.ok(&j, &["git", "fetch"]);
        jj.ok(
            &j,
            &["bookmark", "track", &format!("push-{}@origin", &id[..12])],
        );
        std::fs::write(j.join("g.txt"), "g, edited\n").unwrap();
        jj.ok(&j, &["squash", "--into", "@-"]);
        jj.ok(&j, &["git", "push", "--change", "@-"]);
        c.wait_revisions(1, 2);
        let mrs = c.mrs();
        assert_eq!(mrs.len(), 1, "{version}");
        assert_eq!(mrs[0]["source_branch"], format!("push-{}", &id[..12]));
    }
}
