// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! References between issues and merge requests (ISSUE-02): `#n` links to
//! either, each records "mentioned this in" in what it points at once, and
//! a mention from a private repository shows only to its readers.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::http::{Client, create_repo};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

pub(crate) struct Ctx {
    pub s: TestServer,
    pub alice: Client,
    pub token: String,
    pub url: String,
    pub git: GitClient,
}

impl Ctx {
    pub fn start() -> Self {
        let s = TestServer::start();
        let mut alice = s.login("alice", PASSWORD);
        let token = slath_tests::http::create_token(
            &mut alice,
            "x",
            &["repo:write", "issues:write", "mrs:write"],
        );
        Self {
            url: s.url(&DEMO),
            s,
            alice,
            token,
            git: GitClient::new(),
        }
    }

    pub fn api(&self, method: &str, path: &str, body: Option<Value>) -> slath_tests::http::Resp {
        self.s.client().api(
            method,
            &format!("/api/v1/repos{path}"),
            Some(&self.token),
            body.as_ref(),
        )
    }

    pub fn issue(&self, repo: &str, title: &str, body: &str) -> i64 {
        let r = self.api(
            "POST",
            &format!("/{repo}/issues"),
            Some(json!({"title": title, "body": body})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        r.json()["number"].as_i64().unwrap()
    }

    pub fn mr(&self, branch: &str, description: &str) -> i64 {
        let r = self.api(
            "POST",
            "/alice/demo/merge-requests",
            Some(json!({"source_branch": branch, "title": format!("Merge {branch}"), "description": description})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        r.json()["number"].as_i64().unwrap()
    }

    /// A clone with `main` pushed.
    pub fn seed(&self, root: &Path) -> std::path::PathBuf {
        let w = root.join("w");
        std::fs::create_dir_all(&w).unwrap();
        self.git.ok(&w, &["init", "-q", "-b", "main"]);
        self.commit(&w, "README", "init");
        self.push(&w, "main");
        w
    }

    pub fn commit(&self, w: &Path, file: &str, msg: &str) -> String {
        std::fs::write(w.join(file), msg).unwrap();
        self.git.ok(w, &["add", "-A"]);
        self.git.ok(w, &["commit", "-q", "-m", msg]);
        self.git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    pub fn push(&self, w: &Path, refspec: &str) {
        self.git.ok(w, &["push", "-q", &self.url, refspec]);
    }
}

#[test]
fn conf_issue_02_issues_and_merge_requests_refer_to_each_other() {
    let mut c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = c.seed(d.path());
    c.git.ok(&w, &["switch", "-q", "-c", "topic"]);
    c.commit(&w, "b.txt", "Add b");
    c.push(&w, "topic");

    assert_eq!(c.issue("alice/demo", "Broken b", ""), 1);
    assert_eq!(c.mr("topic", "Works on #1."), 2);
    let issue = c.alice.get("/alice/demo/issues/1").body;
    assert_eq!(issue.matches("mentioned this in").count(), 1, "{issue}");
    assert!(
        issue.contains("href=\"/alice/demo/merge-requests/2\""),
        "{issue}"
    );

    // The same reference again (edit, comment) records nothing new.
    let r = c.api(
        "PATCH",
        "/alice/demo/merge-requests/2",
        Some(json!({"description": "Still works on #1."})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let r = c.api(
        "POST",
        "/alice/demo/merge-requests/2/comments",
        Some(json!({"body": "See #1"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let issue = c.alice.get("/alice/demo/issues/1").body;
    assert_eq!(issue.matches("mentioned this in").count(), 1, "{issue}");

    // `#2` in an issue links to the merge request, with its state.
    let r = c.api(
        "POST",
        "/alice/demo/issues/1/comments",
        Some(json!({"body": "Fixed by #2"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let issue = c.alice.get("/alice/demo/issues/1").body;
    assert!(
        issue.contains("class=\"ref ref-open\" href=\"/alice/demo/merge-requests/2\" title=\"Merge topic (open)\""),
        "{issue}"
    );
    let mr = c.alice.get("/alice/demo/merge-requests/2").body;
    assert!(mr.contains("mentioned this in"), "{mr}");
    assert!(mr.contains("href=\"/alice/demo/issues/1\""), "{mr}");

    // From a private repository: recorded, but shown only to its readers.
    let r = c.api(
        "PATCH",
        "/alice/demo",
        Some(json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);
    create_repo(&mut c.alice, "alice", "priv", "private");
    c.issue("alice/priv", "Secret", "Blocked on alice/demo#2");
    c.s.create_user("bob", PASSWORD, false);
    let bob =
        c.s.login("bob", PASSWORD)
            .get("/alice/demo/merge-requests/2")
            .body;
    assert_eq!(bob.matches("mentioned this in").count(), 1, "{bob}");
    assert!(!bob.contains("/alice/priv"), "{bob}");
    let anon = c.s.client().get("/alice/demo/merge-requests/2").body;
    assert!(!anon.contains("/alice/priv"), "{anon}");
    let mr = c.alice.get("/alice/demo/merge-requests/2").body;
    assert!(mr.contains("href=\"/alice/priv/issues/1\""), "{mr}");
}
