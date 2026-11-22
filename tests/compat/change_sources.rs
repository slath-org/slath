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
        // Pushing opens merge requests only with a token that may.
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

    // A token without mrs:write pushes but can't open one, and says so.
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
    assert_eq!(
        lines[0],
        "couldn't open a merge request for push-c: the token you pushed with lacks the mrs:write scope",
        "{lines:?}"
    );
    assert!(
        lines[2].ends_with("/alice/demo/compare?source=push-c"),
        "{lines:?}"
    );
    assert_eq!(c.mrs().len(), 1);
    // That link offers the branch, though lists leave it out.
    let page = c.page("/alice/demo/compare?source=push-c");
    assert!(page.contains("<option value=\"push-c\" selected"), "{page}");

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
