// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Origin labels (`conf_agent_12_*`): user-generated text in API responses
//! and webhooks carries `untrusted_content` and who wrote it, as they stood
//! when writing. (The schema test in `crates/http/tests/openapi.rs` checks
//! that no response field is left out.)

use serde_json::{Value, json};
use slath_tests::server::{DEMO, PASSWORD, TestServer};

const LOCAL: &str = "[webhooks]\nallow_http = true\nallow_networks = [\"127.0.0.0/8\"]\n";

fn token(s: &TestServer, user: &str) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(
        &mut c,
        user,
        &["repo:write", "issues:write", "mrs:write", "user:write"],
    )
}

fn bot(s: &TestServer, repo: &str) -> String {
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            "alice",
            "--name",
            "alice-agent",
            "--token-scopes",
            "repo:write,issues:write,mrs:write,statuses:write",
            "--repo",
            repo,
            "--output",
            "json",
        ],
        None,
    );
    assert!(ok, "{out}{err}");
    serde_json::from_str::<Value>(&out).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn api(s: &TestServer, t: &str, method: &str, path: &str, body: Option<Value>) -> Value {
    let r = s.client().api(method, path, Some(t), body.as_ref());
    assert!(r.status < 300, "{method} {path}: {} {}", r.status, r.body);
    if r.body.is_empty() {
        Value::Null
    } else {
        r.json()
    }
}

/// The origin of an object, checking that it's marked untrusted.
fn origin(v: &Value) -> &str {
    assert_eq!(v["untrusted_content"], true, "{v}");
    v["origin"].as_str().unwrap()
}

#[test]
fn conf_agent_12_origins_follow_the_author_when_writing() {
    let s = TestServer::start_full(&[DEMO], LOCAL);
    for u in ["bob", "carol"] {
        s.create_user(u, PASSWORD, false);
    }
    let mut web = s.login("alice", PASSWORD);
    slath_tests::http::create_repo(&mut web, "alice", "open", "public");
    let alice = token(&s, "alice");
    let bot = bot(&s, "alice/open");
    let (bob, carol) = (token(&s, "bob"), token(&s, "carol"));
    let repo = "/api/v1/repos/alice/open";
    api(
        &s,
        &alice,
        "PUT",
        &format!("{repo}/collaborators/bob"),
        Some(json!({"role": "read"})),
    );
    api(
        &s,
        &alice,
        "PUT",
        &format!("{repo}/collaborators/alice-agent"),
        Some(json!({"role": "maintain"})),
    );
    let hook = api(
        &s,
        &alice,
        "POST",
        &format!("{repo}/webhooks"),
        Some(json!({"url": "http://127.0.0.1:9/hook", "events": ["issue", "issue_comment"]})),
    );

    // Issues: an outside contributor's, edited by a writer, stays theirs;
    // a member's, edited by a bot, becomes a bot's.
    let open = |t: &str, title: &str| {
        api(
            &s,
            t,
            "POST",
            &format!("{repo}/issues"),
            Some(json!({"title": title, "body": "text"})),
        )
    };
    let i1 = open(&carol, "From an outsider");
    assert_eq!(origin(&i1), "outside_contributor");
    let n1 = i1["number"].as_i64().unwrap();
    let i1 = api(
        &s,
        &alice,
        "PATCH",
        &format!("{repo}/issues/{n1}"),
        Some(json!({"body": "tidied"})),
    );
    assert_eq!(origin(&i1), "outside_contributor");
    let i2 = open(&bob, "From a member");
    assert_eq!(origin(&i2), "member");
    let n2 = i2["number"].as_i64().unwrap();
    let i2 = api(
        &s,
        &bot,
        "PATCH",
        &format!("{repo}/issues/{n2}"),
        Some(json!({"body": "rewritten"})),
    );
    assert_eq!(origin(&i2), "bot");
    // Closing changes no text, so no origin.
    let i2 = api(
        &s,
        &alice,
        "PATCH",
        &format!("{repo}/issues/{n2}"),
        Some(json!({"state": "closed"})),
    );
    assert_eq!(origin(&i2), "bot");

    // Comments by each kind of author.
    for t in [&alice, &bob, &carol, &bot] {
        api(
            &s,
            t,
            "POST",
            &format!("{repo}/issues/{n1}/comments"),
            Some(json!({"body": "a comment"})),
        );
    }
    // A later role leaves recorded origins alone.
    api(
        &s,
        &alice,
        "PUT",
        &format!("{repo}/collaborators/carol"),
        Some(json!({"role": "write"})),
    );
    api(
        &s,
        &carol,
        "POST",
        &format!("{repo}/issues/{n1}/comments"),
        Some(json!({"body": "now a writer"})),
    );
    let comments = api(
        &s,
        &alice,
        "GET",
        &format!("{repo}/issues/{n1}/comments"),
        None,
    );
    let got: Vec<(&str, &str)> = comments["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["author"].as_str().unwrap(), origin(c)))
        .collect();
    assert_eq!(
        got,
        [
            ("alice", "writer"),
            ("bob", "member"),
            ("carol", "outside_contributor"),
            ("alice-agent", "bot"),
            ("carol", "writer"),
        ]
    );

    // Labels, milestones, the repository's description, search hits.
    let l = api(
        &s,
        &bot,
        "POST",
        &format!("{repo}/labels"),
        Some(json!({"name": "agent", "color": "336699"})),
    );
    assert_eq!(origin(&l), "bot");
    let l = api(
        &s,
        &alice,
        "PATCH",
        &format!("{repo}/labels/agent"),
        Some(json!({"color": "112233"})),
    );
    assert_eq!(origin(&l), "bot", "a new color changes no text");
    let m = api(
        &s,
        &alice,
        "POST",
        &format!("{repo}/milestones"),
        Some(json!({"title": "v1"})),
    );
    assert_eq!(origin(&m), "writer");
    api(
        &s,
        &alice,
        "PATCH",
        repo,
        Some(json!({"description": "Open for everyone"})),
    );
    assert_eq!(origin(&api(&s, &carol, "GET", repo, None)), "writer");
    let hits = api(&s, &carol, "GET", "/api/v1/search/issues?q=outsider", None);
    assert_eq!(origin(&hits["items"][0]), "outside_contributor");

    // Webhooks carry the same markers.
    let payload = s.db.query_one(&format!(
        "SELECT payload::text FROM webhook_deliveries WHERE webhook_id = '{}' AND event = 'issue_comment'
          ORDER BY created_at LIMIT 1",
        hook["id"].as_str().unwrap()
    ));
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(origin(&payload["issue"]), "outside_contributor");
    assert_eq!(origin(&payload["comment"]), "writer");
}

#[test]
fn conf_agent_12_organization_members_and_bot_work() {
    let s = TestServer::start_full(&[DEMO], LOCAL);
    s.create_user("dave", PASSWORD, false);
    let mut web = s.login("alice", PASSWORD);
    let r = slath_tests::http::submit(
        &mut web,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", "acme"), ("display_name", "")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    slath_tests::http::create_repo(&mut web, "acme", "lib", "public");
    let alice = token(&s, "alice");
    api(
        &s,
        &alice,
        "PUT",
        "/api/v1/orgs/acme/members/dave",
        Some(json!({"role": "member"})),
    );
    let dave = token(&s, "dave");
    let i = api(
        &s,
        &dave,
        "POST",
        "/api/v1/repos/acme/lib/issues",
        Some(json!({"title": "A member's issue", "body": ""})),
    );
    assert_eq!(origin(&i), "member");

    // A bot's merge request, status, and review.
    let bot = bot(&s, "acme/lib");
    api(
        &s,
        &alice,
        "PUT",
        "/api/v1/repos/acme/lib/collaborators/alice-agent",
        Some(json!({"role": "write"})),
    );
    let git = slath_tests::server::GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "lib\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    let url = format!("http://git:{bot}@{}/acme/lib.git", s.addr);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "-c", "topic"]);
    std::fs::write(w.join("README"), "lib, improved\n").unwrap();
    git.ok(w, &["commit", "-qam", "improve"]);
    git.ok(w, &["push", "-q", &url, "topic"]);
    let m = api(
        &s,
        &bot,
        "POST",
        "/api/v1/repos/acme/lib/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Improve"})),
    );
    assert_eq!(origin(&m), "bot");
    let n = m["number"].as_i64().unwrap();
    let start = std::time::Instant::now();
    let head = loop {
        let m = api(
            &s,
            &alice,
            "GET",
            &format!("/api/v1/repos/acme/lib/merge-requests/{n}"),
            None,
        );
        if let Some(h) = m["head_oid"].as_str() {
            break h.to_owned();
        }
        assert!(start.elapsed().as_secs() < 30, "no revision");
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let st = api(
        &s,
        &bot,
        "POST",
        &format!("/api/v1/repos/acme/lib/statuses/{head}"),
        Some(json!({"state": "success", "context": "ci", "description": "All good"})),
    );
    assert_eq!(origin(&st), "bot");
    api(
        &s,
        &dave,
        "POST",
        &format!("/api/v1/repos/acme/lib/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "comment", "body": "Looks fine"})),
    );
    let reviews = api(
        &s,
        &alice,
        "GET",
        &format!("/api/v1/repos/acme/lib/merge-requests/{n}/reviews"),
        None,
    );
    assert_eq!(origin(&reviews[0]), "member");
}
