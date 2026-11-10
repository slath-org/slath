// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merge when ready (REVIEW-09): it merges exactly once, as whoever set it,
//! when the last requirement is met; a revision pushed by anyone else
//! cancels it and the setter hears about it; their own pushes don't.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::fork_merge_requests::{api, commit, eventually, token};

fn mr(s: &TestServer, t: &str, n: i64) -> Value {
    api(
        s,
        t,
        "GET",
        &format!("/alice/demo/merge-requests/{n}"),
        None,
    )
    .json()
}

fn open(s: &TestServer, t: &str, branch: &str, target: &str) -> (i64, String) {
    let r = api(
        s,
        t,
        "POST",
        "/alice/demo/merge-requests",
        Some(
            json!({"source_branch": branch, "target_branch": target, "title": format!("Merge {branch}")}),
        ),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    eventually(s, "no first revision", || mr(s, t, n)["revisions"] == 1);
    (n, mr(s, t, n)["head_oid"].as_str().unwrap().to_owned())
}

fn auto(s: &TestServer, t: &str, n: i64, body: Option<Value>) -> slath_tests::http::Resp {
    let method = if body.is_some() { "PUT" } else { "DELETE" };
    api(
        s,
        t,
        method,
        &format!("/alice/demo/merge-requests/{n}/auto-merge"),
        body,
    )
}

fn push(git: &GitClient, w: &Path, url: &str, branch: &str, file: &str) {
    git.ok(w, &["switch", "-q", branch]);
    commit(git, w, file);
    git.ok(w, &["push", "-q", url, branch]);
}

/// Notifications of `user` about merge when ready, by reason.
fn auto_merge_notices(s: &TestServer, user: &str) -> i64 {
    s.db.query_one(&format!(
        "SELECT count(*)::text FROM notifications n JOIN users u ON u.id = n.user_id
         JOIN namespaces ns ON ns.id = u.namespace_id
         WHERE ns.name = '{user}' AND n.reason = 'auto_merge_result'"
    ))
    .parse()
    .unwrap()
}

#[test]
fn conf_review_09_merge_when_ready_merges_once_and_cancels_on_a_foreign_push() {
    let s = TestServer::start();
    for u in ["bob", "carol", "dave"] {
        s.create_user(u, PASSWORD, false);
    }
    let alice = token(&s, "alice");
    let (bob, carol, dave) = (token(&s, "bob"), token(&s, "carol"), token(&s, "dave"));
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    let url = s.url(&DEMO);
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    commit(&git, &w, "README");
    git.ok(&w, &["push", "-q", &url, "main", "main:release"]);
    for (method, path, body) in [
        ("PATCH", "/alice/demo", json!({"visibility": "public"})),
        (
            "PUT",
            "/alice/demo/collaborators/bob",
            json!({"role": "write"}),
        ),
        (
            "PUT",
            "/alice/demo/collaborators/carol",
            json!({"role": "write"}),
        ),
        (
            "POST",
            "/alice/demo/protections",
            json!({"pattern": "main", "required_approvals": 1, "required_status_contexts": ["ci"]}),
        ),
    ] {
        let r = api(&s, &alice, method, path, Some(body));
        assert!(r.status < 300, "{path}: {}", r.body);
    }
    for b in ["ready", "foreign", "own"] {
        git.ok(&w, &["switch", "-q", "-c", b, "main"]);
        commit(&git, &w, &format!("{b}.txt"));
        git.ok(&w, &["push", "-q", &url, b]);
    }

    // Set while requirements are missing: nothing happens yet.
    let (n, head) = open(&s, &alice, "ready", "main");
    let r = auto(&s, &dave, n, Some(json!({})));
    assert_eq!(r.status, 403, "readers can't set it: {}", r.body);
    let r = auto(&s, &alice, n, Some(json!({"strategy": "merge-commit"})));
    assert_eq!(r.status, 200, "{}", r.body);
    let m = r.json();
    assert_eq!(m["state"], "open");
    assert_eq!(m["auto_merge"]["strategy"], "merge-commit", "{m}");
    assert_eq!(m["auto_merge"]["set_by"], "alice");
    let r = api(
        &s,
        &bob,
        "POST",
        &format!("/alice/demo/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "approve"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let status = |state: &str| {
        let r = api(
            &s,
            &carol,
            "POST",
            &format!("/alice/demo/statuses/{head}"),
            Some(json!({"state": state, "context": "ci"})),
        );
        assert!(r.status < 300, "{}", r.body);
    };
    status("pending");
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert_eq!(mr(&s, &alice, n)["state"], "open");
    // The last requirement: it merges, once, as alice.
    status("success");
    eventually(&s, "not merged when ready", || {
        mr(&s, &alice, n)["state"] == "merged"
    });
    let main = git
        .ok(&s.repo_path(&DEMO), &["rev-parse", "main"])
        .trim()
        .to_owned();
    assert_eq!(
        git.ok(&s.repo_path(&DEMO), &["rev-parse", "main^2"]).trim(),
        head
    );
    assert!(mr(&s, &alice, n)["auto_merge"].is_null());
    status("success");
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert_eq!(
        git.ok(&s.repo_path(&DEMO), &["rev-parse", "main"]).trim(),
        main,
        "merged only once"
    );
    let merged_events: i64 =
        s.db.query_one("SELECT count(*)::text FROM timeline_events WHERE kind = 'merged'")
            .parse()
            .unwrap();
    assert_eq!(merged_events, 1);
    eventually(&s, "alice not told", || {
        auto_merge_notices(&s, "alice") == 1
    });

    // Bob sets it; alice pushes: cancelled, and bob hears about it.
    let (n, _) = open(&s, &alice, "foreign", "release");
    // An open merge request into an unprotected branch merges right away
    // when everything is met, so require an approval there first.
    let r = api(
        &s,
        &alice,
        "POST",
        "/alice/demo/protections",
        Some(json!({"pattern": "release", "required_approvals": 1})),
    );
    assert!(r.status < 300, "{}", r.body);
    let r = auto(&s, &bob, n, Some(json!({})));
    assert_eq!(r.status, 200, "{}", r.body);
    push(&git, &w, &url, "foreign", "foreign-2.txt");
    eventually(&s, "not cancelled by alice's push", || {
        mr(&s, &alice, n)["auto_merge"].is_null()
    });
    let page = s
        .login("bob", PASSWORD)
        .get(&format!("/alice/demo/merge-requests/{n}"))
        .body;
    assert!(
        page.contains("cancelled merging when ready by pushing a new revision"),
        "{page}"
    );
    eventually(&s, "bob not told", || auto_merge_notices(&s, "bob") == 1);

    // Alice's own push keeps hers; the setter or a writer can cancel it.
    let (n, _) = open(&s, &alice, "own", "release");
    assert_eq!(auto(&s, &alice, n, Some(json!({}))).status, 200);
    push(&git, &w, &url, "own", "own-2.txt");
    eventually(&s, "no second revision", || {
        mr(&s, &alice, n)["revisions"] == 2
    });
    assert_eq!(mr(&s, &alice, n)["auto_merge"]["set_by"], "alice");
    assert_eq!(auto(&s, &dave, n, None).status, 403);
    assert_eq!(auto(&s, &carol, n, None).status, 204);
    assert!(mr(&s, &alice, n)["auto_merge"].is_null());

    // Requirements met already: setting it merges right away.
    assert_eq!(auto(&s, &alice, n, Some(json!({}))).status, 200);
    let r = api(
        &s,
        &bob,
        "POST",
        &format!("/alice/demo/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "approve"})),
    );
    assert!(r.status < 300, "{}", r.body);
    eventually(&s, "not merged after the approval", || {
        mr(&s, &alice, n)["state"] == "merged"
    });
}

#[test]
fn conf_review_09_merge_when_ready_in_the_ui() {
    let s = TestServer::start();
    let alice = token(&s, "alice");
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    let url = s.url(&DEMO);
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    commit(&git, &w, "README");
    git.ok(&w, &["push", "-q", &url, "main"]);
    let r = api(
        &s,
        &alice,
        "POST",
        "/alice/demo/protections",
        Some(json!({"pattern": "main", "required_approvals": 1})),
    );
    assert!(r.status < 300, "{}", r.body);
    git.ok(&w, &["switch", "-q", "-c", "topic"]);
    commit(&git, &w, "a.txt");
    git.ok(&w, &["push", "-q", &url, "topic"]);
    let (n, _) = open(&s, &alice, "topic", "main");
    let page = format!("/alice/demo/merge-requests/{n}");
    let mut web = s.login("alice", PASSWORD);
    let body = web.get(&page).body;
    assert!(body.contains(">Merge when ready</button>"), "{body}");
    let r = slath_tests::http::submit(
        &mut web,
        &page,
        &format!("{page}/auto-merge"),
        &[("strategy", "rebase")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let body = web.get(&format!("{page}?status=auto_merge")).body;
    assert!(
        body.contains("It merges (rebase) as soon as every requirement is met."),
        "{body}"
    );
    assert!(
        body.contains("set this to merge when ready (rebase)"),
        "{body}"
    );
    let r = slath_tests::http::submit(&mut web, &page, &format!("{page}/auto-merge/cancel"), &[]);
    assert_eq!(r.status, 303, "{}", r.body);
    let body = web.get(&page).body;
    assert!(body.contains(">Merge when ready</button>"), "{body}");
    assert!(body.contains("cancelled merging when ready"), "{body}");
}
