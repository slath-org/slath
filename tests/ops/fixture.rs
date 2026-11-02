// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Writes the schema and data fixture of a release
//! (`tests/fixtures/schema/v<version>.sql`), the starting point of future
//! upgrade tests. Run with `just schema-fixture` when preparing a release.

use serde_json::json;
use slath_tests::fixtures;
use slath_tests::http::{create_repo, create_token, submit};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

#[test]
#[ignore = "writes a release fixture; run with `just schema-fixture`"]
fn write_schema_fixture() {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    s.create_user("root-admin", PASSWORD, true);
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("tags", &src, &[]);
    git.ok(
        &src,
        &[
            "push",
            "-q",
            &s.url(&DEMO),
            "refs/heads/*:refs/heads/*",
            "refs/tags/*:refs/tags/*",
        ],
    );
    let mut a = s.login("alice", PASSWORD);
    assert_eq!(
        submit(
            &mut a,
            "/-/orgs/new",
            "/-/orgs",
            &[("name", "acme"), ("display_name", "Acme")]
        )
        .status,
        303
    );
    assert_eq!(
        submit(
            &mut a,
            "/acme/-/members",
            "/acme/-/members",
            &[("username", "bob"), ("role", "member")]
        )
        .status,
        303
    );
    create_repo(&mut a, "acme", "tools", "public");
    assert_eq!(
        submit(
            &mut a,
            "/alice/demo/settings/access",
            "/alice/demo/settings/access",
            &[("username", "bob"), ("role", "write")]
        )
        .status,
        303
    );
    create_repo(&mut a, "alice", "renamed", "private");
    assert_eq!(
        submit(
            &mut a,
            "/alice/renamed/settings/general",
            "/alice/renamed/settings/rename",
            &[("name", "new-name")]
        )
        .status,
        303
    );
    let (_, public) = slath_tests::sshd::keygen(work.path(), "bob");
    s.add_ssh_key("bob", &public);
    let mut b = s.login("bob", PASSWORD);
    let _ = create_token(&mut b, "ci", &["repo:read"]);

    // Since 0.2: an internal repository, a team with a repository role,
    // branch protection, and issues with labels, a milestone, an assignee,
    // comments, a mention, and a closed issue.
    create_repo(&mut a, "alice", "inside", "internal");
    for (page, action, fields) in [
        (
            "/acme/-/teams",
            "/acme/-/teams",
            &[("name", "core"), ("description", "Core team")][..],
        ),
        (
            "/acme/-/teams/core",
            "/acme/-/teams/core/members",
            &[("username", "bob")][..],
        ),
        (
            "/acme/tools/settings/access",
            "/acme/tools/settings/access/teams",
            &[("team", "core"), ("role", "write")][..],
        ),
        (
            "/alice/demo/settings/protections",
            "/alice/demo/settings/protections",
            &[
                ("pattern", "main"),
                ("push_role", "maintain"),
                ("block_force_push", "1"),
            ][..],
        ),
    ] {
        let r = submit(&mut a, page, action, fields);
        assert_eq!(r.status, 303, "{action}: {}", r.body);
    }
    let token = create_token(&mut a, "fixture", &["issues:write", "repo:read"]);
    let mut c = s.client();
    let mut api = |m: &str, path: &str, body: serde_json::Value| {
        let r = c.api(
            m,
            &format!("/api/v1/repos/alice/demo{path}"),
            Some(&token),
            Some(&body),
        );
        assert!(r.status < 300, "{m} {path}: {} {}", r.status, r.body);
    };
    api(
        "POST",
        "/milestones",
        json!({"title": "v1.0", "due_on": "2027-01-31"}),
    );
    api(
        "POST",
        "/issues",
        json!({"title": "Crash when the config is empty", "body": "Steps:\n\n1. Start with an empty file.\n\n@bob, can you look?"}),
    );
    api(
        "PATCH",
        "/issues/1",
        json!({"labels": ["bug", "good first issue"], "milestone": "v1.0", "assignees": ["bob"]}),
    );
    api(
        "POST",
        "/issues",
        json!({"title": "Document the config file", "body": ""}),
    );
    api(
        "PATCH",
        "/issues/2",
        json!({"state": "closed", "state_reason": "completed"}),
    );
    let r = submit(
        &mut b,
        "/alice/demo/issues/1",
        "/alice/demo/issues/1/comments",
        &[("body", "On it, thanks @alice. See #2.")],
    );
    assert_eq!(r.status, 303, "{}", r.body);

    let engine = std::env::var("SLATH_CONTAINER_ENGINE").unwrap_or_else(|_| "docker".to_owned());
    let container = std::env::var("SLATH_DEV_DB_CONTAINER")
        .unwrap_or_else(|_| "slath-dev-postgres-1".to_owned());
    let out = std::process::Command::new(engine)
        .args([
            "exec",
            &container,
            "pg_dump",
            "-U",
            "slath",
            "--no-owner",
            "--no-privileges",
            "-d",
        ])
        .arg(s.db.name())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/schema");
    std::fs::create_dir_all(&dir).unwrap();
    let header = format!(
        "-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>\n\
         -- SPDX-License-Identifier: AGPL-3.0-or-later\n\
         --\n\
         -- Schema and data of Slath {} (schema {}), written by `just schema-fixture`.\n\
         -- Users: alice, bob, root-admin (password \"{PASSWORD}\"). Don't edit.\n\n",
        env!("CARGO_PKG_VERSION"),
        slath_db_schema(&s),
    );
    let path = dir.join(format!("v{}.sql", env!("CARGO_PKG_VERSION")));
    std::fs::write(&path, header + &String::from_utf8(out.stdout).unwrap()).unwrap();
    eprintln!("wrote {}", path.display());
}

fn slath_db_schema(s: &TestServer) -> String {
    s.db.query_one("SELECT version::text FROM schema_info")
}
