// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Organizations, membership, and the shared namespace.

use slath_tests::http::{Client, submit};
use slath_tests::server::TestServer;

const PW: &str = "correct horse battery staple";

fn create_org(c: &mut Client, name: &str) -> slath_tests::http::Resp {
    submit(
        c,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", name), ("display_name", "")],
    )
}

fn set_role(c: &mut Client, org: &str, user: &str, role: &str) -> slath_tests::http::Resp {
    submit(
        c,
        &format!("/{org}/-/members"),
        &format!("/{org}/-/members/{user}/role"),
        &[("role", role)],
    )
}

#[test]
fn conf_rbac_01_namespace_collisions_are_case_insensitive() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    for taken in ["alice", "ALICE", "Alice"] {
        let r = create_org(&mut a, taken);
        assert_eq!(r.status, 422, "{taken}");
        assert!(r.body.contains("This name is already taken."), "{taken}");
    }
    // Look-alikes and reserved names never become namespaces.
    for bad in [
        "\u{430}lice",
        "alice\u{200b}",
        "\u{ff41}lice",
        "alice.git",
        "-alice",
        "api",
        "assets",
        "-",
        "settings",
    ] {
        assert_eq!(create_org(&mut a, bad).status, 422, "{bad:?}");
    }
    assert_eq!(create_org(&mut a, "acme").status, 303);
    // A user can't take the org's name in any case either.
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "create-user",
            "--username",
            "ACME",
            "--email",
            "x@example.org",
            "--password-stdin",
        ],
        Some(PW),
    );
    assert!(!ok, "{out}");
    assert!(err.contains("already taken"), "{err}");
}

#[test]
fn conf_rbac_01_last_owner_cannot_leave_or_be_demoted() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("bob", PW, false);
    let mut a = s.login("alice", PW);
    assert_eq!(create_org(&mut a, "acme").status, 303);
    let r = set_role(&mut a, "acme", "alice", "member");
    assert_eq!(r.status, 422);
    assert!(r.body.contains("An organization needs at least one owner."));
    let r = submit(
        &mut a,
        "/acme/-/members",
        "/acme/-/members/alice/remove",
        &[],
    );
    assert_eq!(r.status, 422);
    // With a second owner, alice may step down and leave.
    let r = submit(
        &mut a,
        "/acme/-/members",
        "/acme/-/members",
        &[("username", "bob"), ("role", "owner")],
    );
    assert_eq!(r.status, 303);
    assert_eq!(set_role(&mut a, "acme", "alice", "member").status, 303);
    let r = submit(
        &mut a,
        "/acme/-/members",
        "/acme/-/members/alice/remove",
        &[],
    );
    assert_eq!(r.status, 303);
    let mut b = s.login("bob", PW);
    let r = submit(&mut b, "/acme/-/members", "/acme/-/members/bob/remove", &[]);
    assert_eq!(r.status, 422, "bob is now the last owner");
}

#[test]
fn conf_rbac_01_only_owners_manage_members_and_only_members_see_them() {
    let s = TestServer::start_with(&[]);
    for u in ["alice", "bob", "mallory"] {
        s.create_user(u, PW, false);
    }
    let mut a = s.login("alice", PW);
    create_org(&mut a, "acme");
    submit(
        &mut a,
        "/acme/-/members",
        "/acme/-/members",
        &[("username", "bob"), ("role", "member")],
    );
    let mut b = s.login("bob", PW);
    assert_eq!(b.get("/acme/-/members").status, 200);
    let token = slath_tests::http::csrf_token(&b.get("/acme/-/members").body).unwrap();
    let r = b.post_form(
        "/acme/-/members",
        &[
            ("_csrf", &token),
            ("username", "mallory"),
            ("role", "owner"),
        ],
    );
    assert_eq!(r.status, 403);
    let r = b.post_form(
        "/acme/-/members/alice/role",
        &[("_csrf", &token), ("role", "member")],
    );
    assert_eq!(r.status, 403);
    // Non-members and anonymous visitors see the profile, not the members.
    let mut m = s.login("mallory", PW);
    assert_eq!(m.get("/acme").status, 200);
    assert_eq!(m.get("/acme/-/members").status, 404);
    assert_eq!(s.client().get("/acme/-/members").status, 404);
    assert_eq!(m.get("/acme/-/settings").status, 404);
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM org_members"),
        "2"
    );
}

#[test]
fn profiles_render_and_unknown_namespaces_are_not_found() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    create_org(&mut a, "acme");
    let mut c = s.client();
    let u = c.get("/alice");
    assert_eq!(u.status, 200, "{}\n{}", u.body, s.log());
    assert!(u.body.contains("<h1>alice</h1>"));
    assert!(u.body.contains("href=\"/acme\""));
    assert_eq!(c.get("/ALICE").status, 200);
    let o = c.get("/acme");
    assert_eq!(o.status, 200);
    assert!(o.body.contains("<h1>acme</h1>"));
    assert_eq!(c.get("/nobody").status, 404);
    assert_eq!(
        c.get("/alice/-/members").status,
        404,
        "users have no members page"
    );
}

#[test]
fn org_creation_can_be_limited_to_site_admins() {
    let s = TestServer::start_full(&[], "[auth]\nusers_can_create_orgs = false\n");
    s.create_user("alice", PW, false);
    s.create_user("root-admin", PW, true);
    let mut a = s.login("alice", PW);
    assert_eq!(create_org(&mut a, "acme").status, 403);
    let mut r = s.login("root-admin", PW);
    assert_eq!(create_org(&mut r, "acme").status, 303);
}
