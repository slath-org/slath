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
    // Webhooks need a reachable address; the fixture's points at the host.
    let s = TestServer::start_full(
        &[DEMO],
        "[webhooks]\nallow_http = true\nallow_networks = [\"127.0.0.0/8\"]\n",
    );
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

    // Since 0.3: merge requests (one merged, one open with a line thread,
    // a reply, an approval, and a commit status), a fork, a webhook, a
    // deploy key, sensitive paths, and an organization bot with a token.
    let ta = create_token(
        &mut a,
        "fixture-0.3",
        &["repo:write", "mrs:write", "statuses:write", "user:write"],
    );
    let tb = create_token(&mut b, "fixture-0.3", &["repo:write", "mrs:write"]);
    let mut call = |m: &str, path: &str, token: &str, body: serde_json::Value| {
        let body = (!body.is_null()).then_some(body);
        let r = c.api(m, &format!("/api/v1{path}"), Some(token), body.as_ref());
        assert!(r.status < 300, "{m} {path}: {} {}", r.status, r.body);
        r.json()
    };
    let url = s.url(&DEMO);
    for (branch, file, text, message) in [
        ("docs", "README.md", "# Demo\n", "Add a README"),
        ("fix-a", "a.txt", "3\n", "Count to three"),
    ] {
        git.ok(&src, &["switch", "-q", "-c", branch, "main"]);
        std::fs::write(src.join(file), text).unwrap();
        git.ok(&src, &["add", "."]);
        git.ok(&src, &["commit", "-q", "-m", message]);
        git.ok(&src, &["push", "-q", &url, branch]);
    }
    let mut mr = |branch: &str, title: &str| {
        let v = call(
            "POST",
            "/repos/alice/demo/merge-requests",
            &ta,
            json!({"source_branch": branch, "title": title, "description": "Fixes #2."}),
        );
        let n = v["number"].as_i64().unwrap();
        let start = std::time::Instant::now();
        loop {
            let v = call(
                "GET",
                &format!("/repos/alice/demo/merge-requests/{n}"),
                &ta,
                serde_json::Value::Null,
            );
            if v["revisions"] == 1 {
                return (n, v["head_oid"].as_str().unwrap().to_owned());
            }
            assert!(start.elapsed().as_secs() < 30, "!{n}: no revision");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    };
    let (merged, head) = mr("docs", "Add a README");
    let (open, open_head) = mr("fix-a", "Count to three");
    call(
        "POST",
        &format!("/repos/alice/demo/merge-requests/{merged}/reviews"),
        &tb,
        json!({"verdict": "approve"}),
    );
    call(
        "POST",
        &format!("/repos/alice/demo/merge-requests/{merged}/merge"),
        &ta,
        json!({"strategy": "merge-commit", "expected_head_oid": head}),
    );
    let mrs = format!("/repos/alice/demo/merge-requests/{open}");
    let thread = call(
        "POST",
        &format!("{mrs}/threads"),
        &tb,
        json!({"body": "Why three?", "path": "a.txt", "side": "new", "line_start": 1, "line_end": 1}),
    );
    let thread = thread["id"].as_str().unwrap().to_owned();
    call(
        "POST",
        &format!("{mrs}/threads/{thread}/comments"),
        &ta,
        json!({"body": "The tests count to three."}),
    );
    call(
        "PUT",
        &format!("{mrs}/reviewers"),
        &ta,
        json!({"reviewers": ["bob"]}),
    );
    call(
        "POST",
        &format!("{mrs}/reviews"),
        &tb,
        json!({"verdict": "approve", "body": "Looks good."}),
    );
    call(
        "POST",
        &format!("/repos/alice/demo/statuses/{open_head}"),
        &ta,
        json!({"state": "success", "context": "ci/build", "description": "Build passed"}),
    );
    call(
        "POST",
        "/repos/acme/tools/forks",
        &tb,
        json!({"namespace": "bob"}),
    );
    call(
        "POST",
        "/repos/alice/demo/webhooks",
        &ta,
        json!({"url": "http://127.0.0.1:9/hook", "events": ["push", "merge_request"], "secret": "a shared secret of some length"}),
    );
    let (_, deploy) = slath_tests::sshd::keygen(work.path(), "deploy");
    call(
        "POST",
        "/repos/alice/demo/keys",
        &ta,
        json!({"key": deploy.trim(), "title": "deploy"}),
    );
    call(
        "PUT",
        "/repos/alice/demo/sensitive-paths",
        &ta,
        json!({"paths": ["deploy/**"]}),
    );
    call(
        "POST",
        "/orgs/acme/bots",
        &ta,
        json!({"username": "acme-ci", "display_name": "Acme CI"}),
    );
    let r = submit(
        &mut a,
        "/-/bots/acme-ci",
        "/-/bots/acme-ci/tokens",
        &[
            ("name", "ci"),
            ("repos", "acme/tools"),
            ("scopes", "repo:read"),
            ("expires", "30"),
        ],
    );
    assert_eq!(r.status, 200, "{}", r.body);

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
