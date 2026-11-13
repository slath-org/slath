// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The audit log and site administration.

use slath_tests::http::{Client, create_repo, create_token, csrf_token, submit};
use slath_tests::server::TestServer;

const PW: &str = "correct horse battery staple";

fn events(s: &TestServer) -> usize {
    s.db.query_one("SELECT count(*)::text FROM audit_events")
        .parse()
        .unwrap()
}

fn last_action(s: &TestServer) -> String {
    s.db.query_one("SELECT action FROM audit_events ORDER BY created_at DESC, id DESC LIMIT 1")
}

/// Runs `f` and asserts it recorded exactly one event named `action`.
fn one_event(s: &TestServer, action: &str, f: impl FnOnce()) {
    let before = events(s);
    f();
    assert_eq!(events(s) - before, 1, "{action}: event count");
    assert_eq!(last_action(s), action);
}

fn ssh_key() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/auth/tests/data/ssh/ed25519.pub");
    std::fs::read_to_string(p).unwrap().trim().to_owned()
}

#[test]
fn conf_rbac_03_each_audited_action_writes_one_event_without_secrets() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("bob", PW, false);
    let mut a = s.client();
    one_event(&s, "login.success", || {
        assert_eq!(slath_tests::http::login(&mut a, "alice", PW).status, 303);
    });
    one_event(&s, "login.failure", || {
        slath_tests::http::login(&mut s.client(), "alice", "wrong password guess");
    });
    let mut token = String::new();
    one_event(&s, "token.create", || {
        token = create_token(&mut a, "ci", &["repo:read"])
    });
    one_event(&s, "token.revoke", || {
        let page = a.get("/-/settings/tokens").body;
        let id = page
            .split("/-/settings/tokens/")
            .nth(1)
            .and_then(|r| r.split('/').next())
            .unwrap()
            .to_owned();
        let r = submit(
            &mut a,
            "/-/settings/tokens",
            &format!("/-/settings/tokens/{id}/revoke"),
            &[],
        );
        assert_eq!(r.status, 303);
    });
    one_event(&s, "ssh_key.add", || {
        let r = submit(
            &mut a,
            "/-/settings/keys",
            "/-/settings/keys",
            &[("title", "laptop"), ("key", &ssh_key())],
        );
        assert_eq!(r.status, 303, "{}", r.body);
    });
    one_event(&s, "ssh_key.remove", || {
        let page = a.get("/-/settings/keys").body;
        let id = page
            .split("/-/settings/keys/")
            .nth(1)
            .and_then(|r| r.split('/').next())
            .unwrap()
            .to_owned();
        let r = submit(
            &mut a,
            "/-/settings/keys",
            &format!("/-/settings/keys/{id}/delete"),
            &[],
        );
        assert_eq!(r.status, 303);
    });
    one_event(&s, "repo.create", || {
        create_repo(&mut a, "alice", "proj", "private");
    });
    one_event(&s, "repo.visibility", || {
        let r = submit(
            &mut a,
            "/alice/proj/settings/general",
            "/alice/proj/settings/visibility",
            &[("visibility", "public")],
        );
        assert_eq!(r.status, 303);
    });
    one_event(&s, "repo.collaborator.set", || {
        let r = submit(
            &mut a,
            "/alice/proj/settings/access",
            "/alice/proj/settings/access",
            &[("username", "bob"), ("role", "write")],
        );
        assert_eq!(r.status, 303);
    });
    one_event(&s, "repo.rename", || {
        let r = submit(
            &mut a,
            "/alice/proj/settings/general",
            "/alice/proj/settings/rename",
            &[("name", "renamed")],
        );
        assert_eq!(r.status, 303);
    });
    one_event(&s, "org.create", || {
        let r = submit(
            &mut a,
            "/-/orgs/new",
            "/-/orgs",
            &[("name", "acme"), ("display_name", "")],
        );
        assert_eq!(r.status, 303);
    });
    one_event(&s, "password.change", || {
        let r = submit(
            &mut a,
            "/-/settings/security",
            "/-/settings/security/password",
            &[
                ("new_password", "a brand new passphrase"),
                ("confirm_password", "a brand new passphrase"),
            ],
        );
        assert_eq!(r.status, 303, "{}", r.body);
    });
    // Nothing secret ends up in event data.
    let dump =
        s.db.query_one("SELECT string_agg(action || ' ' || data::text, E'\\n') FROM audit_events");
    for secret in [
        PW,
        "wrong password guess",
        "a brand new passphrase",
        &token,
        &token[10..],
    ] {
        assert!(!dump.contains(secret), "secret in audit data:\n{dump}");
    }
    assert!(!dump.contains("slath_pat_"), "{dump}");
    let ips = s.db.query_one("SELECT count(*)::text FROM audit_events WHERE actor_ip IS NOT NULL AND action LIKE 'login.%'");
    assert_ne!(ips, "0", "login events carry the client address");
}

#[test]
fn conf_sec_admin_pages_are_not_found_for_everyone_else() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("boss", PW, true);
    let mut anon = s.client();
    let mut a = s.login("alice", PW);
    let mut r = s.login("boss", PW);
    let token = create_token(&mut r, "x", &["user:read", "repo:write"]);
    for path in [
        "/-/admin",
        "/-/admin/users",
        "/-/admin/orgs",
        "/-/admin/bots",
        "/-/admin/repos",
        "/-/admin/audit",
        "/-/admin/health",
    ] {
        assert_eq!(anon.get(path).status, 404, "{path}");
        assert_eq!(a.get(path).status, 404, "{path}");
        // An admin's token isn't a way into the admin pages.
        let resp = s
            .client()
            .get_with(path, &[("Authorization", &format!("Bearer {token}"))]);
        assert_eq!(resp.status, 404, "{path} with token");
        let ok = r.get(path);
        assert!(
            ok.status == 200 || ok.status == 303,
            "{path}: {}",
            ok.status
        );
    }
    // POSTs too, even with a valid CSRF token from another page.
    let csrf = csrf_token(&a.get("/-/settings/profile").body).unwrap();
    let resp = a.post_form("/-/admin/users/alice/grant-admin", &[("_csrf", &csrf)]);
    assert_eq!(resp.status, 404);
    let resp = a.post_form(
        "/-/admin/users",
        &[("_csrf", &csrf), ("username", "evil"), ("admin", "1")],
    );
    assert_eq!(resp.status, 404);
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM users WHERE is_admin"),
        "1"
    );
    // Suspended admins lose access as well.
    s.db.execute("UPDATE users SET state = 'suspended' WHERE namespace_id = (SELECT id FROM namespaces WHERE name = 'boss')");
    assert_ne!(r.get("/-/admin/users").status, 200);
}

fn admin_post(c: &mut Client, action: &str, fields: &[(&str, &str)]) -> slath_tests::http::Resp {
    submit(c, "/-/admin/users", action, fields)
}

#[test]
fn conf_sec_admin_suspending_revokes_sessions_and_tokens() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("boss", PW, true);
    let mut a = s.login("alice", PW);
    let token = create_token(&mut a, "x", &["user:read"]);
    let mut api = s.client();
    assert_eq!(
        api.api("GET", "/api/v1/user", Some(&token), None).status,
        200
    );
    let mut r = s.login("boss", PW);
    one_event(&s, "admin.user.suspend", || {
        let resp = admin_post(&mut r, "/-/admin/users/alice/suspend", &[]);
        assert_eq!(resp.status, 303);
    });
    assert!(
        !a.get("/").body.contains("action=\"/-/logout\""),
        "alice is signed out"
    );
    assert_eq!(
        api.api("GET", "/api/v1/user", Some(&token), None).status,
        401
    );
    assert_ne!(
        slath_tests::http::login(&mut s.client(), "alice", PW).status,
        303
    );
    one_event(&s, "admin.user.unsuspend", || {
        assert_eq!(
            admin_post(&mut r, "/-/admin/users/alice/unsuspend", &[]).status,
            303
        );
    });
    // Tokens stay revoked after reactivation; signing in works again.
    assert_eq!(
        api.api("GET", "/api/v1/user", Some(&token), None).status,
        401
    );
    assert_eq!(
        slath_tests::http::login(&mut s.client(), "alice", PW).status,
        303
    );
    one_event(&s, "admin.user.grant_admin", || {
        assert_eq!(
            admin_post(&mut r, "/-/admin/users/alice/grant-admin", &[]).status,
            303
        );
    });
    one_event(&s, "admin.user.revoke_admin", || {
        assert_eq!(
            admin_post(&mut r, "/-/admin/users/alice/revoke-admin", &[]).status,
            303
        );
    });
    // Admins can't lock themselves out.
    assert_eq!(
        admin_post(&mut r, "/-/admin/users/boss/suspend", &[]).status,
        409
    );
}

#[test]
fn conf_sec_admin_changes_need_sudo() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("boss", PW, true);
    let mut r = s.login("boss", PW);
    s.db.execute("UPDATE sessions SET sudo_until = NULL");
    let resp = admin_post(&mut r, "/-/admin/users/alice/suspend", &[]);
    assert_eq!(resp.status, 303);
    assert!(
        resp.header("location")
            .unwrap()
            .starts_with("/-/sudo?next=")
    );
    assert_eq!(s.db.query_one("SELECT state::text FROM users WHERE namespace_id = (SELECT id FROM namespaces WHERE name = 'alice')"), "active");
}

#[test]
fn admins_create_users_and_restore_deleted_repositories() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("boss", PW, true);
    let mut r = s.login("boss", PW);
    let resp = admin_post(
        &mut r,
        "/-/admin/users",
        &[("username", "carol"), ("email", "carol@example.org")],
    );
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(resp.header("cache-control"), Some("no-store"));
    let start = resp.body.find("id=\"new-password\"").unwrap();
    let value = resp.body[start..]
        .split("value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned();
    assert!(value.len() >= 20);
    assert_eq!(
        slath_tests::http::login(&mut s.client(), "carol", &value).status,
        303
    );
    // Duplicate names are a form error.
    let resp = admin_post(&mut r, "/-/admin/users", &[("username", "CAROL")]);
    assert_eq!(resp.status, 422);

    let mut a = s.login("alice", PW);
    create_repo(&mut a, "alice", "doomed", "private");
    let resp = submit(
        &mut a,
        "/alice/doomed/settings/danger",
        "/alice/doomed/settings/danger",
        &[("confirm", "alice/doomed")],
    );
    assert_eq!(resp.status, 303);
    assert_eq!(a.get("/alice/doomed").status, 404);
    let page = r.get("/-/admin/repos").body;
    let id = page
        .split("/-/admin/repos/")
        .nth(1)
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .to_owned();
    one_event(&s, "repo.restore", || {
        let resp = submit(
            &mut r,
            "/-/admin/repos",
            &format!("/-/admin/repos/{id}/restore"),
            &[],
        );
        assert_eq!(resp.status, 303);
    });
    assert_eq!(a.get("/alice/doomed").status, 200);
    // The audit page shows and filters events.
    let audit = r.get("/-/admin/audit?action=repo.");
    assert_eq!(audit.status, 200);
    assert!(audit.body.contains("repo.restore"));
    assert!(!audit.body.contains("login.success"));
    assert_eq!(r.get("/-/admin/health").status, 200);
}

#[test]
fn conf_rbac_03_org_owners_see_their_organizations_audit_log_only() {
    let s = TestServer::start_with(&[]);
    for u in ["alice", "bob"] {
        s.create_user(u, PW, false);
    }
    let mut a = s.login("alice", PW);
    let mut b = s.login("bob", PW);
    for (c, org) in [(&mut a, "acme"), (&mut b, "other")] {
        let r = submit(
            c,
            "/-/orgs/new",
            "/-/orgs",
            &[("name", org), ("display_name", "")],
        );
        assert_eq!(r.status, 303);
    }
    let r = submit(
        &mut a,
        "/acme/-/members",
        "/acme/-/members",
        &[("username", "bob"), ("role", "member")],
    );
    assert_eq!(r.status, 303);
    let page = a.get("/acme/-/audit");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("org.member.set"));
    assert!(
        !page.body.contains("login.success"),
        "only this org's events"
    );
    assert_eq!(a.get("/other/-/audit").status, 404);
    assert_eq!(b.get("/acme/-/audit").status, 404, "members aren't owners");
    assert_eq!(s.client().get("/acme/-/audit").status, 404);
    assert_eq!(a.get("/alice/-/audit").status, 404);
}

#[test]
fn conf_ops_07_admins_see_and_control_background_migrations() {
    let s = TestServer::start_with(&[]);
    s.create_user("boss", PW, true);
    s.create_user("alice", PW, false);
    s.db.execute(
        "INSERT INTO background_migrations (name, introduced_in, state, processed, total_estimate, started_at)
         VALUES ('demo_backfill', '0.2.0', 'paused', 40, 100, now() - interval '1 minute')",
    );
    let mut a = s.client();
    slath_tests::http::login(&mut a, "alice", PW);
    assert_eq!(a.get("/-/admin/upgrades").status, 404);
    let r = a.post_form("/-/admin/upgrades/background/demo_backfill/resume", &[]);
    assert!(r.status == 404 || r.status == 403, "{}", r.status);

    let mut boss = s.client();
    slath_tests::http::login(&mut boss, "boss", PW);
    let page = boss.get("/-/admin/upgrades");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("demo_backfill"));
    assert!(page.body.contains("40%"));
    one_event(&s, "background_migration.resume", || {
        let r = submit(
            &mut boss,
            "/-/admin/upgrades",
            "/-/admin/upgrades/background/demo_backfill/resume",
            &[],
        );
        assert_eq!(r.status, 303, "{}", r.body);
    });
    assert_eq!(
        s.db.query_one("SELECT state FROM background_migrations WHERE name = 'demo_backfill'"),
        "pending"
    );
    // A control that doesn't apply to the current state is a conflict.
    let r = submit(
        &mut boss,
        "/-/admin/upgrades",
        "/-/admin/upgrades/background/demo_backfill/retry",
        &[],
    );
    assert_eq!(r.status, 409);
    let r = submit(
        &mut boss,
        "/-/admin/upgrades",
        "/-/admin/upgrades/background/nope/pause",
        &[],
    );
    assert_eq!(r.status, 404);
}
