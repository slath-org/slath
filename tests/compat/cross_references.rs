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

/// The state of an issue (as alice sees it).
fn state(c: &Ctx, repo: &str, n: i64) -> String {
    c.api("GET", &format!("/{repo}/issues/{n}"), None).json()["state"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn eventually(c: &Ctx, what: &str, mut f: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !f() {
        assert!(start.elapsed().as_secs() < 30, "{what}\n{}", c.s.log());
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[test]
fn conf_issue_02_closing_keywords_close_only_what_the_actor_may_close() {
    let mut c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = c.seed(d.path());
    for t in ["By push", "By description", "By merged commit", "Untouched"] {
        c.issue("alice/demo", t, "");
    }
    create_repo(&mut c.alice, "alice", "priv", "private");
    c.issue("alice/priv", "Secret", "");

    // A keyword on another branch does nothing until it lands.
    c.git.ok(&w, &["switch", "-q", "-c", "topic"]);
    c.commit(&w, "t.txt", "Topic work\n\nFixes #3");
    c.push(&w, "topic");
    let mr = c.mr("topic", "Closes #2, see #4.");
    assert_eq!(mr, 5);

    // A push to the default branch closes what its commits name.
    c.git.ok(&w, &["switch", "-q", "main"]);
    let oid = c.commit(&w, "m.txt", "Repair\n\nfixes: #1, fixes alice/priv#1");
    c.push(&w, "main");
    eventually(&c, "#1 closed by the push", || {
        // alice may close in alice/priv too.
        state(&c, "alice/demo", 1) == "closed" && state(&c, "alice/priv", 1) == "closed"
    });
    let page = c.alice.get("/alice/demo/issues/1").body;
    assert!(page.contains("closed this as completed in"), "{page}");
    assert!(
        page.contains(&format!("href=\"/alice/demo/commit/{oid}\"")),
        "{page}"
    );
    assert_eq!(state(&c, "alice/demo", 3), "open");

    // Merging closes what the description and the merged commits name.
    let head = c.api("GET", "/alice/demo/merge-requests/5", None).json()["head_oid"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = c.api(
        "POST",
        "/alice/demo/merge-requests/5/merge",
        Some(json!({"strategy": "merge-commit", "expected_head_oid": head})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    eventually(&c, "#2 and #3 closed by the merge", || {
        state(&c, "alice/demo", 2) == "closed" && state(&c, "alice/demo", 3) == "closed"
    });
    let page = c.alice.get("/alice/demo/issues/2").body;
    assert!(
        page.contains("href=\"/alice/demo/merge-requests/5\""),
        "{page}"
    );
    assert_eq!(state(&c, "alice/demo", 4), "open");

    // bob can read alice/demo but not close its issues, and can't read
    // alice/priv: his keywords change nothing.
    let r = c.api(
        "PATCH",
        "/alice/demo",
        Some(json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);
    c.issue("alice/priv", "Another secret", "");
    c.s.create_user("bob", PASSWORD, false);
    let mut bob = c.s.login("bob", PASSWORD);
    create_repo(&mut bob, "bob", "tools", "public");
    let token = slath_tests::http::create_token(&mut bob, "b", &["repo:write", "issues:write"]);
    let r = c.s.client().api(
        "POST",
        "/api/v1/repos/bob/tools/issues",
        Some(&token),
        Some(&json!({"title": "Marker"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let url = format!(
        "http://git:{token}@{}/bob/tools.git",
        c.s.base().trim_start_matches("http://")
    );
    let b = d.path().join("b");
    std::fs::create_dir_all(&b).unwrap();
    c.git.ok(&b, &["init", "-q", "-b", "main"]);
    // The same commit closes bob's own issue, after the others.
    c.commit(
        &b,
        "x.txt",
        "Fixes alice/demo#4, fixes alice/priv#2, closes #1",
    );
    c.git.ok(&b, &["push", "-q", &url, "main"]);
    eventually(&c, "bob's marker closed", || {
        state(&c, "bob/tools", 1) == "closed"
    });
    assert_eq!(state(&c, "alice/demo", 4), "open");
    assert_eq!(state(&c, "alice/priv", 2), "open");
}
