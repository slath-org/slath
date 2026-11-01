// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Registration modes, invitations, and the restricted state of accounts
//! with an unconfirmed address.

use std::time::Duration;

use slath_tests::http::{Client, create_repo, submit};
use slath_tests::server::TestServer;
use slath_tests::smtp::SmtpServer;

const PW: &str = "correct horse battery staple";

fn register(c: &mut Client, page: &str, fields: &[(&str, &str)]) -> slath_tests::http::Resp {
    let mut all = vec![("password", PW), ("confirm_password", PW)];
    all.extend_from_slice(fields);
    submit(c, page, "/-/register", &all)
}

#[test]
fn registration_is_not_found_when_disabled() {
    let s = TestServer::start_with(&[]);
    let mut c = s.client();
    assert_eq!(c.get("/-/register").status, 404);
    let r = register(
        &mut c,
        "/-/login",
        &[("username", "mallory"), ("email", "m@example.org")],
    );
    assert_eq!(r.status, 404);
    assert!(!c.get("/-/login").body.contains("Create an account"));
    assert_eq!(s.db.query_one("SELECT count(*)::text FROM users"), "0");
}

#[test]
fn open_registration_restricts_unconfirmed_accounts() {
    let smtp = SmtpServer::start();
    let s = TestServer::start_opts(
        &[],
        &smtp.config(),
        &[("SLATH__AUTH__REGISTRATION", "open")],
    );
    let mut c = s.client();
    assert!(c.get("/-/login").body.contains("Create an account"));
    let r = register(
        &mut c,
        "/-/register",
        &[
            ("username", "eve"),
            ("email", "eve@example.org"),
            ("confirm_password", "different"),
        ],
    );
    assert_eq!(r.status, 422);
    let r = register(
        &mut c,
        "/-/register",
        &[("username", "eve"), ("email", "eve@example.org")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    // Signed in, but restricted until the address is confirmed.
    assert!(c.get("/").body.contains("Confirm your email address"));
    let r = submit(
        &mut c,
        "/-/new",
        "/-/new",
        &[
            ("namespace", "eve"),
            ("name", "first"),
            ("description", ""),
            ("visibility", "public"),
            ("default_branch", "main"),
        ],
    );
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("Confirm your email address"));
    let m = smtp.wait_for_text("/-/verify-email?token=", Duration::from_secs(20));
    let body = m.data.replace("=\r\n", "").replace("=3D", "=");
    let path = body[body.find("/-/verify-email?token=").unwrap()..]
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let token = path.split("token=").nth(1).unwrap().to_owned();
    assert_eq!(
        submit(&mut c, &path, "/-/verify-email", &[("token", &token)]).status,
        200
    );
    assert_eq!(create_repo(&mut c, "eve", "first", "public").status, 303);
    assert!(!c.get("/").body.contains("Confirm your email address"));
}

#[test]
fn registrations_are_rate_limited_per_ip() {
    let s = TestServer::start_opts(&[], "", &[("SLATH__AUTH__REGISTRATION", "open")]);
    for i in 0..5 {
        let mut c = s.client();
        let name = format!("user{i}");
        let email = format!("{name}@example.org");
        let r = register(
            &mut c,
            "/-/register",
            &[("username", &name), ("email", &email)],
        );
        assert_eq!(r.status, 303, "{i}: {}", r.body);
    }
    let mut c = s.client();
    let r = register(
        &mut c,
        "/-/register",
        &[("username", "user9"), ("email", "u9@example.org")],
    );
    assert_eq!(r.status, 429);
}

#[test]
fn invitations_are_single_use_and_bound_to_their_address() {
    let s = TestServer::start_opts(&[], "", &[("SLATH__AUTH__REGISTRATION", "invite")]);
    s.create_user("boss", PW, true);
    let mut boss = s.login("boss", PW);
    let mut anon = s.client();
    assert_eq!(
        anon.get("/-/register").status,
        404,
        "an invitation is needed"
    );
    assert_eq!(anon.get("/-/register?invite=bogus").status, 404);

    let r = submit(
        &mut boss,
        "/-/admin/invites",
        "/-/admin/invites",
        &[("email", "new@example.org"), ("note", "new hire")],
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let link = r
        .body
        .split("<code class=\"secret-key\">")
        .nth(1)
        .unwrap()
        .split("</code>")
        .next()
        .unwrap();
    let path = &link[link.find("/-/register").unwrap()..];
    let invite = path.split("invite=").nth(1).unwrap();
    let page = anon.get(path);
    assert_eq!(page.status, 200);
    assert!(page.body.contains("new@example.org"));
    let r = register(
        &mut anon,
        path,
        &[
            ("username", "newbie"),
            ("email", "other@example.org"),
            ("invite", invite),
        ],
    );
    assert_eq!(r.status, 422);
    let r = register(
        &mut anon,
        path,
        &[
            ("username", "newbie"),
            ("email", "new@example.org"),
            ("invite", invite),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        s.db.query_one("SELECT email_verified::text FROM users WHERE email = 'new@example.org'"),
        "true",
        "the invited address counts as confirmed"
    );
    // Single use.
    assert_eq!(s.client().get(path).status, 404);
    let list = boss.get("/-/admin/invites");
    assert!(list.body.contains("used by newbie"));

    // Revoked invitations stop working.
    let r = submit(&mut boss, "/-/admin/invites", "/-/admin/invites", &[]);
    let link = r
        .body
        .split("<code class=\"secret-key\">")
        .nth(1)
        .unwrap()
        .split("</code>")
        .next()
        .unwrap();
    let path = link[link.find("/-/register").unwrap()..].to_owned();
    assert_eq!(s.client().get(&path).status, 200);
    let id =
        s.db.query_one("SELECT id::text FROM invitations WHERE used_at IS NULL");
    assert_eq!(
        submit(
            &mut boss,
            "/-/admin/invites",
            &format!("/-/admin/invites/{id}/revoke"),
            &[]
        )
        .status,
        303
    );
    assert_eq!(s.client().get(&path).status, 404);
    // Others can't see or make invitations.
    let mut newbie = s.login("newbie", PW);
    assert_eq!(newbie.get("/-/admin/invites").status, 404);
}
