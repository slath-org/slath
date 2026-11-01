// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Account settings, session management, and sudo mode.

use slath_tests::http::{Client, csrf_token, login, submit};
use slath_tests::server::TestServer;

const PW: &str = "correct horse battery staple";

fn signed_in(c: &mut Client) -> bool {
    c.get("/").body.contains("action=\"/-/logout\"")
}

fn session_ids(c: &mut Client) -> Vec<String> {
    let page = c.get("/-/settings/sessions");
    page.body
        .split("action=\"/-/settings/sessions/")
        .skip(1)
        .filter_map(|r| r.split('/').next())
        .filter(|id| id.len() == 64)
        .map(str::to_owned)
        .collect()
}

fn expire_sudo(s: &TestServer) {
    s.db.execute("UPDATE sessions SET sudo_until = NULL");
}

#[test]
fn conf_auth_03_revoked_session_fails_on_next_request() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let mut b = s.login("alice", PW);
    let ids = session_ids(&mut a);
    assert_eq!(ids.len(), 2);
    let current_a = a.get("/-/settings/sessions").body;
    // Revoke the session that isn't marked as this browser.
    let other = ids
        .iter()
        .find(|id| {
            let row = current_a.split(id.as_str()).next().unwrap();
            !row.rsplit("<tr>").next().unwrap().contains("This browser")
        })
        .unwrap();
    let r = submit(
        &mut a,
        "/-/settings/sessions",
        &format!("/-/settings/sessions/{other}/revoke"),
        &[],
    );
    assert_eq!(r.status, 303);
    assert!(signed_in(&mut a));
    assert!(!signed_in(&mut b));
}

#[test]
fn conf_auth_03_sign_out_everywhere() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let mut b = s.login("alice", PW);
    let r = submit(
        &mut a,
        "/-/settings/sessions",
        "/-/settings/sessions/revoke-all",
        &[],
    );
    assert_eq!(r.status, 303);
    assert!(!signed_in(&mut a));
    assert!(!signed_in(&mut b));
}

#[test]
fn conf_rbac_02_other_users_session_ids_are_not_found() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("mallory", PW, false);
    let mut a = s.login("alice", PW);
    let alice_id = session_ids(&mut a).pop().unwrap();
    let mut m = s.login("mallory", PW);
    let r = submit(
        &mut m,
        "/-/settings/sessions",
        &format!("/-/settings/sessions/{alice_id}/revoke"),
        &[],
    );
    assert_eq!(r.status, 404);
    let r = submit(
        &mut m,
        "/-/settings/sessions",
        &format!("/-/settings/sessions/{}/revoke", "0".repeat(64)),
        &[],
    );
    assert_eq!(r.status, 404);
    assert!(signed_in(&mut a));
}

#[test]
fn conf_auth_03_password_change_revokes_other_sessions() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let mut b = s.login("alice", PW);
    let new = "a completely new passphrase";
    let r = submit(
        &mut a,
        "/-/settings/security",
        "/-/settings/security/password",
        &[("new_password", new), ("confirm_password", new)],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(signed_in(&mut a), "the changing session stays");
    assert!(!signed_in(&mut b));
    assert_eq!(login(&mut s.client(), "alice", PW).status, 401);
    assert_eq!(login(&mut s.client(), "alice", new).status, 303);
}

#[test]
fn password_change_validates_input() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let r = submit(
        &mut a,
        "/-/settings/security",
        "/-/settings/security/password",
        &[("new_password", "short"), ("confirm_password", "different")],
    );
    assert_eq!(r.status, 422);
    assert!(r.body.contains("Use at least 12 characters."));
    assert!(
        r.body.contains("The passwords don&#39;t match."),
        "{}",
        r.body
    );
    assert!(r.body.contains("aria-invalid=\"true\""));
}

#[test]
fn conf_auth_03_sensitive_actions_need_sudo_and_return_afterwards() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    expire_sudo(&s);
    let page = a.get("/-/settings/security");
    let token = csrf_token(&page.body).unwrap();
    let new = "another new passphrase";
    let referer = format!("{}/-/settings/security", s.base());
    let r = a.post_form_with(
        "/-/settings/security/password",
        &[
            ("_csrf", &token),
            ("new_password", new),
            ("confirm_password", new),
        ],
        &[("referer", &referer)],
    );
    assert_eq!(r.status, 303);
    let to = r.header("location").unwrap().to_owned();
    assert_eq!(to, "/-/sudo?next=%2F%2D%2Fsettings%2Fsecurity");
    // Nothing changed yet.
    assert_eq!(login(&mut s.client(), "alice", PW).status, 303);
    // A wrong password stays on the confirmation page.
    let r = submit(
        &mut a,
        &to,
        "/-/sudo",
        &[
            ("password", "wrong password!!"),
            ("next", "/-/settings/security"),
        ],
    );
    assert_eq!(r.status, 401);
    // The right one returns to the form.
    let r = submit(
        &mut a,
        &to,
        "/-/sudo",
        &[("password", PW), ("next", "/-/settings/security")],
    );
    assert_eq!(r.status, 303);
    assert_eq!(r.header("location"), Some("/-/settings/security"));
    let r = submit(
        &mut a,
        "/-/settings/security",
        "/-/settings/security/password",
        &[("new_password", new), ("confirm_password", new)],
    );
    assert_eq!(r.status, 303);
    assert_eq!(
        r.header("location"),
        Some("/-/settings/security?status=password-changed")
    );
}

#[test]
fn conf_sec_04_profile_rejects_text_direction_tricks() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let r = submit(
        &mut a,
        "/-/settings/profile",
        "/-/settings/profile",
        &[("display_name", "Alice \u{202e}gnp.exe")],
    );
    assert_eq!(r.status, 422);
    let r = submit(
        &mut a,
        "/-/settings/profile",
        "/-/settings/profile",
        &[("display_name", "<b>Alice</b>")],
    );
    assert_eq!(r.status, 303);
    let page = a.get("/-/settings/profile");
    let field = page
        .body
        .split("id=\"display_name\"")
        .nth(1)
        .unwrap()
        .split('>')
        .next()
        .unwrap()
        .to_owned();
    assert!(
        field.contains("&#60;b&#62;Alice&#60;") && !field.contains("<b>"),
        "escaped: {field}"
    );
}

#[test]
fn settings_need_a_session() {
    let s = TestServer::start_with(&[]);
    let mut c = s.client();
    for p in [
        "/-/settings/profile",
        "/-/settings/security",
        "/-/settings/sessions",
        "/-/sudo",
    ] {
        let r = c.get(p);
        assert_eq!(r.status, 401, "{p}");
        assert!(r.body.contains("/-/login?next="), "{p}");
    }
}

#[test]
fn conf_sec_07_avatars_are_generated_locally() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut c = s.client();
    let a = c.get("/-/avatars/alice.svg");
    let n = c.get("/-/avatars/nosuchuser.svg");
    assert_eq!(a.status, 200);
    assert_eq!(n.status, 200, "no existence oracle");
    assert_eq!(a.header("content-type"), Some("image/svg+xml"));
    assert!(
        a.header("content-security-policy")
            .unwrap()
            .contains("sandbox")
    );
    assert!(
        !a.body.contains("http") || a.body.matches("http").count() == 1,
        "only the SVG namespace"
    );
    assert_eq!(c.get("/-/avatars/alice.png").status, 404);
}
