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

fn policy_rule(s: &TestServer, token: &str, on: bool) {
    let r = api(
        s,
        token,
        "POST",
        "/protections",
        Some(json!({"pattern": "main", "required_approvals": 1, "single_maintainer": on})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
}

fn review(s: &TestServer, token: &str, n: i64, body: Value) -> slath_tests::http::Resp {
    api(
        s,
        token,
        "POST",
        &format!("/merge-requests/{n}/reviews"),
        Some(body),
    )
}

/// The requirement `key` of the merge request's mergeability.
fn requirement(s: &TestServer, token: &str, n: i64, key: &str) -> Value {
    mergeability(s, token, n)["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == key)
        .cloned()
        .unwrap_or(Value::Null)
}

fn head(s: &TestServer, token: &str, n: i64) -> String {
    api(s, token, "GET", &format!("/merge-requests/{n}"), None).json()["head_oid"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Waits until the merge request has `k` revisions.
fn revisions(s: &TestServer, token: &str, n: i64, k: i64) {
    let start = std::time::Instant::now();
    while api(s, token, "GET", &format!("/merge-requests/{n}"), None).json()["revisions"] != k {
        assert!(start.elapsed().as_secs() < 30, "no revision {k}");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[test]
fn conf_review_32_the_maintainers_review_of_the_exact_head_counts() {
    let s = TestServer::start();
    let alice = token(&s, "alice");
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    let n = open_mr(&s, &alice, &w, "a.txt");
    policy_rule(&s, &alice, true);
    let r = requirement(&s, &alice, n, "approvals");
    assert_eq!(r["state"], "unmet", "{r}");
    assert!(
        r["reason"]
            .as_str()
            .unwrap()
            .contains("Under the one-maintainer policy, alice's review of this revision counts."),
        "{r}"
    );
    // The author's approval still never counts; their review under the
    // policy does, and is no approval.
    let r = review(&s, &alice, n, json!({"verdict": "approve"}));
    assert_eq!(r.status, 422, "{}", r.body);
    let r = review(&s, &alice, n, json!({"verdict": "accountable_review"}));
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["verdict"], "accountable_review");
    assert_eq!(r.json()["counts"], false);
    let r = requirement(&s, &alice, n, "approvals");
    assert_eq!(r["state"], "met", "{r}");
    assert!(
        r["reason"]
            .as_str()
            .unwrap()
            .starts_with("Reviewed by alice under the one-maintainer policy (r1, "),
        "{r}"
    );
    assert_eq!(mergeability(&s, &alice, n)["mergeable"], true);
    // A new revision needs a new review, even with the same patches.
    let git = slath_tests::server::GitClient::new();
    git.ok(&w, &["commit", "-q", "--amend", "--no-edit", "--date=now"]);
    git.ok(
        &w,
        &[
            "push",
            "-q",
            "--force",
            &s.url(&slath_tests::server::DEMO),
            "topic",
        ],
    );
    revisions(&s, &alice, n, 2);
    assert_eq!(requirement(&s, &alice, n, "approvals")["state"], "unmet");
    let r = review(&s, &alice, n, json!({"verdict": "accountable_review"}));
    assert_eq!(r.status, 201, "{}", r.body);
    // The merge records the policy.
    let h = head(&s, &alice, n);
    let r = api(
        &s,
        &alice,
        "POST",
        &format!("/merge-requests/{n}/merge"),
        Some(json!({"expected_head_oid": h})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let ledger = api(
        &s,
        &alice,
        "GET",
        &format!("/merge-requests/{n}/ledger"),
        None,
    )
    .json();
    let entry = ledger
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "one_maintainer_policy")
        .cloned()
        .unwrap_or_else(|| panic!("no policy entry: {ledger}"));
    assert_eq!(entry["detail"]["met"], json!(["approvals"]), "{entry}");
}

#[test]
fn conf_review_32_only_the_maintainer_records_it_while_the_policy_applies() {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let (alice, bob) = (token(&s, "alice"), token(&s, "bob"));
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
    let d = tempfile::tempdir().unwrap();
    let n = open_mr(&s, &alice, &d.path().join("w"), "a.txt");
    // Without the setting, nobody can record it, and nothing changes.
    let r = review(&s, &alice, n, json!({"verdict": "accountable_review"}));
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("one maintainer"), "{}", r.body);
    policy_rule(&s, &alice, true);
    let r = review(&s, &bob, n, json!({"verdict": "accountable_review"}));
    assert!(matches!(r.status, 403 | 422), "{}", r.body);
    // It covers the whole merge request, never one change.
    let r = review(
        &s,
        &alice,
        n,
        json!({"verdict": "accountable_review", "change": "kkkkkkkk"}),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("change"), "{}", r.body);
    let r = review(&s, &alice, n, json!({"verdict": "accountable_review"}));
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(mergeability(&s, &alice, n)["mergeable"], true);
    // A second person with write access restores the normal rules at once.
    role("write");
    let r = requirement(&s, &alice, n, "approvals");
    assert_eq!(r["state"], "unmet", "{r}");
    assert!(
        !r["reason"].as_str().unwrap().contains("one-maintainer"),
        "{r}"
    );
    let r = review(&s, &alice, n, json!({"verdict": "accountable_review"}));
    assert_eq!(r.status, 422, "{}", r.body);
    let h = head(&s, &alice, n);
    let r = api(
        &s,
        &alice,
        "POST",
        &format!("/merge-requests/{n}/merge"),
        Some(json!({"expected_head_oid": h})),
    );
    assert_eq!(r.status, 409, "{}", r.body);
    role("read");
    assert_eq!(mergeability(&s, &alice, n)["mergeable"], true);
    // So does turning the setting off.
    policy_rule(&s, &alice, false);
    assert_eq!(mergeability(&s, &alice, n)["mergeable"], false);
    assert_eq!(mergeability(&s, &alice, n)["one_maintainer"], Value::Null);
}

#[test]
fn conf_review_32_sensitive_paths_need_every_file_viewed() {
    let s = TestServer::start();
    let alice = token(&s, "alice");
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    let n = open_mr(&s, &alice, &w, ".slath/workflows/ci.yml");
    policy_rule(&s, &alice, true);
    let r = requirement(&s, &alice, n, "sensitive_paths");
    assert_eq!(r["state"], "unmet", "{r}");
    assert!(
        r["reason"]
            .as_str()
            .unwrap()
            .contains("with every file viewed counts"),
        "{r}"
    );
    let r = review(&s, &alice, n, json!({"verdict": "accountable_review"}));
    assert_eq!(r.status, 201, "{}", r.body);
    // Approvals are met; sensitive paths need every file viewed.
    assert_eq!(requirement(&s, &alice, n, "approvals")["state"], "met");
    let r = requirement(&s, &alice, n, "sensitive_paths");
    assert_eq!(r["state"], "unmet", "{r}");
    assert!(
        r["reason"]
            .as_str()
            .unwrap()
            .contains("needs every file viewed: 0 of 1 are"),
        "{r}"
    );
    let mut ui = s.login("alice", PASSWORD);
    let files = format!("/alice/demo/merge-requests/{n}/files");
    let r = slath_tests::http::submit(
        &mut ui,
        &files,
        &format!("/alice/demo/merge-requests/{n}/viewed"),
        &[
            ("path", ".slath/workflows/ci.yml"),
            ("viewed", "true"),
            ("back", &files),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    // The earlier review's snapshot doesn't change; a new one counts.
    assert_eq!(
        requirement(&s, &alice, n, "sensitive_paths")["state"],
        "unmet"
    );
    let r = review(&s, &alice, n, json!({"verdict": "accountable_review"}));
    assert_eq!(r.status, 201, "{}", r.body);
    let r = requirement(&s, &alice, n, "sensitive_paths");
    assert_eq!(r["state"], "met", "{r}");
    let h = head(&s, &alice, n);
    let r = api(
        &s,
        &alice,
        "POST",
        &format!("/merge-requests/{n}/merge"),
        Some(json!({"expected_head_oid": h})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let ledger = api(
        &s,
        &alice,
        "GET",
        &format!("/merge-requests/{n}/ledger"),
        None,
    )
    .json();
    let met: Vec<Value> = ledger
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "one_maintainer_policy")
        .map(|e| e["detail"]["met"].clone())
        .collect();
    assert_eq!(met, [json!(["approvals", "sensitive_paths"])], "{ledger}");
}

#[test]
fn conf_review_32_required_checks_still_count_as_before() {
    let s = TestServer::start();
    let alice = token(&s, "alice");
    let d = tempfile::tempdir().unwrap();
    let n = open_mr(&s, &alice, &d.path().join("w"), "a.txt");
    let r = api(
        &s,
        &alice,
        "POST",
        "/protections",
        Some(
            json!({"pattern": "main", "required_approvals": 1, "single_maintainer": true,
                    "required_status_contexts": ["ci"]}),
        ),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let r = review(&s, &alice, n, json!({"verdict": "accountable_review"}));
    assert_eq!(r.status, 201, "{}", r.body);
    let mb = mergeability(&s, &alice, n);
    assert_eq!(mb["mergeable"], false, "{mb}");
    let unmet: Vec<&Value> = mb["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["state"] != "met")
        .map(|r| &r["key"])
        .collect();
    assert_eq!(unmet, [&json!("statuses")], "{mb}");
    // The author's own status doesn't count, under the policy too.
    let h = head(&s, &alice, n);
    let mut c = s.login("alice", PASSWORD);
    let reporter = slath_tests::http::create_token(&mut c, "ci", &["statuses:write"]);
    let r = api(
        &s,
        &reporter,
        "POST",
        &format!("/statuses/{h}"),
        Some(json!({"context": "ci", "state": "success"})),
    );
    assert!(r.status < 300, "{}", r.body);
    assert_eq!(mergeability(&s, &alice, n)["mergeable"], false);
}

#[test]
fn conf_review_32_jj_stacks_land_under_the_policy_change_by_change() {
    use crate::jj::{Jj, jj_binaries};
    let s = TestServer::start();
    let alice = token(&s, "alice");
    let d = tempfile::tempdir().unwrap();
    // main, from Git.
    let w = d.path().join("w");
    open_mr(&s, &alice, &w, "a.txt");
    policy_rule(&s, &alice, true);
    let url = s.url(&slath_tests::server::DEMO);
    let (version, bin) = jj_binaries().into_iter().next().unwrap();
    let jj = Jj::new(&version, &bin);
    let j = d.path().join("jj");
    jj.ok(d.path(), &["git", "clone", &url, j.to_str().unwrap()]);
    let mut made = Vec::new();
    for (i, x) in ["b", "c"].iter().enumerate() {
        let parent = if i == 0 { "main@origin" } else { "@" };
        jj.ok(&j, &["new", parent, "-m", &format!("Add {x}")]);
        std::fs::write(j.join(format!("{x}.txt")), format!("{x}\n")).unwrap();
        made.push(jj.show(&j, "@", "change_id"));
    }
    jj.ok(&j, &["bookmark", "create", "jj-topic", "-r", "@"]);
    jj.ok(&j, &["git", "push", "--bookmark", "jj-topic"]);
    let r = api(
        &s,
        &alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "jj-topic", "title": "jj"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    revisions(&s, &alice, n, 1);
    assert_eq!(requirement(&s, &alice, n, "approvals")["state"], "unmet");
    let r = review(&s, &alice, n, json!({"verdict": "accountable_review"}));
    assert_eq!(r.status, 201, "{}", r.body);
    // The review of the head covers the bottom change as well.
    let h = head(&s, &alice, n);
    let r = api(
        &s,
        &alice,
        "POST",
        &format!("/merge-requests/{n}/merge"),
        Some(json!({"strategy": "rebase", "expected_head_oid": h, "upto_change": made[0]})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let ledger = api(
        &s,
        &alice,
        "GET",
        &format!("/merge-requests/{n}/ledger"),
        None,
    )
    .json();
    assert!(
        ledger
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "one_maintainer_policy"),
        "{ledger}"
    );
    // The rest is a new revision: it needs a new review, then merges.
    revisions(&s, &alice, n, 2);
    assert_eq!(requirement(&s, &alice, n, "approvals")["state"], "unmet");
    let r = review(&s, &alice, n, json!({"verdict": "accountable_review"}));
    assert_eq!(r.status, 201, "{}", r.body);
    let h = head(&s, &alice, n);
    let r = api(
        &s,
        &alice,
        "POST",
        &format!("/merge-requests/{n}/merge"),
        Some(json!({"expected_head_oid": h})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
}
