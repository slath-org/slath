// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Email verification and password reset (AUTH-03): single-use tokens with
//! a TTL, identical answers for known and unknown addresses, sessions
//! revoked on reset, and the second factor still required.

use std::time::Duration;

use slath_auth::totp;
use slath_tests::http::{Client, login, submit};
use slath_tests::server::TestServer;
use slath_tests::smtp::{Received, SmtpServer};

const PW: &str = "correct horse battery staple";
const NEW_PW: &str = "a brand new passphrase here";

/// The first `public_url` link in a message with this path prefix
/// (quoted-printable soft breaks undone), as a path and query.
fn link(m: &Received, prefix: &str) -> String {
    let body = m.data.replace("=\r\n", "").replace("=3D", "=");
    let start = body
        .find(prefix)
        .unwrap_or_else(|| panic!("no {prefix} link in {body}"));
    body[start..]
        .split(|c: char| c.is_whitespace())
        .next()
        .unwrap()
        .to_owned()
}

fn verify(c: &mut Client, m: &Received) {
    let path = link(m, "/-/verify-email?token=");
    let token = path.split("token=").nth(1).unwrap().to_owned();
    let page = c.get(&path);
    assert_eq!(page.status, 200);
    assert!(page.body.contains("Confirm"));
    let r = submit(c, &path, "/-/verify-email", &[("token", &token)]);
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains("confirmed"));
    // Single use.
    let r = submit(c, &path, "/-/verify-email", &[("token", &token)]);
    assert_eq!(r.status, 410);
}

fn request_reset(c: &mut Client, email: &str) -> slath_tests::http::Resp {
    submit(
        c,
        "/-/password/reset",
        "/-/password/reset",
        &[("email", email)],
    )
}

/// The part of a page that differs between answers (no CSRF token).
fn answer(r: &slath_tests::http::Resp) -> String {
    r.body
        .split("<main")
        .nth(1)
        .and_then(|m| m.split("</main>").next())
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn conf_auth_03_password_reset_is_single_use_and_signs_out_everywhere() {
    let smtp = SmtpServer::start();
    let s = TestServer::start_full(&[], &smtp.config());
    s.create_user("alice", PW, false);
    let mut alice = s.login("alice", PW);
    // The account's address gets a confirmation link at creation.
    let m = smtp.wait_for(1, Duration::from_secs(20));
    assert_eq!(m[0].to, vec!["<alice@example.org>"]);
    verify(&mut s.client(), &m[0]);
    assert_eq!(
        s.db.query_one("SELECT email_verified::text FROM users WHERE email = 'alice@example.org'"),
        "true"
    );

    // Known and unknown addresses get the same answer.
    let mut anon = s.client();
    let known = request_reset(&mut anon, "Alice@Example.org");
    let unknown = request_reset(&mut anon, "nobody@example.org");
    assert_eq!(known.status, 200);
    assert_eq!(
        (known.status, answer(&known)),
        (unknown.status, answer(&unknown))
    );
    let m = smtp.wait_for(2, Duration::from_secs(20));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        smtp.messages().len(),
        2,
        "nothing goes to unknown addresses"
    );
    assert_eq!(
        m[1].to,
        vec!["<alice@example.org>"],
        "the stored address, not the typed one"
    );
    let path = link(&m[1], "/-/password/reset/confirm?token=");
    let token = path.split("token=").nth(1).unwrap().to_owned();

    let page = anon.get(&path);
    assert_eq!(page.status, 200);
    assert_eq!(page.header("cache-control"), Some("no-store"));
    assert_eq!(page.header("referrer-policy"), Some("no-referrer"));
    let r = submit(
        &mut anon,
        &path,
        "/-/password/reset/confirm",
        &[
            ("token", &token),
            ("new_password", NEW_PW),
            ("confirm_password", "other"),
        ],
    );
    assert_eq!(r.status, 422);
    let r = submit(
        &mut anon,
        &path,
        "/-/password/reset/confirm",
        &[
            ("token", &token),
            ("new_password", NEW_PW),
            ("confirm_password", NEW_PW),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.header("location"), Some("/-/login?status=password-reset"));
    // Every session ended; the new password works, the old doesn't.
    assert_eq!(alice.get("/-/settings/profile").status, 401);
    assert_eq!(login(&mut s.client(), "alice", PW).status, 401);
    assert_eq!(
        s.login("alice", NEW_PW).get("/-/settings/profile").status,
        200
    );
    // The link works once.
    assert_eq!(anon.get(&path).status, 410);
    let r = submit(
        &mut anon,
        "/-/password/reset",
        "/-/password/reset/confirm",
        &[
            ("token", &token),
            ("new_password", PW),
            ("confirm_password", PW),
        ],
    );
    assert_eq!(r.status, 422);
    assert_eq!(s.login("alice", NEW_PW).get("/").status, 200);
    let notices = smtp.wait_for(3, Duration::from_secs(20));
    assert!(
        notices[2]
            .data
            .contains("password of your account was reset")
    );
}

#[test]
fn conf_auth_03_reset_links_expire_and_need_a_confirmed_address() {
    let smtp = SmtpServer::start();
    let s = TestServer::start_full(&[], &smtp.config());
    s.create_user("alice", PW, false);
    let _ = smtp.wait_for(1, Duration::from_secs(20));
    // Unconfirmed address: no reset email.
    request_reset(&mut s.client(), "alice@example.org");
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(smtp.messages().len(), 1);
    verify(&mut s.client(), &smtp.messages()[0]);
    request_reset(&mut s.client(), "alice@example.org");
    let m = smtp.wait_for(2, Duration::from_secs(20));
    let path = link(&m[1], "/-/password/reset/confirm?token=");
    s.db.execute("UPDATE password_resets SET expires_at = now() - interval '1 second'");
    assert_eq!(s.client().get(&path).status, 410);
    // A request with several addresses is refused outright.
    let mut c = s.client();
    let page = c.get("/-/password/reset");
    let csrf = slath_tests::http::csrf_token(&page.body).unwrap();
    let r = c.post_form(
        "/-/password/reset",
        &[
            ("_csrf", &csrf),
            ("email", "alice@example.org"),
            ("email", "evil@example.org"),
        ],
    );
    assert!(r.status == 400 || r.status == 422, "{}", r.status);
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(smtp.messages().len(), 2);
}

#[test]
fn conf_auth_03_reset_still_requires_the_second_factor() {
    let smtp = SmtpServer::start();
    let s = TestServer::start_full(&[], &smtp.config());
    s.create_user("alice", PW, false);
    verify(
        &mut s.client(),
        &smtp.wait_for(1, Duration::from_secs(20))[0],
    );
    let mut a = s.login("alice", PW);
    let r = submit(
        &mut a,
        "/-/settings/security",
        "/-/settings/security/totp",
        &[],
    );
    let key = r
        .body
        .split("<code class=\"secret-key\">")
        .nth(1)
        .unwrap()
        .split("</code>")
        .next()
        .unwrap();
    let secret = totp::base32_decode(key).unwrap();
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let step = totp::step_at(now);
    let r = submit(
        &mut a,
        "/-/settings/security",
        "/-/settings/security/totp/confirm",
        &[("code", &totp::code_at(&secret, step))],
    );
    assert_eq!(r.status, 200);
    request_reset(&mut s.client(), "alice@example.org");
    let reset = smtp.wait_for_text("reset the password", Duration::from_secs(20));
    let path = link(&reset, "/-/password/reset/confirm?token=");
    let token = path.split("token=").nth(1).unwrap().to_owned();
    let mut c = s.client();
    assert!(c.get(&path).body.contains("Two-factor code"));
    let fields = |code: &str| {
        vec![
            ("token".to_owned(), token.clone()),
            ("new_password".to_owned(), NEW_PW.to_owned()),
            ("confirm_password".to_owned(), NEW_PW.to_owned()),
            ("code".to_owned(), code.to_owned()),
        ]
    };
    let send = |c: &mut Client, code: &str| {
        let f = fields(code);
        let refs: Vec<(&str, &str)> = f.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        submit(c, &path, "/-/password/reset/confirm", &refs)
    };
    let r = send(&mut c, "");
    assert_eq!(r.status, 422);
    assert!(r.body.contains("authenticator app"));
    assert_eq!(
        login(&mut s.client(), "alice", NEW_PW).status,
        401,
        "nothing changed"
    );
    let r = send(&mut c, &totp::code_at(&secret, step + 1));
    assert_eq!(r.status, 303, "{}", r.body);
    // Signing in still asks for the second factor.
    let r = login(&mut s.client(), "alice", NEW_PW);
    assert!(
        r.header("location")
            .unwrap()
            .starts_with("/-/login/two-factor")
    );
}

#[test]
fn without_email_resets_are_hidden() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut c = s.client();
    let page = c.get("/-/login");
    assert!(!page.body.contains("/-/password/reset"));
    assert!(page.body.contains("Ask an administrator"));
    assert_eq!(c.get("/-/password/reset").status, 404);
    assert_eq!(
        c.post_form("/-/password/reset", &[("email", "alice@example.org")])
            .status,
        403
    );
}
