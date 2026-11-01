// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Repository lifecycle and visibility.

use slath_tests::http::{Client, Resp, submit};
use slath_tests::server::TestServer;

pub const PW: &str = "correct horse battery staple";

/// Creates a repository through the UI.
pub fn create_repo(c: &mut Client, ns: &str, name: &str, visibility: &str) -> Resp {
    let r = submit(
        c,
        "/-/new",
        "/-/new",
        &[
            ("namespace", ns),
            ("name", name),
            ("description", ""),
            ("visibility", visibility),
            ("default_branch", "main"),
        ],
    );
    assert_eq!(r.status, 303, "create {ns}/{name}: {}", r.body);
    r
}

/// Status and body with per-response values (nonces, tokens, IDs) blanked.
pub fn normalized(r: &Resp) -> (u16, String) {
    let mut out = String::new();
    let mut rest = r.body.as_str();
    while let Some(i) = rest.find(['"', '>']) {
        out.push_str(&rest[..=i]);
        rest = &rest[i + 1..];
        let end = rest.find(['"', '<']).unwrap_or(rest.len());
        let value = &rest[..end];
        let random = value.len() >= 20
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=_-".contains(&b));
        out.push_str(if random { "*" } else { value });
        rest = &rest[end..];
    }
    out.push_str(rest);
    (r.status, out)
}

fn setup() -> (TestServer, Client) {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("bob", PW, false);
    let a = s.login("alice", PW);
    (s, a)
}

#[test]
fn conf_git_03_private_repos_are_indistinguishable_from_missing_ones() {
    let (s, mut a) = setup();
    create_repo(&mut a, "alice", "secret", "private");
    create_repo(&mut a, "alice", "open", "public");
    let mut bob = s.login("bob", PW);
    let mut anon = s.client();
    for c in [&mut bob, &mut anon] {
        for suffix in [
            "",
            "/settings/general",
            "/settings/access",
            "/settings/danger",
        ] {
            let private = c.get(&format!("/alice/secret{suffix}"));
            let missing = c.get(&format!("/alice/nothere{suffix}"));
            assert_eq!(private.status, 404, "{suffix}");
            // The pages echo the requested URL (sign-in link); nothing else differs.
            let (p, m) = (normalized(&private), normalized(&missing));
            assert_eq!(
                (p.0, p.1.replace("secret", "X")),
                (m.0, m.1.replace("nothere", "X")),
                "{suffix}"
            );
            assert_eq!(private.header("location"), None);
        }
        assert_eq!(c.get("/alice/open").status, 200);
    }
    assert_eq!(a.get("/alice/secret").status, 200);
    // Profile lists show only what the viewer may see.
    assert!(!anon.get("/alice").body.contains("secret"));
    assert!(anon.get("/alice").body.contains("alice/open"));
    assert!(a.get("/alice").body.contains("alice/secret"));
    assert!(!anon.get("/").body.contains("secret"));
}

#[test]
fn conf_git_04_new_repository_is_initialized_on_disk() {
    let (s, mut a) = setup();
    create_repo(&mut a, "alice", "proj", "private");
    let id =
        s.db.query_one("SELECT id::text FROM repositories WHERE name = 'proj'");
    let path = s
        .data_dir()
        .join("repos")
        .join(&id[..2])
        .join(format!("{id}.git"));
    assert!(path.join("HEAD").is_file());
    let cfg = std::fs::read_to_string(path.join("config")).unwrap();
    assert!(cfg.contains("fsckObjects = true"), "{cfg}");
    assert!(
        std::fs::read_dir(s.data_dir().join("repos").join(".tmp"))
            .unwrap()
            .next()
            .is_none(),
        "no leftovers"
    );
    let home = a.get("/alice/proj");
    assert!(home.body.contains("This repository is empty"));
    assert!(home.body.contains(&format!("{}/alice/proj.git", s.base())));
    // Duplicate names fail case-insensitively.
    let r = submit(
        &mut a,
        "/-/new",
        "/-/new",
        &[
            ("namespace", "alice"),
            ("name", "PROJ"),
            ("visibility", "private"),
        ],
    );
    assert_eq!(r.status, 422);
    // Nobody creates repositories in someone else's namespace.
    let mut bob = s.login("bob", PW);
    let r = submit(
        &mut bob,
        "/-/new",
        "/-/new",
        &[
            ("namespace", "alice"),
            ("name", "intruder"),
            ("visibility", "public"),
        ],
    );
    assert_eq!(r.status, 422);
    assert!(r.body.contains("You can"), "{}", r.body);
}

#[test]
fn conf_rbac_01_rename_redirects_only_readers() {
    let (s, mut a) = setup();
    create_repo(&mut a, "alice", "secret", "private");
    create_repo(&mut a, "alice", "open", "public");
    for (old, new) in [("secret", "hidden"), ("open", "shown")] {
        let r = submit(
            &mut a,
            &format!("/alice/{old}/settings/general"),
            &format!("/alice/{old}/settings/rename"),
            &[("name", new)],
        );
        assert_eq!(r.status, 303, "{}", r.body);
    }
    // The owner follows the redirect, keeping the rest of the path.
    let r = a.get("/alice/secret/settings/general?x=1");
    assert_eq!(r.status, 301);
    assert_eq!(
        r.header("location"),
        Some("/alice/hidden/settings/general?x=1")
    );
    // Others learn nothing about the private repository's new name.
    let mut bob = s.login("bob", PW);
    for c in [&mut bob, &mut s.client()] {
        let r = c.get("/alice/secret");
        assert_eq!(r.status, 404);
        assert_eq!(r.header("location"), None);
        assert!(!r.body.contains("alice/hidden"));
        // Public repositories redirect everyone.
        let r = c.get("/alice/open");
        assert_eq!(r.status, 301);
        assert_eq!(r.header("location"), Some("/alice/shown"));
    }
    // Creating a repository with the old name takes it over.
    create_repo(&mut a, "alice", "open", "private");
    assert_eq!(a.get("/alice/open").status, 200);
}

#[test]
fn conf_git_03_deleted_repository_is_invisible_immediately() {
    let (s, mut a) = setup();
    create_repo(&mut a, "alice", "doomed", "public");
    let id =
        s.db.query_one("SELECT id::text FROM repositories WHERE name = 'doomed'");
    // A wrong confirmation does nothing.
    let r = submit(
        &mut a,
        "/alice/doomed/settings/danger",
        "/alice/doomed/settings/danger",
        &[("confirm", "doomed")],
    );
    assert_eq!(r.status, 422);
    let r = submit(
        &mut a,
        "/alice/doomed/settings/danger",
        "/alice/doomed/settings/danger",
        &[("confirm", "alice/doomed")],
    );
    assert_eq!(r.status, 303);
    assert_eq!(a.get("/alice/doomed").status, 404);
    assert_eq!(s.client().get("/alice/doomed").status, 404);
    assert!(!s.client().get("/").body.contains("doomed"));
    // A job moves the data to the trash.
    let trash = s.data_dir().join("trash").join(format!("{id}.git"));
    for _ in 0..100 {
        if trash.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(trash.join("HEAD").is_file(), "moved to the trash");
}

#[test]
fn conf_rbac_01_collaborators_gain_and_lose_access() {
    let (s, mut a) = setup();
    create_repo(&mut a, "alice", "team", "private");
    let mut bob = s.login("bob", PW);
    assert_eq!(bob.get("/alice/team").status, 404);
    let r = submit(
        &mut a,
        "/alice/team/settings/access",
        "/alice/team/settings/access",
        &[("username", "bob"), ("role", "write")],
    );
    assert_eq!(r.status, 303);
    assert_eq!(bob.get("/alice/team").status, 200);
    // A writer isn't an administrator: settings stay hidden.
    assert_eq!(bob.get("/alice/team/settings/general").status, 404);
    let token = slath_tests::http::csrf_token(&bob.get("/alice/team").body).unwrap();
    let r = bob.post_form(
        "/alice/team/settings/rename",
        &[("_csrf", &token), ("name", "stolen")],
    );
    assert_eq!(r.status, 404);
    let r = submit(
        &mut a,
        "/alice/team/settings/access",
        "/alice/team/settings/access/bob/remove",
        &[],
    );
    assert_eq!(r.status, 303);
    assert_eq!(bob.get("/alice/team").status, 404);
}

#[test]
fn conf_auth_03_visibility_change_needs_sudo() {
    let (s, mut a) = setup();
    create_repo(&mut a, "alice", "proj", "private");
    s.db.execute("UPDATE sessions SET sudo_until = NULL");
    let r = submit(
        &mut a,
        "/alice/proj/settings/general",
        "/alice/proj/settings/visibility",
        &[("visibility", "public")],
    );
    assert_eq!(r.status, 303);
    assert!(r.header("location").unwrap().starts_with("/-/sudo"));
    assert_eq!(s.client().get("/alice/proj").status, 404, "still private");
    let r = submit(
        &mut a,
        "/alice/proj/settings/danger",
        "/alice/proj/settings/danger",
        &[("confirm", "alice/proj")],
    );
    assert!(r.header("location").unwrap().starts_with("/-/sudo"));
    assert_eq!(a.get("/alice/proj").status, 200, "not deleted");
}

#[test]
fn archived_repositories_show_a_notice() {
    let (_s, mut a) = setup();
    create_repo(&mut a, "alice", "old", "public");
    let r = submit(
        &mut a,
        "/alice/old/settings/general",
        "/alice/old/settings/state/archive",
        &[],
    );
    assert_eq!(r.status, 303);
    assert!(
        a.get("/alice/old")
            .body
            .contains("This repository is archived.")
    );
    let r = submit(
        &mut a,
        "/alice/old/settings/general",
        "/alice/old/settings/state/unarchive",
        &[],
    );
    assert_eq!(r.status, 303);
    assert!(
        !a.get("/alice/old")
            .body
            .contains("This repository is archived.")
    );
}

#[test]
fn org_members_create_repositories_in_their_org() {
    let (s, mut a) = setup();
    submit(
        &mut a,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", "acme"), ("display_name", "")],
    );
    submit(
        &mut a,
        "/acme/-/members",
        "/acme/-/members",
        &[("username", "bob"), ("role", "member")],
    );
    let mut bob = s.login("bob", PW);
    create_repo(&mut bob, "acme", "tool", "private");
    // Bob is a plain member (no default access), but created it; the org
    // owner administers it.
    assert_eq!(a.get("/acme/tool/settings/general").status, 200);
}
