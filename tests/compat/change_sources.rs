// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Change sources: branches under the repository's prefix (`push-` by
//! default, as `jj git push --change` names its bookmarks) that exist only
//! for a merge request. Branch lists leave them out.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

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

    fn branches(&self) -> Vec<String> {
        self.api("GET", "/branches", None).json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["name"].as_str().unwrap().to_owned())
            .collect()
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
