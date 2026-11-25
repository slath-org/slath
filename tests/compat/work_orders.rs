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
    /// The bot's standing token (`repo:write` on alice/demo).
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
            agent: bot(&s, "alice", "agent", "repo:write"),
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
        (
            json!({"target": {"issue": 1, "merge_request": 1}}),
            "/target",
        ),
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
