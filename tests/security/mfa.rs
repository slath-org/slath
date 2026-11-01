// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Two-factor authentication (AUTH-02, AUTH-03): TOTP enrollment, the
//! sign-in code step, replay and drift rules, recovery codes, sudo with a
//! second factor, removal, the administrator requirement, and the reset.

use std::time::Duration;

use slath_auth::totp;
use slath_tests::http::{Client, login, submit};
use slath_tests::server::TestServer;
use slath_tests::smtp::SmtpServer;

const PW: &str = "correct horse battery staple";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap()
}

/// The setup key shown on the enrollment page.
fn secret_from(page: &str) -> Vec<u8> {
    let key = page
        .split("<code class=\"secret-key\">")
        .nth(1)
        .and_then(|r| r.split("</code>").next())
        .unwrap_or_else(|| panic!("no setup key on the page: {page}"));
    totp::base32_decode(key).unwrap()
}

fn recovery_codes_from(page: &str) -> Vec<String> {
    page.split("<li><code>")
        .skip(1)
        .filter_map(|r| r.split("</code>").next())
        .map(str::to_owned)
        .collect()
}

/// Enrolls TOTP for a signed-in client. Returns the seed, the step used
/// for confirmation, and the recovery codes.
fn enroll(c: &mut Client) -> (Vec<u8>, i64, Vec<String>) {
    let r = submit(c, "/-/settings/security", "/-/settings/security/totp", &[]);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let secret = secret_from(&r.body);
    assert!(r.body.contains("data:image/svg+xml;base64,"));
    // A wrong code is refused with a field error.
    let wrong = submit(
        c,
        "/-/settings/security",
        "/-/settings/security/totp/confirm",
        &[("code", "000000")],
    );
    let step = totp::step_at(now());
    if wrong.status == 200 {
        // 000000 happened to be right; astronomically unlikely, but valid.
        return (secret, step, recovery_codes_from(&wrong.body));
    }
    assert_eq!(wrong.status, 422);
    let code = totp::code_at(&secret, step);
    let r = submit(
        c,
        "/-/settings/security",
        "/-/settings/security/totp/confirm",
        &[("code", &code)],
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let codes = recovery_codes_from(&r.body);
    assert_eq!(codes.len(), 10, "{}", r.body);
    (secret, step, codes)
}

/// Signs in with password, then the code step. Returns the final response.
fn login_with_code(s: &TestServer, user: &str, code: &str) -> (Client, slath_tests::http::Resp) {
    let mut c = s.client();
    let r = login(&mut c, user, PW);
    assert_eq!(r.status, 303);
    let to = r.header("location").unwrap().to_owned();
    assert!(to.starts_with("/-/login/two-factor"), "{to}");
    assert!(
        !r.set_cookies()
            .iter()
            .any(|c| c.starts_with("slath_session=") && !c.contains("Max-Age=0")),
        "no session before the second factor"
    );
    let r = submit(&mut c, &to, "/-/login/two-factor", &[("code", code)]);
    (c, r)
}

#[test]
fn conf_auth_02_totp_codes_are_single_use_with_one_step_of_drift() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let (secret, confirmed_step, _) = enroll(&mut a);
    // The seed is stored encrypted.
    let stored = s.db.query_one(
        "SELECT encode(secret_enc, 'hex') FROM totp_credentials t JOIN users u ON u.id = t.user_id \
         WHERE u.namespace_id IN (SELECT id FROM namespaces WHERE name = 'alice')",
    );
    let hex: String = secret.iter().map(|b| format!("{b:02x}")).collect();
    assert!(!stored.contains(&hex));

    // The next step's code (one step of drift ahead) signs in once.
    let next = totp::code_at(&secret, confirmed_step + 1);
    let (mut c, r) = login_with_code(&s, "alice", &next);
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(c.get("/-/settings/profile").status, 200);
    // The same code again is refused, and so is an older one.
    let (_, r) = login_with_code(&s, "alice", &next);
    assert_eq!(r.status, 401);
    let (_, r) = login_with_code(&s, "alice", &totp::code_at(&secret, confirmed_step));
    assert_eq!(r.status, 401);
    // Two steps ahead is outside the window.
    let (_, r) = login_with_code(
        &s,
        "alice",
        &totp::code_at(&secret, totp::step_at(now()) + 2),
    );
    assert_eq!(r.status, 401);
}

#[test]
fn conf_auth_03_recovery_codes_work_once_and_wrong_codes_end_the_step() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let (_, _, codes) = enroll(&mut a);
    let (mut c, r) = login_with_code(&s, "alice", &codes[0].to_uppercase().replace('-', " "));
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(c.get("/-/settings/profile").status, 200);
    let (_, r) = login_with_code(&s, "alice", &codes[0]);
    assert_eq!(r.status, 401, "a recovery code works once");
    let page = c.get("/-/settings/security");
    assert!(
        page.body.contains("9 of 10 recovery codes left"),
        "{}",
        page.body
    );

    // Five wrong codes end the step: back to the password form.
    let mut c = s.client();
    let r = login(&mut c, "alice", PW);
    let to = r.header("location").unwrap().to_owned();
    for _ in 0..4 {
        assert_eq!(
            submit(&mut c, &to, "/-/login/two-factor", &[("code", "123456")]).status,
            401
        );
    }
    let r = submit(&mut c, &to, "/-/login/two-factor", &[("code", "123456")]);
    assert_eq!(r.status, 303);
    assert!(r.header("location").unwrap().starts_with("/-/login?"));
    assert_eq!(c.get(&to).status, 303, "the step is gone");
}

#[test]
fn conf_auth_03_removing_totp_needs_sudo_with_a_code_and_notifies() {
    let smtp = SmtpServer::start();
    let s = TestServer::start_full(&[], &smtp.config());
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let (secret, step, _) = enroll(&mut a);
    let _ = smtp.wait_for_text("turned on", Duration::from_secs(20));
    // Out of sudo mode: removal asks for the password and a code.
    s.db.execute("UPDATE sessions SET sudo_until = NULL");
    let r = submit(
        &mut a,
        "/-/settings/security",
        "/-/settings/security/totp/disable",
        &[],
    );
    assert_eq!(r.status, 303);
    let sudo = r.header("location").unwrap().to_owned();
    assert!(sudo.starts_with("/-/sudo"), "{sudo}");
    let page = a.get(&sudo);
    assert!(page.body.contains("Two-factor code"));
    let r = submit(&mut a, &sudo, "/-/sudo", &[("password", PW), ("code", "")]);
    assert_eq!(r.status, 401, "the password alone isn't enough");
    let code = totp::code_at(&secret, step + 1);
    let r = submit(
        &mut a,
        &sudo,
        "/-/sudo",
        &[("password", PW), ("code", &code)],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let r = submit(
        &mut a,
        "/-/settings/security",
        "/-/settings/security/totp/disable",
        &[],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM totp_credentials"),
        "0"
    );
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM recovery_codes"),
        "0"
    );
    let _ = smtp.wait_for_text("turned off", Duration::from_secs(20));
    // Signing in needs only the password again.
    assert_eq!(s.login("alice", PW).get("/-/settings/profile").status, 200);
}

#[test]
fn conf_auth_02_administration_requires_a_second_factor() {
    let s = TestServer::start_opts(&[], "", &[("SLATH__AUTH__REQUIRE_MFA_FOR_ADMINS", "true")]);
    s.create_user("boss", PW, true);
    let mut b = s.login("boss", PW);
    assert_eq!(b.get("/-/admin/users").status, 404);
    assert!(
        b.get("/")
            .body
            .contains("Administration needs two-factor authentication")
    );
    let (secret, step, _) = enroll(&mut b);
    // Still a password session: sign in again with a code.
    assert_eq!(b.get("/-/admin/users").status, 404);
    let (mut b, r) = login_with_code(&s, "boss", &totp::code_at(&secret, step + 1));
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(b.get("/-/admin/users").status, 200);
    assert!(!b.get("/").body.contains("Administration needs two-factor"));
}

#[test]
fn conf_auth_03_admin_reset_removes_factors_and_sessions() {
    let smtp = SmtpServer::start();
    let s = TestServer::start_full(&[], &smtp.config());
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    enroll(&mut a);
    let (ok, out, err) = s.slath(&["admin", "reset-mfa", "--username", "alice"], None);
    assert!(ok, "{out}{err}");
    // Her sessions end on the next request; the password alone signs in.
    assert_eq!(a.get("/-/settings/profile").status, 401);
    assert_eq!(s.login("alice", PW).get("/-/settings/profile").status, 200);
    assert_eq!(
        s.db.query_one("SELECT action FROM audit_events WHERE action = 'mfa.reset'"),
        "mfa.reset"
    );
    let _ = smtp.wait_for_text("administrator removed", Duration::from_secs(20));
    let (ok, _, err) = s.slath(&["admin", "reset-mfa", "--username", "nobody"], None);
    assert!(!ok && err.contains("no user named"), "{err}");
}
