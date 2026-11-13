// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stopping bots (AGENT-01): a stop takes effect at once (tokens, SSH keys,
//! and pushes), only a bot's managers and site administrators can stop and
//! re-enable it, re-enabling brings no credentials back, and both are
//! audited. Freezing (AGENT-10): every credential of the frozen bots fails
//! until unfrozen, nothing is revoked, and only organization owners and
//! site administrators can freeze.

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

fn data_key(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/auth/tests/data/ssh")
        .join(name);
    std::fs::read_to_string(p).unwrap().trim().to_owned()
}

/// True if sshd would be offered the key with this fingerprint.
fn key_offered(s: &TestServer, fp: &str) -> bool {
    let sock = s.ipc_socket();
    let (ok, out, _) = s.slath(&["ssh-keys", "--socket", sock.to_str().unwrap(), fp], None);
    ok && out.contains("ssh-serve")
}

#[test]
fn conf_agent_10_freezing_refuses_every_bot_credential_until_unfrozen() {
    let s = TestServer::start_with(&[]);
    let (ta, user_bot) = setup(&s);
    // The organization acme (alice owns it, bob is a member) and its bot.
    s.create_user("bob", PW, false);
    s.create_user("sysop", PW, true);
    let mut a = s.login("alice", PW);
    let r = slath_tests::http::submit(
        &mut a,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", "acme"), ("display_name", "")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let mut api = s.client();
    let r = api.api(
        "PUT",
        "/api/v1/orgs/acme/members/bob",
        Some(&ta),
        Some(&json!({"role": "member"})),
    );
    assert!(r.status < 300, "{}", r.body);
    slath_tests::http::create_repo(&mut a, "acme", "lib", "private");
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            "acme",
            "--name",
            "acme-ci",
            "--token-scopes",
            "repo:read,user:read",
            "--repo",
            "acme/lib",
            "--output",
            "json",
        ],
        None,
    );
    assert!(ok, "{out}{err}");
    let org_bot = serde_json::from_str::<serde_json::Value>(&out).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = api.api(
        "POST",
        "/api/v1/bots/acme-ci/keys",
        Some(&ta),
        Some(&json!({"title": "ci", "key": data_key("ed25519.pub")})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let fp = data_key("ed25519.fp");
    let works = |api: &mut slath_tests::http::Client, t: &str| {
        api.api("GET", "/api/v1/user", Some(t), None).status == 200
    };
    assert!(works(&mut api, &org_bot) && key_offered(&s, &fp));

    // Only the organization's owners can freeze it.
    let mut b = s.login("bob", PW);
    let tb = create_token(&mut b, "t", &["user:write"]);
    let freeze = |api: &mut slath_tests::http::Client, t: &str, m: &str| {
        api.api(m, "/api/v1/orgs/acme/bot-freeze", Some(t), None)
    };
    assert_eq!(freeze(&mut api, &tb, "PUT").status, 403);
    assert_eq!(freeze(&mut api, &org_bot, "PUT").status, 403);
    let r = freeze(&mut api, &ta, "PUT");
    assert_eq!(r.status, 200, "{}", r.body);
    contract::check("PUT", "/api/v1/orgs/{org}/bot-freeze", &r);
    assert_eq!(r.json()["frozen"], true);
    assert_eq!(r.json()["frozen_by"], "alice");
    // The org's bot is refused everywhere; alice's own bot isn't covered.
    assert!(!works(&mut api, &org_bot));
    assert!(!key_offered(&s, &fp));
    assert!(works(&mut api, &user_bot));
    assert!(push(&s, &user_bot, "org-frozen"));
    // Freezing again changes nothing; unfreezing restores what was there.
    assert_eq!(freeze(&mut api, &ta, "PUT").status, 200);
    assert_eq!(audit_count(&s, "bot.freeze"), "1");
    let r = freeze(&mut api, &ta, "DELETE");
    assert_eq!(r.json()["frozen"], false);
    assert!(works(&mut api, &org_bot) && key_offered(&s, &fp));
    assert_eq!(audit_count(&s, "bot.unfreeze"), "1");

    // The instance freeze: site administrators only; it covers every bot
    // and no person.
    let path = "/api/v1/admin/bot-freeze";
    assert_eq!(api.api("PUT", path, Some(&ta), None).status, 404);
    let mut r = s.login("sysop", PW);
    let admin = create_token(&mut r, "t", &["admin", "user:write"]);
    let r = api.api("PUT", path, Some(&admin), None);
    assert_eq!(r.status, 200, "{}", r.body);
    contract::check("PUT", path, &r);
    assert!(!works(&mut api, &org_bot));
    assert!(!works(&mut api, &user_bot));
    assert!(!push(&s, &user_bot, "instance-frozen"));
    assert!(works(&mut api, &ta));
    let r = api.api("GET", path, Some(&admin), None);
    contract::check("GET", path, &r);
    assert_eq!(r.json()["frozen_by"], "sysop");
    assert_eq!(api.api("DELETE", path, Some(&admin), None).status, 200);
    assert!(works(&mut api, &org_bot) && works(&mut api, &user_bot));
    assert!(push(&s, &user_bot, "unfrozen"));
    assert_eq!(audit_count(&s, "bot.freeze"), "2");
    assert_eq!(audit_count(&s, "bot.unfreeze"), "2");
}

/// Opens a merge request from a new branch of `repo` as `token`.
fn bot_mr(s: &TestServer, token: &str, repo: &str, branch: &str) {
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let url = format!(
        "http://git:{token}@{}/{repo}.git",
        s.base().trim_start_matches("http://")
    );
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "x\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "-c", branch]);
    std::fs::write(w.join("f.txt"), branch).unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", branch]);
    git.ok(w, &["push", "-q", &url, branch]);
    let r = s.client().api(
        "POST",
        &format!("/api/v1/repos/{repo}/merge-requests"),
        Some(token),
        Some(&json!({"source_branch": branch, "title": format!("From {branch}")})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
}

#[test]
fn conf_agent_01_the_overview_shows_only_what_the_viewer_may_see() {
    // A low limit refills slowly, so the count of the bot's requests holds.
    let s = TestServer::start_full(&[], "[limits]\nbot_requests_per_hour = 10\n");
    s.create_user("alice", PW, false);
    s.create_user("bob", PW, false);
    s.create_user("sysop", PW, true);
    let mut a = s.login("alice", PW);
    let r = slath_tests::http::submit(
        &mut a,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", "acme"), ("display_name", "")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    slath_tests::http::create_repo(&mut a, "acme", "lib", "private");
    let ta = create_token(&mut a, "t", &["user:write", "repo:write"]);
    let mut b = s.login("bob", PW);
    slath_tests::http::create_repo(&mut b, "bob", "secret", "private");
    let tb = create_token(&mut b, "t", &["user:write", "repo:write"]);
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            "acme",
            "--name",
            "acme-ci",
            "--token-scopes",
            "repo:write,mrs:write,user:read",
            "--repo",
            "acme/lib",
            "--repo",
            "bob/secret",
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
    for (repo, token) in [("acme/lib", &ta), ("bob/secret", &tb)] {
        let r = api.api(
            "PUT",
            &format!("/api/v1/repos/{repo}/collaborators/acme-ci"),
            Some(token),
            Some(&json!({"role": "write"})),
        );
        assert!(r.status < 300, "{repo}: {}", r.body);
    }
    bot_mr(&s, &bot, "acme/lib", "fix-a");
    bot_mr(&s, &bot, "bob/secret", "fix-b");

    // alice manages the bot but can't read bob/secret: none of it shows.
    let r = api.api("GET", "/api/v1/bots/acme-ci/overview", Some(&ta), None);
    assert_eq!(r.status, 200, "{}", r.body);
    contract::check("GET", "/api/v1/bots/{name}/overview", &r);
    let o = r.json();
    assert_eq!(o["open_merge_requests"], 1, "{o}");
    assert_eq!(o["recent_merge_requests"][0]["repo"], "acme/lib");
    let pushes = o["recent_pushes"].as_array().unwrap();
    assert!(
        !pushes.is_empty() && pushes.iter().all(|p| p["repo"] == "acme/lib"),
        "{o}"
    );
    assert_eq!(o["frozen"], false);
    assert!(o["next_token_expiry"]["expires_at"].is_string(), "{o}");
    // Its two API requests (the merge requests); Git doesn't count.
    assert_eq!(o["rate_limit"], json!({"limit": 10, "used": 2}), "{o}");
    // A site administrator sees both; bob, who doesn't manage it, nothing.
    let mut c = s.login("sysop", PW);
    let admin = create_token(&mut c, "t", &["admin", "user:read"]);
    let o = api
        .api("GET", "/api/v1/bots/acme-ci/overview", Some(&admin), None)
        .json();
    assert_eq!(o["open_merge_requests"], 2, "{o}");
    assert_eq!(
        api.api("GET", "/api/v1/bots/acme-ci/overview", Some(&tb), None)
            .status,
        404
    );
    assert_eq!(
        api.api("GET", "/api/v1/bots/acme-ci/overview", Some(&bot), None)
            .status,
        404
    );
}
