// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! OpenID Connect sign-in (AUTH-01, AUTH-04) against a mock provider:
//! every token check, single-use states bound to the browser, linking only
//! from a signed-in session, no linking by email, second factors still
//! required, disabled accounts stay disabled, and no remote pictures.

use serde_json::json;
use slath_tests::http::{Client, submit};
use slath_tests::oidc::{MockProvider, state_and_nonce};
use slath_tests::server::{PASSWORD, TestServer};

struct Env {
    s: TestServer,
    idp: MockProvider,
    _dir: tempfile::TempDir,
}

fn setup(extra: &str) -> Env {
    let idp = MockProvider::start();
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("client-secret");
    std::fs::write(&secret, "s3cret\n").unwrap();
    let s = TestServer::start_full(&[], &idp.config(&secret, extra));
    s.create_user("alice", PASSWORD, false);
    s.db.execute("UPDATE users SET email_verified = true");
    Env { s, idp, _dir: dir }
}

/// Starts a sign-in in `c`; returns state and nonce.
fn begin(c: &mut Client) -> (String, String) {
    let r = c.get("/-/login/oidc/mock?next=/alice");
    assert_eq!(r.status, 303, "{}", r.body);
    let loc = r.header("location").unwrap().to_owned();
    assert!(loc.contains("code_challenge_method=S256"), "{loc}");
    state_and_nonce(&loc)
}

fn callback(c: &mut Client, state: &str) -> slath_tests::http::Resp {
    c.get(&format!(
        "/-/login/oidc/mock/callback?code=abc&state={state}"
    ))
}

/// Links `sub` to alice through the settings (password sign-in, sudo).
fn link_alice(e: &Env, sub: &str) -> Client {
    let mut a = e.s.login("alice", PASSWORD);
    let r = submit(
        &mut a,
        "/-/settings/security",
        "/-/settings/oidc/mock/link",
        &[],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let loc = r.header("location").unwrap().to_owned();
    let (state, nonce) = state_and_nonce(&loc);
    assert!(!state.is_empty(), "{loc}");
    e.idp.next_token(e.idp.sign(
        &json!({"alg": "ES256", "kid": "test-key"}),
        &e.idp.claims(sub, &nonce),
        false,
    ));
    let r = callback(&mut a, &state);
    assert_eq!(
        r.header("location"),
        Some("/-/settings/security?status=oidc-linked"),
        "{}",
        r.body
    );
    a
}

#[test]
fn conf_auth_01_tokens_are_fully_checked() {
    let e = setup("");
    link_alice(&e, "alice-sub");
    let header = json!({"alg": "ES256", "kid": "test-key"});

    // A good token signs alice in.
    let mut c = e.s.client();
    let (state, nonce) = begin(&mut c);
    e.idp.next_token(
        e.idp
            .sign(&header, &e.idp.claims("alice-sub", &nonce), false),
    );
    let r = callback(&mut c, &state);
    assert_eq!(r.header("location"), Some("/alice"), "{}", r.body);
    assert!(c.get("/-/settings/profile").status == 200);
    // The token request carried the PKCE verifier and client credentials.
    let token_req = e
        .idp
        .requests()
        .into_iter()
        .find(|r| r.starts_with("POST /token"))
        .unwrap();
    assert!(
        token_req.contains("code_verifier=") && token_req.contains("grant_type=authorization_code")
    );
    // Replaying the same state fails.
    e.idp.next_token(
        e.idp
            .sign(&header, &e.idp.claims("alice-sub", &nonce), false),
    );
    let mut again = e.s.client();
    let r = callback(&mut again, &state);
    assert_eq!(r.status, 400);

    type Tweak = Box<dyn Fn(&mut serde_json::Value, &mut serde_json::Value) -> bool>;
    let cases: Vec<(&str, Tweak)> = vec![
        ("bad signature", Box::new(|_, _| true)),
        (
            "alg none",
            Box::new(|h, _| {
                *h = json!({"alg": "none"});
                false
            }),
        ),
        (
            "wrong iss",
            Box::new(|_, c| {
                c["iss"] = "http://127.0.0.1:1".into();
                false
            }),
        ),
        (
            "wrong aud",
            Box::new(|_, c| {
                c["aud"] = "someone-else".into();
                false
            }),
        ),
        (
            "expired",
            Box::new(|_, c| {
                c["exp"] = 1_000_000.into();
                false
            }),
        ),
        (
            "nonce mismatch",
            Box::new(|_, c| {
                c["nonce"] = "nope".into();
                false
            }),
        ),
    ];
    for (name, tweak) in cases {
        let mut c = e.s.client();
        let (state, nonce) = begin(&mut c);
        let mut h = header.clone();
        let mut claims = e.idp.claims("alice-sub", &nonce);
        let wrong_key = tweak(&mut h, &mut claims);
        let token = if h["alg"] == "none" {
            let mut t = e.idp.sign(&h, &claims, false);
            t.truncate(t.rfind('.').unwrap() + 1);
            t
        } else {
            e.idp.sign(&h, &claims, wrong_key)
        };
        e.idp.next_token(token);
        let r = callback(&mut c, &state);
        assert_eq!(r.status, 400, "{name}: {}", r.body);
        assert!(
            r.header("set-cookie")
                .is_none_or(|v| !v.contains("slath_session=") || v.contains("slath_session=;")),
            "{name}"
        );
        assert_ne!(
            c.get("/-/settings/profile").status,
            200,
            "{name}: signed in"
        );
    }

    // A state started in another browser doesn't work here.
    let mut victim = e.s.client();
    let (state, nonce) = begin(&mut victim);
    e.idp.next_token(
        e.idp
            .sign(&header, &e.idp.claims("alice-sub", &nonce), false),
    );
    let mut attacker = e.s.client();
    assert_eq!(callback(&mut attacker, &state).status, 400);
}

#[test]
fn conf_auth_04_email_never_links_accounts() {
    let e = setup("trust_email = true\nallow_registration = true\n");
    let header = json!({"alg": "ES256", "kid": "test-key"});
    // Mallory's provider account claims alice's verified address.
    let mut m = e.s.client();
    let (state, nonce) = begin(&mut m);
    let mut claims = e.idp.claims("mallory-sub", &nonce);
    claims["email"] = "alice@example.org".into();
    claims["email_verified"] = true.into();
    claims["picture"] = "http://169.254.169.254/latest/meta-data/".into();
    e.idp.next_token(e.idp.sign(&header, &claims, false));
    let r = callback(&mut m, &state);
    assert_eq!(r.status, 403, "{}", r.body);
    assert!(
        r.body.contains("Sign in to it with your password"),
        "{}",
        r.body
    );
    assert_eq!(
        e.s.db
            .query_one("SELECT count(*)::text FROM oidc_identities"),
        "0"
    );
    assert_ne!(m.get("/-/settings/profile").status, 200);

    // A link flow started in Mallory's session can't be finished by alice.
    e.s.create_user("mallory", PASSWORD, false);
    let mut mal = e.s.login("mallory", PASSWORD);
    let r = submit(
        &mut mal,
        "/-/settings/security",
        "/-/settings/oidc/mock/link",
        &[],
    );
    let (state, nonce) = state_and_nonce(r.header("location").unwrap());
    e.idp.next_token(
        e.idp
            .sign(&header, &e.idp.claims("mallory-sub", &nonce), false),
    );
    let mut alice = e.s.login("alice", PASSWORD);
    assert_eq!(callback(&mut alice, &state).status, 400);
    assert_eq!(
        e.s.db
            .query_one("SELECT count(*)::text FROM oidc_identities"),
        "0"
    );

    // A new identity without a matching email registers after confirming.
    let mut n = e.s.client();
    let (state, nonce) = begin(&mut n);
    let mut claims = e.idp.claims("newbie-sub", &nonce);
    claims["preferred_username"] = "newbie".into();
    e.idp.next_token(e.idp.sign(&header, &claims, false));
    let r = callback(&mut n, &state);
    assert_eq!(
        r.header("location"),
        Some("/-/login/oidc/register"),
        "{}",
        r.body
    );
    let page = n.get("/-/login/oidc/register");
    assert!(page.body.contains("value=\"newbie\""), "{}", page.body);
    let r = submit(
        &mut n,
        "/-/login/oidc/register",
        "/-/login/oidc/register",
        &[("username", "newbie")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(n.get("/newbie").status, 200);
    assert_eq!(
        e.s.db.query_one("SELECT u.state FROM oidc_identities i JOIN users u ON u.id = i.user_id WHERE i.subject = 'newbie-sub'"),
        "active"
    );
    // The picture claim was never fetched (only discovery, keys, token).
    assert!(e.idp.requests().iter().all(|r| !r.contains("meta-data")));
}

#[test]
fn conf_auth_02_second_factor_and_disabled_accounts() {
    let e = setup("");
    let header = json!({"alg": "ES256", "kid": "test-key"});
    link_alice(&e, "alice-sub");
    // alice has a second factor: the OIDC sign-in still asks for it.
    e.s.db
        .execute("INSERT INTO encryption_keys (key_id) VALUES ('0badc0de') ON CONFLICT DO NOTHING");
    e.s.db.execute(
        "INSERT INTO totp_credentials (user_id, key_id, secret_enc, confirmed)
         SELECT id, '0badc0de', '\\x00', true FROM users WHERE namespace_id = (SELECT id FROM namespaces WHERE name = 'alice')",
    );
    let mut c = e.s.client();
    let (state, nonce) = begin(&mut c);
    e.idp.next_token(
        e.idp
            .sign(&header, &e.idp.claims("alice-sub", &nonce), false),
    );
    let r = callback(&mut c, &state);
    assert!(
        r.header("location")
            .unwrap()
            .starts_with("/-/login/two-factor"),
        "{:?}",
        r.header("location")
    );
    assert_ne!(c.get("/-/settings/profile").status, 200);

    // A disabled account stays disabled.
    e.s.db.execute("DELETE FROM totp_credentials");
    e.s.db.execute("UPDATE users SET state = 'suspended'");
    let mut c = e.s.client();
    let (state, nonce) = begin(&mut c);
    e.idp.next_token(
        e.idp
            .sign(&header, &e.idp.claims("alice-sub", &nonce), false),
    );
    let r = callback(&mut c, &state);
    assert_eq!(r.status, 403, "{}", r.body);
    assert_ne!(c.get("/-/settings/profile").status, 200);
    assert_eq!(
        e.s.db.query_one("SELECT state FROM users LIMIT 1"),
        "suspended"
    );
}
