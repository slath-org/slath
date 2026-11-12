// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plain-text diffs and patches of commits and merge requests: the right
//! content type, and the same authorization as the pages and the API.

use serde_json::json;
use slath_tests::http::create_token;
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

#[test]
fn conf_rbac_01_plain_text_diffs_and_patches_follow_repository_access() {
    let s = TestServer::start();
    s.create_user("mallory", PASSWORD, false);
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git.ok(&src, &["init", "-q", "-b", "main"]);
    std::fs::write(src.join("a.txt"), "one\n").unwrap();
    git.ok(&src, &["add", "."]);
    git.ok(&src, &["commit", "-q", "-m", "Add a"]);
    git.ok(&src, &["push", "-q", &s.url(&DEMO), "main"]);
    std::fs::write(src.join("a.txt"), "two\n").unwrap();
    git.ok(&src, &["commit", "-q", "-am", "Change a"]);
    git.ok(&src, &["push", "-q", &s.url(&DEMO), "main:topic"]);
    let head = git.ok(&src, &["rev-parse", "HEAD"]).trim().to_owned();
    let root = git.ok(&src, &["rev-parse", "HEAD~"]).trim().to_owned();

    let mut alice = s.login("alice", PASSWORD);
    let token = create_token(&mut alice, "mr", &["repo:write", "mrs:write"]);
    let mut api = s.client();
    let r = api.api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&token),
        Some(&json!({"source_branch": "topic", "title": "Change a"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();

    let plain = |r: &slath_tests::http::Resp| {
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.header("content-type"), Some("text/plain; charset=utf-8"));
        assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(r.header("cache-control"), Some("private, no-store"));
    };
    // In the web UI with a session.
    let r = alice.get(&format!("/alice/demo/commit/{head}.diff"));
    plain(&r);
    assert!(r.body.contains("-one\n+two\n"), "{}", r.body);
    let r = alice.get(&format!("/alice/demo/commit/{head}.patch"));
    plain(&r);
    assert!(r.body.contains("Subject: [PATCH] Change a"), "{}", r.body);
    let r = alice.get(&format!("/alice/demo/commit/{root}.patch"));
    plain(&r);
    assert!(r.body.contains("+one\n"), "{}", r.body);
    let r = alice.get(&format!("/alice/demo/merge-requests/{n}.diff"));
    plain(&r);
    assert!(
        r.body.contains("+two\n") && !r.body.contains("Subject:"),
        "{}",
        r.body
    );
    let r = alice.get(&format!("/alice/demo/merge-requests/{n}.patch"));
    plain(&r);
    assert!(r.body.contains("Subject: [PATCH] Change a"), "{}", r.body);
    // Through the API with a token.
    for path in [
        format!("/api/v1/repos/alice/demo/commits/{head}/diff"),
        format!("/api/v1/repos/alice/demo/commits/{head}/patch"),
        format!("/api/v1/repos/alice/demo/merge-requests/{n}/diff"),
        format!("/api/v1/repos/alice/demo/merge-requests/{n}/patch"),
    ] {
        let r = api.api("GET", &path, Some(&token), None);
        plain(&r);
        assert!(r.body.contains("+two\n"), "{path}: {}", r.body);
    }

    // The repository is private: nobody else gets anything.
    let mut mallory = s.login("mallory", PASSWORD);
    let mt = create_token(&mut mallory, "t", &["repo:read"]);
    for path in [
        format!("/alice/demo/commit/{head}.diff"),
        format!("/alice/demo/commit/{head}.patch"),
        format!("/alice/demo/merge-requests/{n}.diff"),
        format!("/alice/demo/merge-requests/{n}.patch"),
    ] {
        assert_eq!(mallory.get(&path).status, 404, "{path}");
        assert_eq!(s.client().get(&path).status, 404, "{path}");
    }
    for path in [
        format!("/api/v1/repos/alice/demo/commits/{head}/diff"),
        format!("/api/v1/repos/alice/demo/merge-requests/{n}/patch"),
    ] {
        assert_eq!(api.api("GET", &path, Some(&mt), None).status, 404, "{path}");
        assert_eq!(api.api("GET", &path, None, None).status, 404, "{path}");
    }
}
