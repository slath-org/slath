// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stopping bots (AGENT-01): a stop takes effect at once (tokens, SSH keys,
//! and pushes), only a bot's managers and site administrators can stop and
//! re-enable it, re-enabling brings no credentials back, and both are
//! audited.

use serde_json::json;
use slath_tests::contract;
use slath_tests::http::create_token;
use slath_tests::server::{GitClient, TestServer};

const PW: &str = "correct horse battery staple";

/// alice with the repository alice/app (one commit on main) and her bot
/// alice-ci, with a write token for it and an SSH key; returns
/// (alice's token, the bot's token).
fn setup(s: &TestServer) -> (String, String) {
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    slath_tests::http::create_repo(&mut a, "alice", "app", "private");
    let ta = create_token(&mut a, "t", &["user:write", "repo:write"]);
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            "alice",
            "--name",
            "alice-ci",
            "--token-scopes",
            "repo:write,user:read",
            "--repo",
            "alice/app",
            "--output",
            "json",
        ],
        None,
    );
    assert!(ok, "{out}{err}");
    let bot = serde_json::from_str::<serde_json::Value>(&out).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut api = s.client();
    let r = api.api(
        "PUT",
        "/api/v1/repos/alice/app/collaborators/alice-ci",
        Some(&ta),
        Some(&json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let dir = tempfile::tempdir().unwrap();
    let (_, public) = slath_tests::sshd::keygen(dir.path(), "ci");
    let r = api.api(
        "POST",
        "/api/v1/bots/alice-ci/keys",
        Some(&ta),
        Some(&json!({"title": "ci", "key": public})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    (ta, bot)
}

fn push_url(s: &TestServer, token: &str) -> String {
    format!(
        "http://git:{token}@{}/alice/app.git",
        s.base().trim_start_matches("http://")
    )
}

/// Pushes a new commit to `branch` with `token`; true if it was accepted.
fn push(s: &TestServer, token: &str, branch: &str) -> bool {
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    git.ok(w, &["init", "-q", "-b", branch]);
    std::fs::write(w.join("f.txt"), branch).unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", branch]);
    git.cmd(w)
        .args(["push", "-q", &push_url(s, token), branch])
        .status()
        .unwrap()
        .success()
}

fn audit_count(s: &TestServer, action: &str) -> String {
    s.db.query_one(&format!(
        "SELECT count(*)::text FROM audit_events WHERE action = '{action}'"
    ))
}

#[test]
fn conf_agent_01_a_stop_takes_effect_at_once() {
    let s = TestServer::start_with(&[]);
    let (ta, bot) = setup(&s);
    let mut api = s.client();
    assert_eq!(api.api("GET", "/api/v1/user", Some(&bot), None).status, 200);
    assert!(push(&s, &bot, "before"));

    let r = api.api("POST", "/api/v1/bots/alice-ci/stop", Some(&ta), None);
    assert_eq!(r.status, 200, "{}", r.body);
    contract::check("POST", "/api/v1/bots/{name}/stop", &r);
    let v = r.json();
    assert_eq!(v["changed"], true);
    assert_eq!(v["tokens_revoked"], 1);
    assert_eq!(v["keys_removed"], 1);
    assert_eq!(v["bot"]["state"], "stopped");
    assert!(v["bot"]["stopped_at"].is_string());

    // Its token, its key, and its pushes stop working.
    assert_eq!(api.api("GET", "/api/v1/user", Some(&bot), None).status, 401);
    assert!(!push(&s, &bot, "after"));
    let keys = api.api("GET", "/api/v1/bots/alice-ci/keys", Some(&ta), None);
    assert_eq!(keys.json()["items"], json!([]));
    assert_eq!(audit_count(&s, "bot.stop"), "1");
    // Stopping again changes nothing and isn't audited.
    let r = api.api("POST", "/api/v1/bots/alice-ci/stop", Some(&ta), None);
    assert_eq!(r.json()["changed"], false);
    assert_eq!(audit_count(&s, "bot.stop"), "1");

    // Re-enabled, it's active without credentials: the old token stays dead.
    let r = api.api("POST", "/api/v1/bots/alice-ci/start", Some(&ta), None);
    assert_eq!(r.status, 200, "{}", r.body);
    contract::check("POST", "/api/v1/bots/{name}/start", &r);
    assert_eq!(r.json()["state"], "active");
    assert_eq!(r.json()["stopped_at"], json!(null));
    assert_eq!(api.api("GET", "/api/v1/user", Some(&bot), None).status, 401);
    let tokens = api.api("GET", "/api/v1/bots/alice-ci/tokens", Some(&ta), None);
    assert!(
        tokens.json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["revoked"] == true),
        "{}",
        tokens.body
    );
    assert_eq!(audit_count(&s, "bot.start"), "1");
}

#[test]
fn conf_agent_01_only_managers_and_admins_stop_and_start() {
    let s = TestServer::start_with(&[]);
    let (ta, bot) = setup(&s);
    s.create_user("bob", PW, false);
    s.create_user("sysop", PW, true);
    let mut b = s.login("bob", PW);
    let tb = create_token(&mut b, "t", &["user:write"]);
    let mut r = s.login("sysop", PW);
    let admin = create_token(&mut r, "t", &["admin", "user:write"]);
    let read_only = create_token(&mut s.login("alice", PW), "ro", &["user:read"]);
    let mut api = s.client();
    let stop = |api: &mut slath_tests::http::Client, token: &str| {
        api.api("POST", "/api/v1/bots/alice-ci/stop", Some(token), None)
            .status
    };
    // Others don't see the bot, the bot can't stop itself, and reading
    // isn't enough.
    assert_eq!(stop(&mut api, &tb), 404);
    assert_eq!(stop(&mut api, &bot), 404);
    assert_eq!(stop(&mut api, &read_only), 404);
    assert_eq!(audit_count(&s, "bot.stop"), "0");
    // A site administrator can stop any bot, and re-enable it.
    assert_eq!(stop(&mut api, &admin), 200);
    let r = api.api("POST", "/api/v1/bots/alice-ci/start", Some(&admin), None);
    assert_eq!(r.status, 200, "{}", r.body);

    // A bot an administrator suspended can't be re-enabled by its owner.
    s.db.execute("UPDATE users SET state = 'suspended' WHERE id = (SELECT u.id FROM users u JOIN namespaces n ON n.id = u.namespace_id WHERE n.name = 'alice-ci')");
    let r = api.api("POST", "/api/v1/bots/alice-ci/start", Some(&ta), None);
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(audit_count(&s, "bot.start"), "1");
}
