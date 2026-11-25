// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Work orders through the API: people with write access delegate an
//! issue or a merge request to a bot with write access; readers read the
//! orders, and the creator, writers, and the bot's managers stop them.
//! Every order keeps its events.

use serde_json::{Value, json};
use slath_tests::server::{PASSWORD, TestServer};

pub struct World {
    pub s: TestServer,
    pub alice: String,
    pub bob: String,
    pub carol: String,
    /// The bot's standing token (`repo:write`, `issues:write`, and
    /// `mrs:write` on alice/demo).
    pub agent: String,
}

pub fn token(s: &TestServer, user: &str, scopes: &[&str]) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, user, scopes)
}

/// Creates a bot of `owner` with `slath admin create-bot` and returns its
/// token.
pub fn bot(s: &TestServer, owner: &str, name: &str, scopes: &str) -> String {
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            owner,
            "--name",
            name,
            "--token-scopes",
            scopes,
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

impl World {
    /// alice/demo (private) with bob a writer, carol a reader, and alice's
    /// bot `agent` a writer; issue #1 is open.
    pub fn start() -> Self {
        let s = TestServer::start();
        for u in ["bob", "carol", "dave"] {
            s.create_user(u, PASSWORD, false);
        }
        let all = ["repo:write", "issues:write", "mrs:write"];
        let w = Self {
            alice: token(&s, "alice", &all),
            bob: token(&s, "bob", &all),
            carol: token(&s, "carol", &all),
            agent: bot(&s, "alice", "agent", "repo:write,issues:write,mrs:write"),
            s,
        };
        for (who, role) in [("bob", "write"), ("carol", "read"), ("agent", "write")] {
            let r = w.api(
                &w.alice,
                "PUT",
                &format!("/collaborators/{who}"),
                Some(json!({"role": role})),
            );
            assert!(r.status < 300, "{}", r.body);
        }
        let r = w.api(
            &w.alice,
            "POST",
            "/issues",
            Some(json!({"title": "Fix the parser"})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        w
    }

    pub fn api(
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

    /// Delegates issue #1 to `agent` as `token`.
    pub fn delegate(&self, token: &str, extra: Value) -> slath_tests::http::Resp {
        let mut body = json!({"target": {"issue": 1}, "bot": "agent", "goal": "Fix the parser"});
        if let (Value::Object(b), Value::Object(e)) = (&mut body, extra) {
            b.extend(e);
        }
        self.api(token, "POST", "/work-orders", Some(body))
    }
}

#[test]
fn conf_agent_13_writers_delegate_issues_to_bots_with_write_access() {
    let w = World::start();
    let r = w.delegate(
        &w.bob,
        json!({"instructions": "Keep the API stable.", "scope": ["src/parser/**"],
               "required_checks": ["ci/test"], "reviewers": ["carol"]}),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let o = r.json();
    assert_eq!(o["number"], 1);
    assert_eq!(o["target"]["issue"], 1);
    assert_eq!(o["target"]["title"], "Fix the parser");
    assert_eq!(o["bot"], "agent");
    assert_eq!(o["creator"], "bob");
    assert_eq!(o["branch_prefix"], "wo/1/");
    assert_eq!(o["scope"], json!(["src/parser/**"]));
    assert_eq!(o["max_revisions"], 20);
    assert_eq!(o["max_open_mrs"], 1);
    assert_eq!(o["required_checks"], json!(["ci/test"]));
    assert_eq!(o["reviewers"], json!(["carol"]));
    assert_eq!(o["state"], "open");
    assert_eq!(o["untrusted_content"], true);
    assert_eq!(o["origin"], "writer");

    // One open order per target.
    let r = w.delegate(&w.alice, json!({}));
    assert_eq!(r.status, 409, "{}", r.body);

    // Readers read it; others don't find it.
    let r = w.api(&w.carol, "GET", "/work-orders/1", None);
    assert_eq!(r.status, 200, "{}", r.body);
    let list = w
        .api(&w.carol, "GET", "/work-orders?state=open", None)
        .json();
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let dave = token(&w.s, "dave", &["repo:write"]);
    assert_eq!(w.api(&dave, "GET", "/work-orders/1", None).status, 404);
    assert_eq!(w.api(&dave, "GET", "/work-orders", None).status, 404);
    assert_eq!(w.api(&w.carol, "GET", "/work-orders/9", None).status, 404);

    // Readers can't delegate, strangers don't find the repository, and
    // bots can't delegate at all.
    for (who, status) in [(&w.carol, 403), (&dave, 404), (&w.agent, 403)] {
        let r = w.api(
            who,
            "POST",
            "/work-orders",
            Some(json!({"target": {"issue": 1}, "bot": "agent", "goal": "x"})),
        );
        assert_eq!(r.status, status, "{}", r.body);
    }

    // The audit log names the order.
    assert_eq!(
        w.s.db.query_one(
            "SELECT count(*)::text FROM audit_events WHERE action = 'work_order.create'"
        ),
        "1"
    );
}

#[test]
fn conf_agent_13_orders_are_checked_before_they_are_stored() {
    let w = World::start();
    // A bot without write access, a person, an unknown name.
    bot(&w.s, "alice", "reader-bot", "repo:read");
    let r = w.api(
        &w.alice,
        "PUT",
        "/collaborators/reader-bot",
        Some(json!({"role": "read"})),
    );
    assert!(r.status < 300, "{}", r.body);
    for b in ["reader-bot", "carol", "nobody"] {
        let r = w.delegate(&w.alice, json!({"bot": b}));
        assert_eq!(r.status, 422, "{b}: {}", r.body);
        assert!(r.body.contains("\"field\":\"bot\""), "{b}: {}", r.body);
    }
    // Fields out of range, each named.
    for (extra, field) in [
        (json!({"goal": " "}), "/goal"),
        (json!({"max_revisions": 0}), "/max_revisions"),
        (json!({"max_revisions": 201}), "/max_revisions"),
        (json!({"max_open_mrs": 11}), "/max_open_mrs"),
        (json!({"expires_in_days": 31}), "/expires_in_days"),
        (json!({"scope": ["/abs"]}), "/scope"),
        (json!({"reviewers": ["dave"]}), "/reviewers"),
        (json!({"target": {"issue": 7}}), "/target"),
        // Both targets, or an unknown field, isn't a work order at all.
        (json!({"target": {"issue": 1, "merge_request": 1}}), "body"),
        (json!({"budget": 3}), "body"),
    ] {
        let r = w.delegate(&w.alice, extra.clone());
        assert_eq!(r.status, 422, "{extra}: {}", r.body);
        assert!(
            r.body.contains(&format!("\"field\":\"{field}\"")),
            "{extra}: {}",
            r.body
        );
    }
    // A secret in the instructions is refused with its place.
    let secret = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
    let r = w.delegate(
        &w.alice,
        json!({"instructions": format!("Deploy\nwith {secret}")}),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("/instructions"), "{}", r.body);
    assert!(r.body.contains("Line 2"), "{}", r.body);
    assert!(!r.body.contains(&secret));
    // A closed issue can't be delegated.
    let r = w.api(
        &w.alice,
        "PATCH",
        "/issues/1",
        Some(json!({"state": "closed"})),
    );
    assert!(r.status < 300, "{}", r.body);
    assert_eq!(w.delegate(&w.alice, json!({})).status, 409);
    assert_eq!(
        w.s.db.query_one("SELECT count(*)::text FROM work_orders"),
        "0"
    );
}

#[test]
fn conf_agent_13_orders_are_stopped_by_their_creator_writers_and_bot_managers() {
    let w = World::start();
    assert_eq!(w.delegate(&w.bob, json!({})).status, 201);
    // A reader can't stop it.
    let r = w.api(&w.carol, "POST", "/work-orders/1/stop", None);
    assert_eq!(r.status, 403, "{}", r.body);
    // The bot's owner can.
    let r = w.api(&w.alice, "POST", "/work-orders/1/stop", None);
    assert_eq!(r.status, 200, "{}", r.body);
    let o = r.json();
    assert_eq!(o["state"], "ended");
    assert_eq!(o["end_reason"], "stopped");
    assert_eq!(o["ended_by"], "alice");
    // Once ended, it stays ended, and the issue can be delegated again.
    assert_eq!(
        w.api(&w.alice, "POST", "/work-orders/1/stop", None).status,
        409
    );
    let r = w.delegate(&w.bob, json!({}));
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["number"], 2);

    // The events record what happened, for readers.
    let r = w.api(&w.carol, "GET", "/work-orders/1/events", None);
    assert_eq!(r.status, 200, "{}", r.body);
    let events: Vec<(String, String)> = r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["action"].as_str().unwrap().to_owned(),
                e["policy"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect();
    assert_eq!(
        events,
        [
            ("work_order.create".to_owned(), String::new()),
            ("work_order.end".to_owned(), "stopped".to_owned()),
        ]
    );
    // An order past its time ends as expired when it's next read.
    w.s.db.execute(
        "UPDATE work_orders SET expires_at = now() - interval '1 minute' WHERE number = 2",
    );
    let o = w.api(&w.carol, "GET", "/work-orders/2", None).json();
    assert_eq!(o["end_reason"], "expired", "{o}");
}

#[test]
fn conf_agent_13_bots_claim_their_orders_with_a_credential_that_reads_nothing_else() {
    let w = World::start();
    let claimer = bot(&w.s, "alice", "claimer", "work_orders:claim");
    let r = w.api(
        &w.alice,
        "PUT",
        "/collaborators/claimer",
        Some(json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    assert_eq!(w.delegate(&w.bob, json!({"bot": "claimer"})).status, 201);

    // The claim credential finds and reads its order, and nothing else.
    let mine =
        w.s.client()
            .api("GET", "/api/v1/user/work-orders", Some(&claimer), None);
    assert_eq!(mine.status, 200, "{}", mine.body);
    assert_eq!(mine.json()[0]["number"], 1, "{}", mine.body);
    assert_eq!(w.api(&claimer, "GET", "/work-orders/1", None).status, 200);
    for path in ["", "/issues", "/issues/1", "/branches", "/work-orders"] {
        let r = w.api(&claimer, "GET", path, None);
        assert_eq!(r.status, 403, "{path}: {}", r.body);
    }
    // A person's token has no orders to pull.
    let r =
        w.s.client()
            .api("GET", "/api/v1/user/work-orders", Some(&w.bob), None);
    assert_eq!(r.status, 403, "{}", r.body);

    // Claiming gives a task token, never kept for replay; claiming again
    // renews it and revokes the first.
    let claim = |key: &str| {
        w.s.client().api_with(
            "POST",
            "/api/v1/repos/alice/demo/work-orders/1/claim",
            Some(&claimer),
            None,
            &[("idempotency-key", key)],
        )
    };
    let first = claim("one");
    assert_eq!(first.status, 201, "{}", first.body);
    assert_eq!(first.header("cache-control"), Some("no-store"));
    let t = first.json()["token"].as_str().unwrap().to_owned();
    assert!(t.starts_with("slath_tsk_"), "{t}");
    let again = claim("one");
    assert_eq!(again.status, 201, "{}", again.body);
    assert_ne!(again.json()["token"].as_str().unwrap(), t);
    assert_eq!(
        w.s.db.query_one(
            "SELECT count(*)::text FROM idempotency_keys k WHERE k::text LIKE '%slath_tsk_%'"
        ),
        "0"
    );
    assert_eq!(
        w.s.db.query_one(
            "SELECT count(*) FILTER (WHERE revoked_at IS NULL)::text || '/' || count(*)::text
               FROM task_tokens"
        ),
        "1/2"
    );
    let minutes = w.s.db.query_one(
        "SELECT round(extract(epoch FROM expires_at - now()) / 60)::text
           FROM task_tokens WHERE revoked_at IS NULL",
    );
    assert_eq!(minutes, "60");
    let o = w.api(&w.carol, "GET", "/work-orders/1", None).json();
    assert!(o["claimed_at"].is_string(), "{o}");

    // Another bot, even one with write access, doesn't find it to claim.
    let r = w.api(&w.agent, "POST", "/work-orders/1/claim", None);
    assert_eq!(r.status, 404, "{}", r.body);
    // An ended order can't be claimed.
    assert_eq!(
        w.api(&w.bob, "POST", "/work-orders/1/stop", None).status,
        200
    );
    let r = claim("two");
    assert_eq!(r.status, 409, "{}", r.body);
    let events = w.api(&w.carol, "GET", "/work-orders/1/events", None).json();
    let actions: Vec<&str> = events["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions,
        [
            "work_order.create",
            "work_order.claim",
            "work_order.claim",
            "work_order.end"
        ]
    );
}

/// alice/demo with `main`, the order's branch `wo/1/fix`, and alice's own
/// `topic`; issue #1 delegated to `claimer` (claim scope only, write
/// role) and claimed. Returns the claim token, the task token, the Git
/// client, and the working copy.
type Claimed = (
    String,
    String,
    slath_tests::server::GitClient,
    tempfile::TempDir,
);

fn claimed(w: &World) -> Claimed {
    claimed_with(w, json!({}))
}

/// [`claimed`], with `extra` fields for the order.
fn claimed_with(w: &World, extra: Value) -> Claimed {
    let claimer = bot(&w.s, "alice", "claimer", "work_orders:claim");
    let r = w.api(
        &w.alice,
        "PUT",
        "/collaborators/claimer",
        Some(json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let mut order = json!({"bot": "claimer"});
    if let (Value::Object(o), Value::Object(e)) = (&mut order, extra) {
        o.extend(e);
    }
    let r = w.delegate(&w.bob, order);
    assert_eq!(r.status, 201, "{}", r.body);
    let git = slath_tests::server::GitClient::new();
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let url = w.s.url(&slath_tests::server::DEMO);
    git.ok(p, &["init", "-q", "-b", "main"]);
    std::fs::write(p.join("README"), "demo\n").unwrap();
    git.ok(p, &["add", "-A"]);
    git.ok(p, &["commit", "-q", "-m", "init"]);
    for branch in ["wo/1/fix", "wo/1/other", "topic"] {
        git.ok(p, &["switch", "-q", "-C", branch, "main"]);
        std::fs::write(p.join(branch.replace('/', "-")), "x\n").unwrap();
        git.ok(p, &["add", "-A"]);
        git.ok(p, &["commit", "-q", "-m", branch]);
    }
    git.ok(
        p,
        &[
            "push",
            "-q",
            &url,
            "main",
            "wo/1/fix",
            "wo/1/other",
            "topic",
        ],
    );
    let task = w.api(&claimer, "POST", "/work-orders/1/claim", None).json()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    (claimer, task, git, dir)
}

fn events(w: &World, n: i32) -> Vec<(String, String, String)> {
    w.api(
        &w.alice,
        "GET",
        &format!("/work-orders/{n}/events?limit=100"),
        None,
    )
    .json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["action"].as_str().unwrap().to_owned(),
                e["decision"].as_str().unwrap().to_owned(),
                e["policy"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect()
}

#[test]
fn conf_agent_13_task_tokens_act_only_on_their_order() {
    let w = World::start();
    let r = w.api(
        &w.alice,
        "POST",
        "/issues",
        Some(json!({"title": "Someone else's issue"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let (_, task, _git, _dir) = claimed(&w);

    // It reads the repository, issues included, and nothing elsewhere.
    for path in ["", "/issues/2", "/branches"] {
        assert_eq!(w.api(&task, "GET", path, None).status, 200, "{path}");
    }
    let mut c = w.s.login("alice", PASSWORD);
    slath_tests::http::create_repo(&mut c, "alice", "open", "public");
    let r =
        w.s.client()
            .api("GET", "/api/v1/repos/alice/open", Some(&task), None);
    assert_eq!(r.status, 404, "{}", r.body);
    let r = w.s.client().api("GET", "/api/v1/repos", Some(&task), None);
    assert!(!r.body.contains("\"open\""), "{}", r.body);
    assert_eq!(
        w.s.client()
            .api("GET", "/api/v1/user", Some(&task), None)
            .status,
        403
    );

    // It talks on its issue only.
    let comment = |n: i32| {
        w.api(
            &task,
            "POST",
            &format!("/issues/{n}/comments"),
            Some(json!({"body": "On it."})),
        )
    };
    assert_eq!(comment(1).status, 201, "{}", comment(1).body);
    assert_eq!(comment(2).status, 403);
    // It opens a merge request from its branches only, one at a time.
    let open = |branch: &str| {
        w.api(
            &task,
            "POST",
            "/merge-requests",
            Some(json!({"source_branch": branch, "title": format!("From {branch}")})),
        )
    };
    let r = open("topic");
    assert_eq!(r.status, 403, "{}", r.body);
    let r = open("wo/1/fix");
    assert_eq!(r.status, 201, "{}", r.body);
    let mine = r.json()["number"].as_i64().unwrap();
    let r = open("wo/1/other");
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(
        r.body.contains("at most 1 open merge request"),
        "{}",
        r.body
    );
    // It edits and comments on its merge request, never someone else's,
    // and never merges.
    let r = w.api(
        &w.alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Alice's"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let theirs = r.json()["number"].as_i64().unwrap();
    let r = w.api(
        &task,
        "PATCH",
        &format!("/merge-requests/{mine}"),
        Some(json!({"title": "Fix the parser"})),
    );
    assert!(r.status < 300, "{}", r.body);
    for (n, status) in [(mine, 201), (theirs, 403)] {
        let r = w.api(
            &task,
            "POST",
            &format!("/merge-requests/{n}/comments"),
            Some(json!({"body": "Done."})),
        );
        assert_eq!(r.status, status, "!{n}: {}", r.body);
    }
    let r = w.api(
        &task,
        "PATCH",
        &format!("/merge-requests/{theirs}"),
        Some(json!({"title": "Mine now"})),
    );
    assert_eq!(r.status, 403, "{}", r.body);
    let r = w.api(
        &task,
        "POST",
        &format!("/merge-requests/{mine}/merge"),
        Some(json!({})),
    );
    assert_eq!(r.status, 403, "{}", r.body);
    // It reports statuses on its merge request's head only.
    let head = |n: i64| {
        w.api(&w.alice, "GET", &format!("/merge-requests/{n}"), None)
            .json()["head_oid"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    for (n, status) in [(mine, 201), (theirs, 403)] {
        let r = w.api(
            &task,
            "POST",
            &format!("/statuses/{}", head(n)),
            Some(json!({"state": "success", "context": "agent/tests"})),
        );
        assert_eq!(r.status, status, "!{n}: {}", r.body);
    }
    // Repository settings are out of reach.
    let r = w.api(
        &task,
        "PUT",
        "/collaborators/carol",
        Some(json!({"role": "admin"})),
    );
    assert_eq!(r.status, 403, "{}", r.body);

    // Every write it tried is an event of the order, with the rule that
    // refused it.
    let ev = events(&w, 1);
    for want in [
        ("POST /issues/{number}/comments", "allowed", ""),
        ("POST /issues/{number}/comments", "denied", "order.target"),
        ("POST /merge-requests", "denied", "order.prefix"),
        ("POST /merge-requests", "allowed", ""),
        ("POST /merge-requests", "denied", "order.budget.open_mrs"),
        ("POST /merge-requests/{number}/merge", "denied", "role"),
        ("POST /statuses/{oid}", "denied", "order.target"),
        ("PUT /collaborators/{user}", "denied", "role"),
    ] {
        assert!(
            ev.iter()
                .any(|(a, d, p)| (a.as_str(), d.as_str(), p.as_str()) == want),
            "{want:?} in {ev:?}"
        );
    }
}

#[test]
fn conf_agent_13_task_tokens_stop_with_their_order() {
    let w = World::start();
    let (claimer, task, _git, _dir) = claimed(&w);
    let works = |t: &str| w.api(t, "GET", "", None).status;
    assert_eq!(works(&task), 200);
    // Renewing revokes the old token.
    let renewed = w.api(&claimer, "POST", "/work-orders/1/claim", None).json()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(works(&task), 401);
    assert_eq!(works(&renewed), 200);
    // A freeze pauses it; unfreezing resumes it.
    for (cmd, status) in [("freeze", 401), ("unfreeze", 200)] {
        let (ok, out, err) = w.s.slath(&["admin", "bot", cmd], None);
        assert!(ok, "{out}{err}");
        assert_eq!(works(&renewed), status, "after {cmd}");
    }
    // Past its time, it stops.
    w.s.db
        .execute("UPDATE task_tokens SET expires_at = now() - interval '1 second'");
    assert_eq!(works(&renewed), 401);
    let fresh = w.api(&claimer, "POST", "/work-orders/1/claim", None).json()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(works(&fresh), 200);
    // Stopping the order ends it at once.
    assert_eq!(
        w.api(&w.bob, "POST", "/work-orders/1/stop", None).status,
        200
    );
    assert_eq!(works(&fresh), 401);
}

/// alice/demo's Git URL with `token` as the password.
fn url_with(w: &World, token: &str) -> String {
    let url = w.s.url(&slath_tests::server::DEMO);
    let (_, host) = url.split_once('@').unwrap();
    format!("http://git:{token}@{host}")
}

/// Waits until merge request `n` has `k` revisions.
fn wait_revisions(w: &World, n: i64, k: i64) {
    let start = std::time::Instant::now();
    while w
        .api(&w.alice, "GET", &format!("/merge-requests/{n}"), None)
        .json()["revisions"]
        != k
    {
        assert!(start.elapsed().as_secs() < 30, "no revision {k}");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[test]
fn conf_agent_13_git_and_jj_agents_push_only_the_orders_branches() {
    let w = World::start();
    let (_, task, git, dir) = claimed(&w);
    let url = url_with(&w, &task);
    let p = dir.path();

    // Git: its branches, and nothing else.
    git.ok(p, &["switch", "-q", "wo/1/fix"]);
    std::fs::write(p.join("parser"), "fixed\n").unwrap();
    git.ok(p, &["add", "-A"]);
    git.ok(p, &["commit", "-q", "-m", "Fix the parser"]);
    git.ok(
        p,
        &["push", "-q", &url, "wo/1/fix", "HEAD:refs/heads/wo/1/new"],
    );
    for refspec in [
        "+HEAD:refs/heads/topic",
        "+HEAD:refs/heads/main",
        "HEAD:refs/tags/v1",
    ] {
        let out = git.cmd(p).args(["push", &url, refspec]).output().unwrap();
        assert!(!out.status.success(), "{refspec} was pushed");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("may push only to wo/1/"), "{refspec}: {err}");
    }

    // jj: the same with a bookmark, nothing set up beyond the remote.
    for (version, bin) in crate::jj::jj_binaries() {
        let jj = crate::jj::Jj::new(&version, &bin);
        let d = tempfile::tempdir().unwrap();
        let j = d.path().join("j");
        jj.ok(d.path(), &["git", "clone", &url, j.to_str().unwrap()]);
        jj.ok(&j, &["new", "main", "-m", "Parser tests"]);
        std::fs::write(j.join("tests"), "ok\n").unwrap();
        let mine = format!("wo/1/jj-{}", version.replace(' ', "-"));
        jj.ok(&j, &["bookmark", "create", &mine, "-r", "@"]);
        jj.ok(&j, &["bookmark", "create", "jj-topic", "-r", "@"]);
        let ok = jj.run(&j, &["git", "push", "--allow-new", "--bookmark", &mine]);
        let ok = if ok.status.success() {
            ok
        } else {
            // Newer jj pushes new bookmarks without --allow-new.
            jj.run(&j, &["git", "push", "--bookmark", &mine])
        };
        assert!(
            ok.status.success(),
            "{version}: {}",
            String::from_utf8_lossy(&ok.stderr)
        );
        let bad = jj.run(
            &j,
            &["git", "push", "--allow-new", "--bookmark", "jj-topic"],
        );
        let bad = if String::from_utf8_lossy(&bad.stderr).contains("--allow-new") {
            jj.run(&j, &["git", "push", "--bookmark", "jj-topic"])
        } else {
            bad
        };
        assert!(!bad.status.success(), "{version}: jj-topic was pushed");
        assert!(
            String::from_utf8_lossy(&bad.stderr).contains("may push only to wo/1/"),
            "{version}: {}",
            String::from_utf8_lossy(&bad.stderr)
        );
    }

    // Revisions are budgeted over the order's merge requests.
    let r = w.api(
        &task,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "wo/1/fix", "title": "Fix the parser"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    wait_revisions(&w, n, 1);
    w.s.db
        .execute("UPDATE work_orders SET max_revisions = 2 WHERE number = 1");
    let amend = |msg: &str| {
        git.ok(p, &["commit", "-q", "--amend", "-m", msg]);
        git.cmd(p)
            .args(["push", "--force", &url, "wo/1/fix"])
            .output()
            .unwrap()
    };
    assert!(amend("Fix the parser, again").status.success());
    wait_revisions(&w, n, 2);
    let out = amend("Fix the parser, once more");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("work order 1 has used its 2 revisions"),
        "{err}"
    );

    let ev = events(&w, 1);
    for want in [
        ("git.push", "allowed", ""),
        ("git.push", "denied", "order.prefix"),
        ("git.push", "denied", "order.budget.revisions"),
    ] {
        assert!(
            ev.iter()
                .any(|(a, d, p)| (a.as_str(), d.as_str(), p.as_str()) == want),
            "{want:?} in {ev:?}"
        );
    }
}

#[test]
fn conf_agent_13_orders_end_when_their_work_ends() {
    let w = World::start();
    for title in ["Second", "Third"] {
        let r = w.api(&w.alice, "POST", "/issues", Some(json!({"title": title})));
        assert_eq!(r.status, 201, "{}", r.body);
    }
    let (_, task, _git, _dir) = claimed(&w);
    let state = |n: i32| {
        let o = w
            .api(&w.alice, "GET", &format!("/work-orders/{n}"), None)
            .json();
        (
            o["state"].as_str().unwrap().to_owned(),
            o["end_reason"].as_str().unwrap_or("").to_owned(),
        )
    };
    let open = || ("open".to_owned(), String::new());
    let ended = |why: &str| ("ended".to_owned(), why.to_owned());

    // Merged: its merge request merges, and none of its others is open.
    let r = w.api(
        &task,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "wo/1/fix", "title": "Fix the parser"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let mr = r.json()["number"].as_i64().unwrap();
    wait_revisions(&w, mr, 1);
    let head = w
        .api(&w.alice, "GET", &format!("/merge-requests/{mr}"), None)
        .json()["head_oid"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(state(1), open());
    // The bot marks its work ready, as any bot does.
    let r = w.api(
        &task,
        "PATCH",
        &format!("/merge-requests/{mr}"),
        Some(json!({"draft": false})),
    );
    assert!(r.status < 300, "{}", r.body);
    let start = std::time::Instant::now();
    while w
        .api(&w.alice, "GET", &format!("/merge-requests/{mr}"), None)
        .json()["attention"]
        != "ready"
    {
        assert!(start.elapsed().as_secs() < 30, "not ready");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let r = w.api(
        &w.alice,
        "POST",
        &format!("/merge-requests/{mr}/merge"),
        Some(
            json!({"strategy": "merge-commit", "expected_head_oid": head, "delete_source": false}),
        ),
    );
    assert!(r.status < 300, "{}", r.body);
    assert_eq!(state(1), ended("merged"));
    assert_eq!(w.api(&task, "GET", "", None).status, 401);

    // Closed: its issue closes.
    let delegate = |target: Value| {
        let r = w.api(
            &w.bob,
            "POST",
            "/work-orders",
            Some(json!({"target": target, "bot": "claimer", "goal": "Do it"})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        i32::try_from(r.json()["number"].as_i64().unwrap()).unwrap()
    };
    let second = delegate(json!({"issue": 2}));
    let r = w.api(
        &w.alice,
        "PATCH",
        "/issues/2",
        Some(json!({"state": "closed"})),
    );
    assert!(r.status < 300, "{}", r.body);
    assert_eq!(state(second), ended("closed"));

    // Closed: a follow-up's merge request closes.
    let r = w.api(
        &w.alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Alice's"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let theirs = r.json()["number"].as_i64().unwrap();
    let follow_up = delegate(json!({"merge_request": theirs}));
    let r = w.api(
        &w.alice,
        "PATCH",
        &format!("/merge-requests/{theirs}"),
        Some(json!({"state": "closed"})),
    );
    assert!(r.status < 300, "{}", r.body);
    assert_eq!(state(follow_up), ended("closed"));

    // Stopped: its bot is stopped.
    let third = delegate(json!({"issue": 3}));
    let (ok, out, err) = w.s.slath(&["admin", "bot", "stop", "claimer"], None);
    assert!(ok, "{out}{err}");
    assert_eq!(state(third), ended("stopped"));
}

#[test]
fn conf_agent_13_order_merge_requests_need_its_checks_and_ask_its_reviewers() {
    let w = World::start();
    let (_, task, _git, _dir) = claimed_with(
        &w,
        json!({"required_checks": ["agent/tests"], "reviewers": ["carol"]}),
    );
    let r = w.api(
        &task,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "wo/1/fix", "title": "Fix the parser"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    // carol is asked, in bob's name.
    assert_eq!(
        w.s.db.query_one(
            "SELECT un.name::text || ' by ' || bn.name::text FROM mr_reviewers r
               JOIN users u ON u.id = r.user_id JOIN namespaces un ON un.id = u.namespace_id
               JOIN users b ON b.id = r.requested_by JOIN namespaces bn ON bn.id = b.namespace_id"
        ),
        "carol by bob"
    );
    // The order's check is required, though no protection asks for it.
    let mergeability = |n: i64| {
        w.api(
            &w.alice,
            "GET",
            &format!("/merge-requests/{n}/mergeability"),
            None,
        )
        .json()
    };
    let m = mergeability(n);
    assert!(
        m["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["key"] == "statuses"
                && r["state"] != "met"
                && r.to_string().contains("agent/tests")),
        "{m}"
    );
    // A merge request outside any order needs nothing extra.
    let r = w.api(
        &w.alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Alice's"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let other = mergeability(r.json()["number"].as_i64().unwrap());
    assert!(!other.to_string().contains("agent/tests"), "{other}");
}

#[test]
fn conf_agent_13_work_orders_only_bots_write_only_through_orders() {
    let w = World::start();
    let alice_admin = token(&w.s, "alice", &["user:write"]);
    let set = |token: &str, on: bool| {
        w.s.client().api(
            "PATCH",
            "/api/v1/bots/agent",
            Some(token),
            Some(&json!({"work_orders_only": on})),
        )
    };
    // Only its managers change it.
    let bob_admin = token(&w.s, "bob", &["user:write"]);
    assert_eq!(set(&bob_admin, true).status, 404);
    let r = set(&alice_admin, true);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["work_orders_only"], true);

    // Its own token reads, but writes nothing, pushes included.
    let comment = || {
        w.api(
            &w.agent,
            "POST",
            "/issues/1/comments",
            Some(json!({"body": "Hi"})),
        )
        .status
    };
    assert_eq!(w.api(&w.agent, "GET", "/issues/1", None).status, 200);
    assert_eq!(comment(), 403);
    let git = slath_tests::server::GitClient::new();
    let dir = tempfile::tempdir().unwrap();
    git.ok(dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(dir.path().join("README"), "x\n").unwrap();
    git.ok(dir.path(), &["add", "-A"]);
    git.ok(dir.path(), &["commit", "-q", "-m", "init"]);
    let out = git
        .cmd(dir.path())
        .args(["push", &url_with(&w, &w.agent), "main"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "a work-orders-only bot pushed");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("works only through work orders"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `slath admin apply` sets it too; off, the bot writes again.
    let plan = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        plan.path(),
        "[[bots]]\nname = \"agent\"\nowner = \"alice\"\nwork_orders_only = false\n",
    )
    .unwrap();
    let (ok, out, err) =
        w.s.slath(&["admin", "apply", plan.path().to_str().unwrap()], None);
    assert!(ok, "{out}{err}");
    assert_eq!(comment(), 201);
    git.ok(dir.path(), &["push", "-q", &url_with(&w, &w.agent), "main"]);
}
