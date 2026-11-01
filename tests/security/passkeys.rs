// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Passkeys (AUTH-02): registration and use as a second factor, for
//! passwordless sign-in, and for sudo; origin, RP ID, user verification,
//! and single-use ceremonies are enforced.

use serde_json::Value;
use slath_tests::http::{Client, csrf_token, login, submit};
use slath_tests::server::{Instance, TestServer};
use slath_tests::webauthn::SoftPasskey;

const PW: &str = "correct horse battery staple";

fn server(env: &[(&str, &str)]) -> (TestServer, String) {
    let inst = Instance::new("");
    let origin = inst.use_localhost();
    let s = inst.start(env);
    (s, origin)
}

/// POSTs to an options endpoint like the page script does.
fn options(c: &mut Client, page: &str, url: &str) -> (u16, Value) {
    let p = c.get(page);
    let token = csrf_token(&p.body).unwrap_or_default();
    let r = c.request(
        "POST",
        url,
        &[("x-csrf-token", &token), ("accept", "application/json")],
        None,
    );
    let v = if r.status == 200 {
        r.json()
    } else {
        Value::Null
    };
    (r.status, v)
}

fn add_passkey(c: &mut Client, key: &mut SoftPasskey, name: &str) -> slath_tests::http::Resp {
    let (status, o) = options(
        c,
        "/-/settings/security",
        "/-/settings/security/passkeys/options",
    );
    assert_eq!(status, 200);
    let cred = key.create(&o["options"]);
    submit(
        c,
        "/-/settings/security",
        "/-/settings/security/passkeys",
        &[
            ("ceremony", o["ceremony"].as_str().unwrap()),
            ("name", name),
            ("credential", &cred.to_string()),
        ],
    )
}

fn passkey_login(s: &TestServer, key: &mut SoftPasskey) -> (Client, slath_tests::http::Resp) {
    let mut c = s.client();
    let (status, o) = options(&mut c, "/-/login", "/-/login/passkey/options");
    assert_eq!(status, 200);
    let cred = key.get(&o["options"]);
    let r = submit(
        &mut c,
        "/-/login",
        "/-/login/passkey",
        &[
            ("ceremony", o["ceremony"].as_str().unwrap()),
            ("credential", &cred.to_string()),
            ("next", "/-/settings/profile"),
        ],
    );
    (c, r)
}

#[test]
fn conf_auth_02_passkeys_sign_in_alone_and_as_second_factor() {
    let (s, origin) = server(&[("SLATH__AUTH__REQUIRE_MFA_FOR_ADMINS", "true")]);
    s.create_user("ada", PW, true);
    let mut a = s.login("ada", PW);
    let mut key = SoftPasskey::new(&origin, 1);
    let r = add_passkey(&mut a, &mut key, "Laptop");
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(r.header("location").unwrap().contains("passkey-added"));
    assert!(a.get("/-/settings/security").body.contains("Laptop"));

    // Passwordless: a passkey session, which satisfies the admin MFA rule.
    let (mut c, r) = passkey_login(&s, &mut key);
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.header("location"), Some("/-/settings/profile"));
    assert_eq!(c.get("/-/admin/users").status, 200);
    assert_eq!(
        s.db.query_one("SELECT auth_level FROM sessions ORDER BY created_at DESC LIMIT 1"),
        "passkey"
    );

    // With a password, the passkey is the second step.
    let mut c = s.client();
    let r = login(&mut c, "ada", PW);
    let step = r.header("location").unwrap().to_owned();
    assert!(step.starts_with("/-/login/two-factor"));
    assert!(c.get(&step).body.contains("Use a passkey"));
    let (status, o) = options(&mut c, &step, "/-/login/two-factor/passkey/options");
    assert_eq!(status, 200);
    let cred = key.get(&o["options"]);
    let r = submit(
        &mut c,
        &step,
        "/-/login/two-factor/passkey",
        &[
            ("ceremony", o["ceremony"].as_str().unwrap()),
            ("credential", &cred.to_string()),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(c.get("/-/admin/users").status, 200);

    // Sudo with a passkey.
    s.db.execute("UPDATE sessions SET sudo_until = NULL");
    let (status, o) = options(&mut c, "/-/sudo", "/-/sudo/passkey/options");
    assert_eq!(status, 200);
    let cred = key.get(&o["options"]);
    let r = submit(
        &mut c,
        "/-/sudo",
        "/-/sudo/passkey",
        &[
            ("ceremony", o["ceremony"].as_str().unwrap()),
            ("credential", &cred.to_string()),
            ("next", "/-/settings/security"),
        ],
    );
    assert_eq!(r.status, 303);
    assert_eq!(r.header("location"), Some("/-/settings/security"));

    // Removing the passkey (sudo is active).
    let id = s.db.query_one("SELECT id::text FROM webauthn_credentials");
    let r = submit(
        &mut c,
        "/-/settings/security",
        &format!("/-/settings/security/passkeys/{id}/delete"),
        &[],
    );
    assert_eq!(r.status, 303);
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM webauthn_credentials"),
        "0"
    );
    let (_, r) = passkey_login(&s, &mut key);
    assert!(r.header("location").unwrap().contains("passkey-failed"));
}

#[test]
fn conf_auth_02_wrong_origin_rp_id_or_missing_verification_is_rejected() {
    let (s, origin) = server(&[]);
    s.create_user("ada", PW, false);
    let mut a = s.login("ada", PW);

    let mut evil = SoftPasskey::new("http://evil.example", 2);
    evil.rp_id = "localhost".to_owned();
    let r = add_passkey(&mut a, &mut evil, "Wrong origin");
    assert!(r.header("location").unwrap().contains("passkey-failed"));
    let mut other_rp = SoftPasskey::new(&origin, 3);
    other_rp.rp_id = "evil.example".to_owned();
    let r = add_passkey(&mut a, &mut other_rp, "Wrong RP");
    assert!(r.header("location").unwrap().contains("passkey-failed"));
    let mut no_uv = SoftPasskey::new(&origin, 4);
    no_uv.user_verified = false;
    let r = add_passkey(&mut a, &mut no_uv, "No UV");
    assert!(r.header("location").unwrap().contains("passkey-failed"));
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM webauthn_credentials"),
        "0"
    );

    let mut key = SoftPasskey::new(&origin, 5);
    assert!(
        add_passkey(&mut a, &mut key, "Good")
            .header("location")
            .unwrap()
            .contains("passkey-added")
    );
    // Assertions from another origin, or for another RP ID, fail.
    key.origin = "http://evil.example".to_owned();
    let (_, r) = passkey_login(&s, &mut key);
    assert!(r.header("location").unwrap().contains("passkey-failed"));
    key.origin.clone_from(&origin);
    key.rp_id = "evil.example".to_owned();
    let (_, r) = passkey_login(&s, &mut key);
    assert!(r.header("location").unwrap().contains("passkey-failed"));
    key.rp_id = "localhost".to_owned();

    // A ceremony can be finished once.
    let mut c = s.client();
    let (_, o) = options(&mut c, "/-/login", "/-/login/passkey/options");
    let cred = key.get(&o["options"]).to_string();
    let ceremony = o["ceremony"].as_str().unwrap().to_owned();
    let fields = [
        ("ceremony", ceremony.as_str()),
        ("credential", cred.as_str()),
    ];
    assert_eq!(
        submit(&mut c, "/-/login", "/-/login/passkey", &fields).header("location"),
        Some("/")
    );
    let mut c2 = s.client();
    let r = submit(&mut c2, "/-/login", "/-/login/passkey", &fields);
    assert!(r.header("location").unwrap().contains("passkey-failed"));
}

#[test]
fn passkeys_need_a_host_name() {
    let s = TestServer::start_with(&[]);
    s.create_user("ada", PW, false);
    let mut a = s.login("ada", PW);
    assert!(!a.get("/-/settings/security").body.contains("Passkeys"));
    assert!(!a.get("/-/login").body.contains("Sign in with a passkey"));
    let (status, _) = options(
        &mut a,
        "/-/settings/security",
        "/-/settings/security/passkeys/options",
    );
    assert_eq!(status, 404);
}
