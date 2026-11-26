// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Protection rules on facts: facts a protection names need an approval by
//! a person, bots' approvals never meet them, and a protection can require
//! every changed file to be viewed by a counting approver.

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::fork_merge_requests::{api, eventually};

fn token(s: &TestServer, user: &str) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, user, &["repo:write", "mrs:write"])
}

/// A bot of bob's with the write role on alice/demo, whose approvals count
/// where the protection lets bots approve; its token.
fn bob_bot(s: &TestServer, alice: &str) -> String {
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            "bob",
            "--name",
            "bob-bot",
            "--token-scopes",
            "repo:write,mrs:write",
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
        "/alice/demo/collaborators/bob-bot",
        Some(json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    serde_json::from_str::<Value>(&out).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// alice's repository with bob a writer, `protection` on main, and an MR
/// whose one change adds a lockfile and deletes a test.
fn start(protection: Value) -> (TestServer, String, String, i64) {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let (alice, bob) = (token(&s, "alice"), token(&s, "bob"));
    let r = api(
        &s,
        &alice,
        "PUT",
        "/alice/demo/collaborators/bob",
        Some(json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let r = api(
        &s,
        &alice,
        "POST",
        "/alice/demo/protections",
        Some(protection),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let url = s.url(&DEMO);
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(w.join("tests")).unwrap();
    std::fs::write(w.join("tests/old_test.rs"), "#[test]\nfn t() {}\n").unwrap();
    std::fs::write(w.join("README"), "demo\n").unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "-c", "topic"]);
    std::fs::write(
        w.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"rand\"\nversion = \"0.9.0\"\n",
    )
    .unwrap();
    git.ok(w, &["rm", "-q", "tests/old_test.rs"]);
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Add rand, drop a test"]);
    git.ok(w, &["push", "-q", &url, "topic"]);
    let r = api(
        &s,
        &alice,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Add rand"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    eventually(&s, "no first revision", || {
        api(
            &s,
            &alice,
            "GET",
            &format!("/alice/demo/merge-requests/{n}"),
            None,
        )
        .json()["revisions"]
            == 1
    });
    (s, alice, bob, n)
}

/// The state and reason of requirement `key`, if the merge box has it.
fn requirement(s: &TestServer, token: &str, n: i64, key: &str) -> Option<(String, String)> {
    let m = api(
        s,
        token,
        "GET",
        &format!("/alice/demo/merge-requests/{n}/mergeability"),
        None,
    )
    .json();
    m["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == key)
        .map(|r| {
            (
                r["state"].as_str().unwrap().to_owned(),
                r["reason"].as_str().unwrap().to_owned(),
            )
        })
}

fn approve(s: &TestServer, token: &str, n: i64) {
    let r = api(
        s,
        token,
        "POST",
        &format!("/alice/demo/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "approve"})),
    );
    assert!(r.status < 300, "{}", r.body);
}

#[test]
fn conf_agent_14_facts_a_protection_names_need_a_persons_approval() {
    let (s, alice, bob, n) = start(json!({
        "pattern": "main",
        "required_approvals": 0,
        "bots_count_as_approvers": true,
        "fact_rules": ["dependencies", "deleted_tests", "secrets"],
    }));
    // The rule reads back as it was saved.
    let rules = api(&s, &alice, "GET", "/alice/demo/protections", None).json();
    assert_eq!(
        rules[0]["fact_rules"],
        json!(["dependencies", "deleted_tests", "secrets"]),
        "{rules}"
    );
    let (state, reason) = requirement(&s, &alice, n, "facts").unwrap();
    assert_eq!(state, "unmet");
    // Only the facts the diff has are named; secrets isn't one of them.
    assert_eq!(
        reason,
        "Changes dependencies and deletes tests. Needs an approval by a person."
    );
    assert_eq!(
        api(
            &s,
            &alice,
            "GET",
            &format!("/alice/demo/merge-requests/{n}/mergeability"),
            None
        )
        .json()["mergeable"],
        false
    );
    // A bot's approval counts for approvals here, never for facts.
    let bot = bob_bot(&s, &alice);
    approve(&s, &bot, n);
    assert_eq!(requirement(&s, &alice, n, "facts").unwrap().0, "unmet");
    // The author's own approval doesn't count either; bob's does.
    approve(&s, &bob, n);
    let (state, reason) = requirement(&s, &alice, n, "facts").unwrap();
    assert_eq!(state, "met", "{reason}");
    assert_eq!(
        reason,
        "Changes dependencies and deletes tests; approved by bob."
    );
}

#[test]
fn conf_agent_14_facts_without_a_rule_or_on_unprotected_branches_need_nothing() {
    // A rule naming only facts the diff doesn't have adds no requirement.
    let (s, alice, _bob, n) = start(json!({
        "pattern": "main",
        "required_approvals": 0,
        "fact_rules": ["added_binaries", "mode_changes"],
    }));
    assert!(requirement(&s, &alice, n, "facts").is_none());
    // Unknown facts are refused with the field.
    let r = api(
        &s,
        &alice,
        "POST",
        "/alice/demo/protections",
        Some(json!({"pattern": "main", "fact_rules": ["dependencies", "colour"]})),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(r.json()["errors"][0]["field"], "fact_rules", "{}", r.body);
    // Without protection on the target, nothing is required.
    let rules = api(&s, &alice, "GET", "/alice/demo/protections", None).json();
    let id = rules[0]["id"].as_str().unwrap();
    let r = api(
        &s,
        &alice,
        "DELETE",
        &format!("/alice/demo/protections/{id}"),
        None,
    );
    assert!(r.status < 300, "{}", r.body);
    assert!(requirement(&s, &alice, n, "facts").is_none());
}

#[test]
fn conf_agent_14_every_changed_file_viewed_by_an_approver() {
    let (s, alice, bob, n) = start(json!({
        "pattern": "main",
        "required_approvals": 1,
        "require_viewed_files": true,
    }));
    let (state, reason) = requirement(&s, &alice, n, "viewed_files").unwrap();
    assert_eq!(state, "unmet");
    assert!(reason.contains("no approval counts yet"), "{reason}");
    // Approving without viewing names the files.
    approve(&s, &bob, n);
    let (state, reason) = requirement(&s, &alice, n, "viewed_files").unwrap();
    assert_eq!(state, "unmet");
    assert_eq!(
        reason,
        "2 changed files weren't viewed by an approver: Cargo.lock, tests/old_test.rs."
    );
    // Viewing them and approving again meets it.
    let mut ui = s.login("bob", PASSWORD);
    let files = format!("/alice/demo/merge-requests/{n}/files");
    for path in ["Cargo.lock", "tests/old_test.rs"] {
        let r = slath_tests::http::submit(
            &mut ui,
            &files,
            &format!("/alice/demo/merge-requests/{n}/viewed"),
            &[("path", path), ("viewed", "true"), ("back", &files)],
        );
        assert_eq!(r.status, 303, "{}", r.body);
    }
    assert_eq!(
        requirement(&s, &alice, n, "viewed_files").unwrap().0,
        "unmet"
    );
    approve(&s, &bob, n);
    let (state, reason) = requirement(&s, &alice, n, "viewed_files").unwrap();
    assert_eq!(state, "met", "{reason}");
}
