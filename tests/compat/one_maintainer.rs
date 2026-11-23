// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one-maintainer policy (REVIEW-32): a protection setting under which
//! the only person with write access can review their own merge requests,
//! in place of approvals they could never get. People turn it on; bots
//! can't.

use serde_json::{Value, json};
use slath_tests::server::{PASSWORD, TestServer};

pub(crate) fn token(s: &TestServer, user: &str) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, "t", &["repo:write", "mrs:write"])
}

pub(crate) fn api(
    s: &TestServer,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> slath_tests::http::Resp {
    s.client().api(
        method,
        &format!("/api/v1/repos/alice/demo{path}"),
        Some(token),
        body.as_ref(),
    )
}

/// A bot of alice's with the admin role on alice/demo; its token.
fn admin_bot(s: &TestServer, alice: &str) -> String {
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            "alice",
            "--name",
            "keeper",
            "--token-scopes",
            "repo:write",
            "--repo",
            "alice/demo",
            "--output",
            "json",
        ],
        None,
    );
    assert!(ok, "{out}{err}");
    let r = api(
        s,
        alice,
        "PUT",
        "/collaborators/keeper",
        Some(json!({"role": "admin"})),
    );
    assert!(r.status < 300, "{}", r.body);
    serde_json::from_str::<Value>(&out).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn conf_review_32_people_turn_the_policy_on_and_bots_cant() {
    let s = TestServer::start();
    let alice = token(&s, "alice");
    let rule = json!({"pattern": "main", "required_approvals": 1, "single_maintainer": true});
    let bot = admin_bot(&s, &alice);
    let r = api(&s, &bot, "POST", "/protections", Some(rule.clone()));
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("single_maintainer"), "{}", r.body);
    // Without the policy the bot may set the rule; a person turns it on.
    let r = api(
        &s,
        &bot,
        "POST",
        "/protections",
        Some(json!({"pattern": "main", "required_approvals": 1})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["single_maintainer"], false);
    let r = api(&s, &alice, "POST", "/protections", Some(rule));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["single_maintainer"], true);
    let list = api(&s, &alice, "GET", "/protections", None).json();
    assert_eq!(list[0]["single_maintainer"], true);
}

/// Pushes `main` and `topic` (one commit adding `file`) to alice/demo and
/// opens a merge request from `topic`; its number.
pub(crate) fn open_mr(s: &TestServer, alice: &str, w: &std::path::Path, file: &str) -> i64 {
    let git = slath_tests::server::GitClient::new();
    let url = s.url(&slath_tests::server::DEMO);
    if !w.join(".git").exists() {
        std::fs::create_dir_all(w).unwrap();
        git.ok(w, &["init", "-q", "-b", "main"]);
        std::fs::write(w.join("README"), "demo\n").unwrap();
        git.ok(w, &["add", "."]);
        git.ok(w, &["commit", "-q", "-m", "init"]);
        git.ok(w, &["push", "-q", &url, "main"]);
    }
    git.ok(w, &["switch", "-q", "-C", "topic", "main"]);
    if let Some(dir) = std::path::Path::new(file).parent() {
        std::fs::create_dir_all(w.join(dir)).unwrap();
    }
    std::fs::write(w.join(file), "change\n").unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", &format!("Change {file}")]);
    git.ok(w, &["push", "-q", "--force", &url, "topic"]);
    let r = api(
        s,
        alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": format!("Change {file}")})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    r.json()["number"].as_i64().unwrap()
}

pub(crate) fn mergeability(s: &TestServer, token: &str, n: i64) -> Value {
    let start = std::time::Instant::now();
    loop {
        let r = api(
            s,
            token,
            "GET",
            &format!("/merge-requests/{n}/mergeability"),
            None,
        );
        assert_eq!(r.status, 200, "{}", r.body);
        let m = api(s, token, "GET", &format!("/merge-requests/{n}"), None).json();
        if m["revisions"].as_i64() >= Some(1) {
            return r.json();
        }
        assert!(start.elapsed().as_secs() < 30, "no revision");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[test]
fn conf_review_32_one_maintainer_counts_people_with_write_access() {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let alice = token(&s, "alice");
    let d = tempfile::tempdir().unwrap();
    let n = open_mr(&s, &alice, &d.path().join("w"), "a.txt");
    // Without the setting there's no policy.
    assert_eq!(mergeability(&s, &alice, n)["one_maintainer"], Value::Null);
    let r = api(
        &s,
        &alice,
        "POST",
        "/protections",
        Some(json!({"pattern": "main", "required_approvals": 1, "single_maintainer": true})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let policy = |want: Value| {
        assert_eq!(mergeability(&s, &alice, n)["one_maintainer"], want);
    };
    policy(json!({"maintainer": "alice", "applies": true}));
    // Bots and readers don't count.
    admin_bot(&s, &alice);
    let role = |r: &str| {
        let r = api(
            &s,
            &alice,
            "PUT",
            "/collaborators/bob",
            Some(json!({"role": r})),
        );
        assert!(r.status < 300, "{}", r.body);
    };
    role("read");
    policy(json!({"maintainer": "alice", "applies": true}));
    // A second person with write access ends it at once.
    role("write");
    policy(json!({"maintainer": null, "applies": false}));
    role("read");
    policy(json!({"maintainer": "alice", "applies": true}));
}
