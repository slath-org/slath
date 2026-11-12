// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bot accounts (`conf_auth_06_*`): created by their owner, never signed in,
//! capped by their owner, and constrained by guardrails.

use serde_json::json;
use slath_tests::contract;
use slath_tests::http::{Client, create_token, login, submit};
use slath_tests::server::TestServer;

const PW: &str = "correct horse battery staple";

fn create_org(c: &mut Client, name: &str) {
    let r = submit(
        c,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", name), ("display_name", "")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
}

#[test]
fn conf_auth_06_bots_are_created_and_seen_by_their_owner_only() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("bob", PW, false);
    let mut a = s.login("alice", PW);
    let mut b = s.login("bob", PW);
    let ta = create_token(&mut a, "t", &["user:write"]);
    let tb = create_token(&mut b, "t", &["user:write"]);
    let mut api = s.client();

    let r = api.api(
        "POST",
        "/api/v1/user/bots",
        Some(&ta),
        Some(&json!({"username": "alice-ci", "display_name": "Alice's CI"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    contract::check("POST", "/api/v1/user/bots", &r);
    let bot = r.json();
    assert_eq!(bot["owner"], json!({"kind": "user", "name": "alice"}));
    assert_eq!(bot["state"], "active");

    let r = api.api("GET", "/api/v1/user/bots", Some(&ta), None);
    assert_eq!(r.status, 200);
    contract::check("GET", "/api/v1/user/bots", &r);
    assert_eq!(r.json()["items"][0]["username"], "alice-ci");
    let r = api.api("GET", "/api/v1/bots/alice-ci", Some(&ta), None);
    assert_eq!(r.status, 200);
    contract::check("GET", "/api/v1/bots/{name}", &r);

    // Someone else's bot doesn't exist for bob, and bob's list is empty.
    assert_eq!(
        api.api("GET", "/api/v1/bots/alice-ci", Some(&tb), None)
            .status,
        404
    );
    let r = api.api("GET", "/api/v1/user/bots", Some(&tb), None);
    assert_eq!(r.json()["items"], json!([]));
    // A user isn't a bot.
    assert_eq!(
        api.api("GET", "/api/v1/bots/alice", Some(&ta), None).status,
        404
    );

    // Bot names share the namespace with users and organizations.
    for taken in ["bob", "ALICE-CI", "api"] {
        let r = api.api(
            "POST",
            "/api/v1/user/bots",
            Some(&ta),
            Some(&json!({"username": taken})),
        );
        assert_eq!(r.status, 422, "{taken}: {}", r.body);
    }
    // Anonymous requests can't create bots.
    let r = api.api(
        "POST",
        "/api/v1/user/bots",
        None,
        Some(&json!({"username": "x"})),
    );
    assert_eq!(r.status, 401);
}

#[test]
fn conf_auth_06_organization_bots_need_an_organization_owner() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("bob", PW, false);
    let mut a = s.login("alice", PW);
    create_org(&mut a, "acme");
    let r = submit(
        &mut a,
        "/acme/-/members",
        "/acme/-/members",
        &[("username", "bob"), ("role", "member")],
    );
    assert!(r.status == 303 || r.status == 200, "{}", r.body);
    let ta = create_token(&mut a, "t", &["user:write"]);
    let mut b = s.login("bob", PW);
    let tb = create_token(&mut b, "t", &["user:write"]);
    let mut api = s.client();

    // A member who isn't an owner can't create or list the org's bots.
    let new = json!({"username": "acme-bot"});
    for (m, body) in [("POST", Some(&new)), ("GET", None)] {
        let r = api.api(m, "/api/v1/orgs/acme/bots", Some(&tb), body);
        assert_eq!(r.status, 404, "{m}: {}", r.body);
    }
    let r = api.api("POST", "/api/v1/orgs/acme/bots", Some(&ta), Some(&new));
    assert_eq!(r.status, 201, "{}", r.body);
    contract::check("POST", "/api/v1/orgs/{org}/bots", &r);
    assert_eq!(r.json()["owner"], json!({"kind": "org", "name": "acme"}));
    let r = api.api("GET", "/api/v1/orgs/acme/bots", Some(&ta), None);
    contract::check("GET", "/api/v1/orgs/{org}/bots", &r);
    assert_eq!(r.json()["items"][0]["username"], "acme-bot");
    assert_eq!(
        api.api("GET", "/api/v1/bots/acme-bot", Some(&tb), None)
            .status,
        404
    );
    // Bots can't own organizations.
    let r = submit(
        &mut a,
        "/acme/-/members",
        "/acme/-/members",
        &[("username", "acme-bot"), ("role", "owner")],
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.body.contains("Bots can&#39;t own organizations."),
        "{}",
        r.body
    );
}

#[test]
fn conf_auth_06_bots_cannot_sign_in() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let t = create_token(&mut a, "t", &["user:write"]);
    let r = s.client().api(
        "POST",
        "/api/v1/user/bots",
        Some(&t),
        Some(&json!({"username": "alice-ci"})),
    );
    assert_eq!(r.status, 201);
    // Bots have no password (the database refuses one), and sign-in
    // with a bot's name fails exactly like with an unknown name.
    let mut c = s.client();
    let bot = login(&mut c, "alice-ci", PW);
    let mut c2 = s.client();
    let unknown = login(&mut c2, "nobody", PW);
    assert_eq!(bot.status, unknown.status);
    assert!(
        bot.set_cookies()
            .iter()
            .all(|c| !c.contains("slath_session=") || c.contains("slath_session=;"))
    );
    assert_ne!(c.get("/-/settings/profile").status, 200);
}

/// Creates a bot with `slath admin create-bot` and returns its token.
fn cli_bot(s: &TestServer, owner: &str, name: &str, extra: &[&str]) -> Result<String, String> {
    let mut args = vec![
        "admin",
        "create-bot",
        "--owner",
        owner,
        "--name",
        name,
        "--output",
        "json",
    ];
    args.extend_from_slice(extra);
    let (ok, out, err) = s.slath(&args, None);
    if !ok {
        return Err(format!("{out}{err}"));
    }
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    Ok(v["token"].as_str().unwrap_or_default().to_owned())
}

#[test]
fn conf_auth_06_bot_tokens_are_repo_restricted_and_expire() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    slath_tests::http::create_repo(&mut a, "alice", "app", "private");
    slath_tests::http::create_repo(&mut a, "alice", "other", "private");

    // Bot tokens must name repositories, may not manage accounts, and
    // can't outlive the configured maximum.
    for bad in [
        &["--token-scopes", "repo:read"][..],
        &["--token-scopes", "user:write", "--repo", "alice/app"],
        &["--token-scopes", "admin", "--repo", "alice/app"],
        &["--token-scopes", "repo:read", "--repo", "alice/nope"],
        &[
            "--token-scopes",
            "repo:read",
            "--repo",
            "alice/app",
            "--token-days",
            "91",
        ],
    ] {
        assert!(cli_bot(&s, "alice", "bad-bot", bad).is_err(), "{bad:?}");
    }
    let token = cli_bot(
        &s,
        "alice",
        "alice-ci",
        &[
            "--token-scopes",
            "repo:write,user:read",
            "--repo",
            "alice/app",
        ],
    )
    .unwrap();
    assert!(token.starts_with("slath_bot_"), "{token}");
    let days = s.db.query_one(
        "SELECT round(extract(epoch FROM expires_at - now()) / 86400)::text FROM access_tokens",
    );
    assert_eq!(days, "30");

    let mut api = s.client();
    // The bot gets a role like anyone else: here on both repositories.
    let tr = create_token(&mut a, "admin", &["repo:write"]);
    for repo in ["app", "other"] {
        let r = api.api(
            "PUT",
            &format!("/api/v1/repos/alice/{repo}/collaborators/alice-ci"),
            Some(&tr),
            Some(&json!({"role": "write"})),
        );
        assert!(r.status < 300, "{}", r.body);
    }
    let r = api.api("GET", "/api/v1/user", Some(&token), None);
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["kind"], "bot");
    assert_eq!(
        api.api("GET", "/api/v1/repos/alice/app", Some(&token), None)
            .status,
        200
    );
    assert_eq!(
        api.api("GET", "/api/v1/repos/alice/other", Some(&token), None)
            .status,
        404
    );
    // Bots can't create bots or organizations.
    for (path, body) in [
        ("/api/v1/user/bots", json!({"username": "sub-bot"})),
        ("/api/v1/orgs", json!({"name": "bot-org"})),
    ] {
        let r = api.api("POST", path, Some(&token), Some(&body));
        assert_eq!(r.status, 403, "{path}: {}", r.body);
    }

    // The owner sees the token, can't create one through the API, and
    // revokes it.
    let tw = create_token(&mut a, "t", &["user:write"]);
    let r = api.api("GET", "/api/v1/bots/alice-ci/tokens", Some(&tw), None);
    assert_eq!(r.status, 200);
    contract::check("GET", "/api/v1/bots/{name}/tokens", &r);
    let id = r.json()["items"][0]["id"].as_str().unwrap().to_owned();
    let r = api.api(
        "POST",
        "/api/v1/bots/alice-ci/tokens",
        Some(&tw),
        Some(&json!({"name": "x", "scopes": ["repo:read"]})),
    );
    assert_eq!(r.status, 403);
    let r = api.api(
        "DELETE",
        &format!("/api/v1/bots/alice-ci/tokens/{id}"),
        Some(&tw),
        None,
    );
    assert_eq!(r.status, 204);
    assert_eq!(
        api.api("GET", "/api/v1/user", Some(&token), None).status,
        401
    );
}

#[test]
fn conf_auth_06_token_kinds_are_bound_to_account_kinds() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    slath_tests::http::create_repo(&mut a, "alice", "app", "private");
    let pat = create_token(&mut a, "t", &["user:read"]);
    let bot = cli_bot(
        &s,
        "alice",
        "alice-ci",
        &["--token-scopes", "user:read", "--repo", "alice/app"],
    )
    .unwrap();
    let mut api = s.client();
    assert_eq!(api.api("GET", "/api/v1/user", Some(&pat), None).status, 200);
    assert_eq!(api.api("GET", "/api/v1/user", Some(&bot), None).status, 200);
    // Swap the owners of the two tokens: a person's token never
    // authenticates a bot, nor a bot's token a person.
    s.db.execute(
        "UPDATE access_tokens SET user_id = CASE WHEN user_id = (SELECT id FROM users WHERE kind = 'bot') \
           THEN (SELECT id FROM users WHERE kind = 'human') ELSE (SELECT id FROM users WHERE kind = 'bot') END",
    );
    assert_eq!(api.api("GET", "/api/v1/user", Some(&pat), None).status, 401);
    assert_eq!(api.api("GET", "/api/v1/user", Some(&bot), None).status, 401);
}

#[test]
fn conf_auth_06_bot_keys_are_managed_by_the_owner() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("bob", PW, false);
    let mut a = s.login("alice", PW);
    let ta = create_token(&mut a, "t", &["user:write"]);
    let mut b = s.login("bob", PW);
    let tb = create_token(&mut b, "t", &["user:write"]);
    cli_bot(&s, "alice", "alice-ci", &[]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (_, public) = slath_tests::sshd::keygen(dir.path(), "ci");
    let mut api = s.client();
    let key = json!({"title": "ci", "key": public});
    let r = api.api("POST", "/api/v1/bots/alice-ci/keys", Some(&tb), Some(&key));
    assert_eq!(r.status, 404);
    let r = api.api("POST", "/api/v1/bots/alice-ci/keys", Some(&ta), Some(&key));
    assert_eq!(r.status, 201, "{}", r.body);
    contract::check("POST", "/api/v1/bots/{name}/keys", &r);
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let r = api.api("GET", "/api/v1/bots/alice-ci/keys", Some(&ta), None);
    contract::check("GET", "/api/v1/bots/{name}/keys", &r);
    assert_eq!(r.json()["items"][0]["title"], "ci");
    // The key belongs to the bot, not to its owner.
    let r = api.api("GET", "/api/v1/user/keys", Some(&ta), None);
    assert_eq!(r.json()["items"], json!([]));
    let path = format!("/api/v1/bots/alice-ci/keys/{id}");
    assert_eq!(api.api("DELETE", &path, Some(&tb), None).status, 404);
    assert_eq!(api.api("DELETE", &path, Some(&ta), None).status, 204);
}
