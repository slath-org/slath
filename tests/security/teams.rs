// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Teams and organization policies (RBAC-01): team grants, the effective
//! role as the highest grant, immediate revocation, the organization MFA
//! requirement, and teams staying within their organization.

use slath_tests::http::{Client, create_repo, create_token, submit};
use slath_tests::server::{GitClient, TestServer};

const PW: &str = "correct horse battery staple";

fn post(c: &mut Client, page: &str, action: &str, fields: &[(&str, &str)]) -> u16 {
    submit(c, page, action, fields).status
}

/// acme (owned by alice) with the private repository acme/secret, and bob
/// as a plain member (no default access).
fn setup() -> (TestServer, Client, Client) {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("bob", PW, false);
    let mut a = s.login("alice", PW);
    assert_eq!(
        post(
            &mut a,
            "/-/orgs/new",
            "/-/orgs",
            &[("name", "acme"), ("display_name", "")]
        ),
        303
    );
    assert_eq!(
        post(
            &mut a,
            "/acme/-/members",
            "/acme/-/members",
            &[("username", "bob"), ("role", "member")]
        ),
        303
    );
    create_repo(&mut a, "acme", "secret", "private");
    let b = s.login("bob", PW);
    (s, a, b)
}

#[test]
fn conf_rbac_01_team_access_is_granted_and_revoked_immediately() {
    let (s, mut a, mut b) = setup();
    assert_eq!(b.get("/acme/secret").status, 404);
    assert_eq!(
        post(
            &mut a,
            "/acme/-/teams",
            "/acme/-/teams",
            &[("name", "devs"), ("description", "Developers")]
        ),
        303
    );
    assert_eq!(
        post(
            &mut a,
            "/acme/-/teams/devs",
            "/acme/-/teams/devs/members",
            &[("username", "bob")]
        ),
        303
    );
    // Membership alone gives nothing; the team's repository grant does.
    assert_eq!(b.get("/acme/secret").status, 404);
    assert_eq!(
        post(
            &mut a,
            "/acme/secret/settings/access",
            "/acme/secret/settings/access/teams",
            &[("team", "devs"), ("role", "read")]
        ),
        303
    );
    assert_eq!(b.get("/acme/secret").status, 200);
    assert!(b.get("/acme").body.contains("secret"), "listed for bob");
    assert!(b.get("/acme/-/teams/devs").body.contains("acme/secret"));
    // Members see teams but can't change them.
    assert_eq!(
        post(
            &mut b,
            "/acme/-/teams",
            "/acme/-/teams",
            &[("name", "mine")]
        ),
        404
    );
    // Deleting the team ends the access on the next request.
    assert_eq!(
        post(
            &mut a,
            "/acme/-/teams/devs",
            "/acme/-/teams/devs/delete",
            &[]
        ),
        303
    );
    assert_eq!(b.get("/acme/secret").status, 404);
    assert!(!b.get("/acme").body.contains("secret"));

    // Leaving the organization also leaves its teams.
    assert_eq!(
        post(&mut a, "/acme/-/teams", "/acme/-/teams", &[("name", "ops")]),
        303
    );
    assert_eq!(
        post(
            &mut a,
            "/acme/-/teams/ops",
            "/acme/-/teams/ops/members",
            &[("username", "bob")]
        ),
        303
    );
    assert_eq!(
        post(&mut a, "/acme/-/members", "/acme/-/members/bob/remove", &[]),
        303
    );
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM team_members"),
        "0"
    );
    // Outsiders don't see teams at all.
    assert_eq!(b.get("/acme/-/teams").status, 404);
    assert_eq!(s.client().get("/acme/-/teams/ops").status, 404);
}

#[test]
fn conf_rbac_01_effective_role_is_the_highest_grant() {
    let (s, mut a, mut b) = setup();
    // A read collaborator in a write team can push.
    assert_eq!(
        post(
            &mut a,
            "/acme/secret/settings/access",
            "/acme/secret/settings/access",
            &[("username", "bob"), ("role", "read")]
        ),
        303
    );
    assert_eq!(
        post(
            &mut a,
            "/acme/-/teams",
            "/acme/-/teams",
            &[("name", "devs")]
        ),
        303
    );
    assert_eq!(
        post(
            &mut a,
            "/acme/-/teams/devs",
            "/acme/-/teams/devs/members",
            &[("username", "bob")]
        ),
        303
    );
    let token = create_token(&mut b, "push", &["repo:write"]);
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git.ok(&src, &["init", "-q", "-b", "main"]);
    std::fs::write(src.join("a.txt"), "a\n").unwrap();
    git.ok(&src, &["add", "."]);
    git.ok(&src, &["commit", "-q", "-m", "a"]);
    let url = format!("http://bob:{token}@{}/acme/secret.git", s.addr);
    let err = git.fails(&src, &["push", "-q", &url, "main"]);
    assert!(!err.is_empty(), "read only: the push is refused");
    assert_eq!(
        post(
            &mut a,
            "/acme/secret/settings/access",
            "/acme/secret/settings/access/teams",
            &[("team", "devs"), ("role", "write")]
        ),
        303
    );
    git.ok(&src, &["push", "-q", &url, "main"]);
    // Admin through the team: the settings open.
    assert_eq!(b.get("/acme/secret/settings/general").status, 404);
    assert_eq!(
        post(
            &mut a,
            "/acme/secret/settings/access",
            "/acme/secret/settings/access/teams",
            &[("team", "devs"), ("role", "admin")]
        ),
        303
    );
    assert_eq!(b.get("/acme/secret/settings/general").status, 200);
}

#[test]
fn conf_rbac_01_org_mfa_requirement_blocks_members_without_it() {
    let (s, mut a, mut b) = setup();
    assert_eq!(
        post(
            &mut a,
            "/acme/secret/settings/access",
            "/acme/secret/settings/access",
            &[("username", "bob"), ("role", "write")]
        ),
        303
    );
    assert_eq!(b.get("/acme/secret").status, 200);
    assert_eq!(
        post(
            &mut a,
            "/acme/-/settings",
            "/acme/-/settings/policies",
            &[("default_member_role", "none"), ("require_mfa", "1")]
        ),
        303
    );
    assert_eq!(
        b.get("/acme/secret").status,
        404,
        "blocked without a second factor"
    );
    assert!(!b.get("/acme").body.contains("secret"));
    // With a second factor (any confirmed one), access is back.
    s.db.execute(
        "INSERT INTO totp_credentials (user_id, secret_enc, key_id, confirmed)
         SELECT u.id, '\\x00', (SELECT key_id FROM encryption_keys LIMIT 1), true
         FROM users u JOIN namespaces n ON n.id = u.namespace_id WHERE n.name = 'bob'",
    );
    assert_eq!(b.get("/acme/secret").status, 200);
    // Read for every member.
    assert_eq!(
        post(
            &mut a,
            "/acme/-/settings",
            "/acme/-/settings/policies",
            &[("default_member_role", "read")]
        ),
        303
    );
    assert_eq!(
        s.db.query_one("SELECT default_member_role || require_mfa::text FROM organizations"),
        "readfalse"
    );
}

#[test]
fn teams_stay_in_their_organization() {
    let (_s, mut a, mut b) = setup();
    assert_eq!(
        post(
            &mut b,
            "/-/orgs/new",
            "/-/orgs",
            &[("name", "evil"), ("display_name", "")]
        ),
        303
    );
    assert_eq!(
        post(
            &mut b,
            "/evil/-/teams",
            "/evil/-/teams",
            &[("name", "devs")]
        ),
        303
    );
    // acme's admin can't grant another organization's team, even by name.
    let r = submit(
        &mut a,
        "/acme/secret/settings/access",
        "/acme/secret/settings/access/teams",
        &[("team", "devs"), ("role", "admin")],
    );
    assert_eq!(r.status, 422);
    // Only members of the organization can join its teams.
    assert_eq!(
        post(
            &mut b,
            "/evil/-/teams/devs",
            "/evil/-/teams/devs/members",
            &[("username", "alice")]
        ),
        422
    );
    // A user's repository has no team access.
    create_repo(&mut a, "alice", "own", "private");
    assert!(
        !a.get("/alice/own/settings/access")
            .body
            .contains("Give a team access")
    );
}
