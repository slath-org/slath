// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The iteration loop's parts: a merge request's revisions, the plain-text
//! interdiff between two of them, and its threads filtered by state.

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

fn token(s: &TestServer, user: &str) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, user, &["repo:write", "mrs:write", "statuses:write"])
}

fn api(
    s: &TestServer,
    token: Option<&str>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> slath_tests::http::Resp {
    s.client().api(
        method,
        &format!("/api/v1/repos/alice/demo{path}"),
        token,
        body.as_ref(),
    )
}

fn wait(s: &TestServer, token: &str, n: i64, k: i64) {
    let start = std::time::Instant::now();
    while api(s, Some(token), "GET", &format!("/merge-requests/{n}"), None).json()["revisions"] != k
    {
        assert!(
            start.elapsed().as_secs() < 30,
            "no revision {k}\n{}",
            s.log()
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// `main` with `f.txt`, and a merge request from `topic` that changes it
/// and adds `g.txt`, at revision 1.
fn open_mr(s: &TestServer, token: &str, w: &std::path::Path) -> i64 {
    let git = GitClient::new();
    let url = s.url(&DEMO);
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "-c", "topic"]);
    std::fs::write(w.join("f.txt"), "one\n2\nthree\n").unwrap();
    git.ok(w, &["commit", "-qam", "Change two"]);
    std::fs::write(w.join("g.txt"), "new\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-qm", "Add g"]);
    git.ok(w, &["push", "-q", &url, "topic"]);
    let r = api(
        s,
        Some(token),
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": "T"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    wait(s, token, n, 1);
    n
}

#[test]
fn revisions_interdiff_and_thread_filter() {
    let s = TestServer::start();
    let alice = token(&s, "alice");
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let n = open_mr(&s, &alice, w);
    // Revision 2 rewrites the second commit.
    std::fs::write(w.join("g.txt"), "newer\n").unwrap();
    git.ok(w, &["commit", "-qa", "--amend", "--no-edit"]);
    git.ok(w, &["push", "-q", "--force", &s.url(&DEMO), "topic"]);
    wait(&s, &alice, n, 2);

    let r = api(
        &s,
        Some(&alice),
        "GET",
        &format!("/merge-requests/{n}/revisions"),
        None,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    slath_tests::contract::check(
        "GET",
        "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/revisions",
        &r,
    );
    let revs = r.json();
    let head = git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned();
    let base = git.ok(w, &["rev-parse", "main"]).trim().to_owned();
    assert_eq!(revs.as_array().unwrap().len(), 2, "{revs}");
    assert_eq!(revs[0]["seq"], 1);
    assert_eq!(revs[1]["seq"], 2);
    assert_eq!(revs[1]["head_oid"], head.as_str());
    assert_eq!(revs[1]["merge_base_oid"], base.as_str());
    assert_eq!(revs[1]["commit_count"], 2);
    assert_eq!(revs[1]["mode"], "per-change");
    assert_eq!(revs[1]["pushed_by"], "alice");

    // The interdiff pairs the rewritten commit with its earlier version.
    let r = api(
        &s,
        Some(&alice),
        "GET",
        &format!("/merge-requests/{n}/interdiff?from=1"),
        None,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(
        r.header("content-type").unwrap().starts_with("text/plain"),
        "{:?}",
        r.header("content-type")
    );
    assert!(r.body.contains("Add g"), "{}", r.body);
    assert!(r.body.contains("+newer"), "{}", r.body);
    let explicit = api(
        &s,
        Some(&alice),
        "GET",
        &format!("/merge-requests/{n}/interdiff?from=1&to=2"),
        None,
    );
    assert_eq!(explicit.body, r.body);
    for bad in ["from=9", "from=1&to=9", "from=0"] {
        let r = api(
            &s,
            Some(&alice),
            "GET",
            &format!("/merge-requests/{n}/interdiff?{bad}"),
            None,
        );
        assert_eq!(r.status, 404, "{bad}: {}", r.body);
    }

    // Threads filtered by state.
    let mut ids = Vec::new();
    for body in ["first", "second"] {
        let r = api(
            &s,
            Some(&alice),
            "POST",
            &format!("/merge-requests/{n}/threads"),
            Some(json!({"body": body})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        ids.push(r.json()["id"].as_str().unwrap().to_owned());
    }
    let r = api(
        &s,
        Some(&alice),
        "PATCH",
        &format!("/merge-requests/{n}/threads/{}", ids[0]),
        Some(json!({"resolved": true})),
    );
    assert_eq!(r.status, 204, "{}", r.body);
    let only = |q: &str| -> Vec<String> {
        let r = api(
            &s,
            Some(&alice),
            "GET",
            &format!("/merge-requests/{n}/threads{q}"),
            None,
        );
        assert_eq!(r.status, 200, "{}", r.body);
        r.json()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(only("?resolved=true"), vec![ids[0].clone()]);
    assert_eq!(only("?resolved=false"), vec![ids[1].clone()]);
    assert_eq!(only(""), ids);

    // Who can't read the merge request learns nothing.
    let r = api(
        &s,
        Some(&alice),
        "PATCH",
        "",
        Some(json!({"visibility": "private"})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    for path in ["revisions", "interdiff?from=1"] {
        let r = api(
            &s,
            None,
            "GET",
            &format!("/merge-requests/{n}/{path}"),
            None,
        );
        assert_eq!(r.status, 404, "{path}: {}", r.body);
    }
}
