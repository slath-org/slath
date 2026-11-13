// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The attention gate (`conf_agent_09_*`): a bot's merge request iterates
//! without asking anyone to review it, and reaches its reviewers once,
//! when it's marked ready and its required checks pass on its head.

use std::time::Duration;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Waits until every queued job that can notify or release ran.
fn settle(s: &TestServer) {
    wait_until("queued jobs", || {
        s.db.query_one(
            "SELECT count(*)::text FROM jobs
              WHERE kind IN ('mr.sync', 'mr.attention', 'notify.fanout', 'email.send')
                AND state IN ('ready', 'running')",
        ) == "0"
    });
}

fn cli_bot(s: &TestServer) -> String {
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            "alice",
            "--name",
            "alice-agent",
            "--token-scopes",
            "repo:write,mrs:write",
            "--repo",
            "alice/demo",
            "--output",
            "json",
        ],
        None,
    );
    assert!(ok, "{out}{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    v["token"].as_str().unwrap().to_owned()
}

struct Setup {
    s: TestServer,
    dir: tempfile::TempDir,
    git: GitClient,
    bot: String,
    bot_url: String,
    alice: String,
    bob: String,
    carol: String,
}

impl Setup {
    /// `main`, a bot with write access, and the writers bob (the
    /// reviewer) and carol (who reports statuses).
    fn new() -> Self {
        let s = TestServer::start();
        for u in ["bob", "carol"] {
            s.create_user(u, PASSWORD, false);
        }
        let git = GitClient::new();
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path();
        git.ok(w, &["init", "-q", "-b", "main"]);
        std::fs::write(w.join("README"), "demo\n").unwrap();
        git.ok(w, &["add", "."]);
        git.ok(w, &["commit", "-q", "-m", "init"]);
        git.ok(w, &["push", "-q", &s.url(&DEMO), "main"]);
        let bot = cli_bot(&s);
        let token = |user: &str, scopes: &[&str]| {
            let mut c = s.login(user, PASSWORD);
            slath_tests::http::create_token(&mut c, user, scopes)
        };
        let alice = token("alice", &["repo:write", "mrs:write"]);
        let bob = token("bob", &["repo:read", "mrs:write", "user:read"]);
        let carol = token("carol", &["repo:read", "statuses:write"]);
        let bot_url = format!("http://git:{bot}@{}/alice/demo.git", s.addr);
        let me = Self {
            s,
            dir,
            git,
            bot,
            bot_url,
            alice,
            bob,
            carol,
        };
        for u in ["alice-agent", "bob", "carol"] {
            let r = me.api(
                &me.alice.clone(),
                "PUT",
                &format!("/collaborators/{u}"),
                Some(json!({"role": "write"})),
            );
            assert!(r.status < 300, "{}", r.body);
        }
        me
    }

    fn api(
        &self,
        token: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> slath_tests::http::Resp {
        self.s.client().api(
            method,
            &format!("/api/v1/repos/alice/demo{path}"),
            Some(token),
            body.as_ref(),
        )
    }

    fn protect(&self, rule: Value) {
        let r = self.api(&self.alice, "POST", "/protections", Some(rule));
        assert_eq!(r.status, 200, "{}", r.body);
    }

    /// Commits a change on `branch` and pushes it to `url`.
    fn push(&self, url: &str, branch: &str, text: &str) {
        let w = self.dir.path();
        let exists = self
            .git
            .cmd(w)
            .args(["rev-parse", "--verify", "-q", branch])
            .output()
            .unwrap()
            .status
            .success();
        if exists {
            self.git.ok(w, &["switch", "-q", branch]);
        } else {
            self.git.ok(w, &["switch", "-q", "-c", branch, "main"]);
        }
        std::fs::write(w.join("work.txt"), text).unwrap();
        self.git.ok(w, &["add", "."]);
        self.git.ok(w, &["commit", "-q", "-m", text]);
        self.git.ok(w, &["push", "-q", url, branch]);
    }

    /// Opens a merge request from `branch` as `token`, asking bob to
    /// review, and waits for its first revision.
    fn open(&self, token: &str, branch: &str) -> i64 {
        let r = self.api(
            token,
            "POST",
            "/merge-requests",
            Some(json!({"source_branch": branch, "target_branch": "main",
                        "title": format!("Work on {branch}"), "reviewers": ["bob"]})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        self.wait_revisions(n, 1);
        n
    }

    fn mr(&self, n: i64) -> Value {
        self.api(&self.alice, "GET", &format!("/merge-requests/{n}"), None)
            .json()
    }

    fn wait_revisions(&self, n: i64, k: i64) {
        wait_until("the revision", || self.mr(n)["revisions"] == k);
    }

    fn wait_attention(&self, n: i64, want: &str) {
        let start = std::time::Instant::now();
        while self.mr(n)["attention"] != want {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "not {want}: {}\n{}",
                self.mr(n),
                self.s.log()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn ready(&self, token: &str, n: i64) {
        let r = self.api(
            token,
            "PATCH",
            &format!("/merge-requests/{n}"),
            Some(json!({"draft": false})),
        );
        assert!(r.status < 300, "{}", r.body);
    }

    fn status(&self, n: i64, state: &str) {
        let head = self.mr(n)["head_oid"].as_str().unwrap().to_owned();
        let r = self.api(
            &self.carol,
            "POST",
            &format!("/statuses/{head}"),
            Some(json!({"state": state, "context": "ci"})),
        );
        assert!(r.status < 300, "{}", r.body);
    }

    /// The reasons of bob's notifications about merge request `n`.
    fn bob_heard(&self, n: i64) -> Vec<String> {
        settle(&self.s);
        let r = self.s.client().api(
            "GET",
            "/api/v1/notifications?all=true",
            Some(&self.bob),
            None,
        );
        assert_eq!(r.status, 200, "{}", r.body);
        r.json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["subject_type"] == "merge_request" && i["number"] == n)
            .map(|i| i["reason"].as_str().unwrap().to_owned())
            .collect()
    }

    fn bob_inbox_has(&self, n: i64) -> bool {
        settle(&self.s);
        let mut bob = self.s.login("bob", PASSWORD);
        bob.get("/-/review-requests")
            .body
            .contains(&format!("merge-requests/{n}\""))
    }

    fn requirement(&self, n: i64, key: &str) -> Option<Value> {
        self.api(
            &self.alice,
            "GET",
            &format!("/merge-requests/{n}/mergeability"),
            None,
        )
        .json()["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .cloned()
    }
}

#[test]
fn conf_agent_09_bot_work_reaches_reviewers_once_ready_and_green() {
    let t = Setup::new();
    t.protect(json!({"pattern": "main", "required_approvals": 1,
                     "required_status_contexts": ["ci"]}));
    let bot_url = t.bot_url.clone();
    t.push(&bot_url, "agent-work", "first try");
    let n = t.open(&t.bot, "agent-work");
    assert_eq!(t.mr(n)["attention"], "iterating");
    let r = t
        .requirement(n, "iterating")
        .expect("an iterating requirement");
    assert_eq!(r["state"], "unmet");
    assert_eq!(r["fix_action"], "Mark it as ready");

    // Iterations: requested, but nobody is asked.
    for (k, text) in [(2, "second try"), (3, "third try")] {
        t.push(&bot_url, "agent-work", text);
        t.wait_revisions(n, k);
    }
    t.status(n, "failure");
    assert!(t.bob_heard(n).is_empty(), "{:?}", t.bob_heard(n));
    assert!(!t.bob_inbox_has(n));
    let requested = t.s.db.query_one(&format!(
        "SELECT count(*)::text FROM mr_reviewers r JOIN merge_requests m ON m.id = r.mr_id
          JOIN users u ON u.id = r.user_id JOIN namespaces s ON s.id = u.namespace_id
          WHERE m.number = {n} AND r.state = 'requested' AND s.name = 'bob'"
    ));
    assert_eq!(requested, "1", "bob is recorded as a requested reviewer");

    // Ready with a failing check: it waits, still quietly.
    t.ready(&t.bot, n);
    assert_eq!(t.mr(n)["attention"], "waiting_for_checks");
    assert!(t.requirement(n, "iterating").is_none());
    t.status(n, "failure");
    settle(&t.s);
    assert_eq!(t.mr(n)["attention"], "waiting_for_checks");
    assert!(t.bob_heard(n).is_empty());
    assert!(!t.bob_inbox_has(n));

    // A new revision waits for its own head's checks.
    t.push(&bot_url, "agent-work", "fourth try");
    t.wait_revisions(n, 4);
    t.status(n, "success");
    t.wait_attention(n, "ready");
    assert_eq!(t.bob_heard(n), ["mr_ready"]);
    assert!(t.bob_inbox_has(n));

    // Marking it ready again asks no one again (bob's notification stays
    // read), and later revisions don't make it iterate again.
    let marked = t.s.db.query_one(
        "WITH r AS (UPDATE notifications SET unread = false
                     WHERE user_id = (SELECT u.id FROM users u JOIN namespaces s ON s.id = u.namespace_id
                                       WHERE s.name = 'bob') RETURNING 1)
         SELECT count(*)::text FROM r",
    );
    assert_ne!(marked, "0", "bob has a notification to mark read");
    t.ready(&t.bot, n);
    assert_eq!(t.bob_heard(n), ["mr_ready"]);
    let unread = || {
        t.s.db.query_one(&format!(
            "SELECT count(*)::text FROM notifications o JOIN merge_requests m ON m.id = o.subject_id
              JOIN users u ON u.id = o.user_id JOIN namespaces s ON s.id = u.namespace_id
              WHERE m.number = {n} AND o.unread AND s.name = 'bob'"
        ))
    };
    assert_eq!(unread(), "0");
    t.push(&bot_url, "agent-work", "fifth try");
    t.wait_revisions(n, 5);
    settle(&t.s);
    assert_eq!(t.mr(n)["attention"], "ready");
    assert!(t.requirement(n, "iterating").is_none());
    let released = t
        .s
        .db
        .query_one("SELECT count(*)::text FROM timeline_events WHERE kind = 'attention_released'");
    assert_eq!(released, "1");

    // Every iteration stays its own revision.
    let revs = t.api(
        &t.alice,
        "GET",
        &format!("/merge-requests/{n}/revisions"),
        None,
    );
    assert_eq!(revs.status, 200, "{}", revs.body);
    assert_eq!(
        revs.json().as_array().map(Vec::len),
        Some(5),
        "{}",
        revs.body
    );
    let r = t.api(
        &t.alice,
        "GET",
        &format!("/merge-requests/{n}/interdiff?from=2&to=3"),
        None,
    );
    assert_eq!(r.status, 200, "{}", r.body);
}

#[test]
fn conf_agent_09_only_bots_iterate_and_checks_can_be_skipped() {
    let t = Setup::new();
    // A person's merge request is ready at once.
    let alice_url = t.s.url(&DEMO);
    t.push(&alice_url, "by-alice", "a person's work");
    let human = t.open(&t.alice, "by-alice");
    assert_eq!(t.mr(human)["attention"], "ready");
    assert!(t.requirement(human, "iterating").is_none());
    assert_eq!(t.bob_heard(human), ["review_requested"]);

    // Without the wait, marking it ready (here by a person) asks at once,
    // even though the required check never ran.
    t.protect(json!({"pattern": "main", "required_approvals": 1,
                     "required_status_contexts": ["ci"], "ready_waits_for_checks": false}));
    let rule = t.api(&t.alice, "GET", "/protections", None);
    assert!(
        rule.body.contains("\"ready_waits_for_checks\":false"),
        "{}",
        rule.body
    );
    let bot_url = t.bot_url.clone();
    t.push(&bot_url, "agent-quick", "quick work");
    let n = t.open(&t.bot, "agent-quick");
    assert_eq!(t.mr(n)["attention"], "iterating");
    assert!(t.bob_heard(n).is_empty());
    // Someone who can't edit the merge request can't mark it ready.
    t.s.create_user("erin", PASSWORD, false);
    let erin = {
        let mut c = t.s.login("erin", PASSWORD);
        slath_tests::http::create_token(&mut c, "erin", &["repo:read", "mrs:write"])
    };
    let r = t.api(
        &erin,
        "PATCH",
        &format!("/merge-requests/{n}"),
        Some(json!({"draft": false})),
    );
    assert!(r.status >= 400, "{}", r.body);
    assert_eq!(t.mr(n)["attention"], "iterating");
    t.ready(&t.alice, n);
    assert_eq!(t.mr(n)["attention"], "ready");
    assert_eq!(t.bob_heard(n), ["mr_ready"]);

    // A bot's draft stays quiet until it's ready, then waits for nothing
    // more than a person's would.
    t.push(&bot_url, "agent-draft", "draft work");
    let r = t.api(
        &t.bot,
        "POST",
        "/merge-requests",
        Some(
            json!({"source_branch": "agent-draft", "target_branch": "main",
                    "title": "Drafted", "draft": true}),
        ),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let d = r.json()["number"].as_i64().unwrap();
    t.wait_revisions(d, 1);
    assert_eq!(t.mr(d)["attention"], "iterating");
    t.ready(&t.bot, d);
    let m = t.mr(d);
    assert_eq!(
        (m["draft"].clone(), m["attention"].clone()),
        (json!(false), json!("ready"))
    );
}
