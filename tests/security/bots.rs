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
