// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The iteration loop (AGENT-03): `…/iteration` returns exactly what the
//! individual endpoints return for the same revision (revisions, interdiff,
//! statuses, threads, mergeability), with the same authorization and
//! within its limits; and those individual parts on their own.

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::jj::{Jj, jj_binaries};

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

fn write(dir: &std::path::Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
}

/// Opens a merge request from `branch` and waits for its first revision.
fn create(s: &TestServer, token: &str, branch: &str) -> i64 {
    let r = api(
        s,
        Some(token),
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": branch, "title": branch})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    wait(s, token, n, 1);
    n
}

/// Everything the iteration endpoint is made of, fetched one by one.
fn parts(s: &TestServer, token: Option<&str>, n: i64, since: Option<i64>) -> Value {
    let get = |path: &str| {
        let r = api(s, token, "GET", &format!("/merge-requests/{n}{path}"), None);
        assert_eq!(r.status, 200, "{path}: {}", r.body);
        r
    };
    let revision = get("/revisions").json().as_array().unwrap().last().cloned();
    let head = revision
        .as_ref()
        .map(|r| r["head_oid"].as_str().unwrap().to_owned());
    let current = revision.as_ref().map(|r| r["seq"].as_i64().unwrap());
    let interdiff = match (since, current) {
        (Some(k), Some(c)) if k < c => {
            json!({"from": k, "to": c, "text": get(&format!("/interdiff?from={k}")).body, "too_large": false})
        }
        _ => Value::Null,
    };
    let failing: Vec<Value> = match head {
        Some(h) => get_statuses(s, token, &h)
            .into_iter()
            .filter(|st| {
                ["ci", "lint"].contains(&st["context"].as_str().unwrap())
                    && ["failure", "error"].contains(&st["state"].as_str().unwrap())
            })
            .collect(),
        None => Vec::new(),
    };
    json!({
        "revision": revision,
        "interdiff": interdiff,
        "failing_checks": failing,
        "threads": get("/threads?resolved=false").json(),
        "mergeability": get("/mergeability").json(),
        // These merge requests belong to no work order.
        "work_order": null,
    })
}

fn get_statuses(s: &TestServer, token: Option<&str>, oid: &str) -> Vec<Value> {
    let r = api(s, token, "GET", &format!("/commits/{oid}/statuses"), None);
    assert_eq!(r.status, 200, "{}", r.body);
    r.json().as_array().unwrap().clone()
}

fn iteration(s: &TestServer, token: Option<&str>, n: i64, since: Option<i64>) -> Value {
    let q = since.map(|k| format!("?since={k}")).unwrap_or_default();
    let r = api(
        s,
        token,
        "GET",
        &format!("/merge-requests/{n}/iteration{q}"),
        None,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    slath_tests::contract::check(
        "GET",
        "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/iteration",
        &r,
    );
    r.json()
}

/// Statuses on the head (a failing required one with an annotation, a
/// passing required one, a failing optional one) and threads in every
/// state: unresolved, resolved, pending for bob, and bob's finding.
fn review(s: &TestServer, alice: &str, bob: &str, n: i64) {
    let mr = api(s, Some(alice), "GET", &format!("/merge-requests/{n}"), None).json();
    let head = mr["head_oid"].as_str().unwrap();
    for (ctx, state) in [("ci", "failure"), ("lint", "success"), ("extra", "error")] {
        let annotations = if ctx == "ci" {
            json!([{"path": "f.txt", "line_start": 2, "severity": "failure", "message": "Test fails here"}])
        } else {
            json!([])
        };
        let r = api(
            s,
            Some(bob),
            "POST",
            &format!("/statuses/{head}"),
            Some(
                json!({"state": state, "context": ctx, "description": format!("{ctx} {state}"),
                        "annotations": annotations}),
            ),
        );
        assert_eq!(r.status, 201, "{}", r.body);
    }
    let thread = |token: &str, body: Value| {
        let r = api(
            s,
            Some(token),
            "POST",
            &format!("/merge-requests/{n}/threads"),
            Some(body),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        r.json()["id"].as_str().unwrap().to_owned()
    };
    thread(alice, json!({"body": "Overall?"}));
    thread(
        bob,
        json!({"body": "On two", "path": "f.txt", "line_start": 2}),
    );
    thread(bob, json!({"body": "Draft note", "pending": true}));
    let done = thread(bob, json!({"body": "Settled"}));
    let r = api(
        s,
        Some(bob),
        "PATCH",
        &format!("/merge-requests/{n}/threads/{done}"),
        Some(json!({"resolved": true})),
    );
    assert_eq!(r.status, 204, "{}", r.body);
    // The finding thread is made in the background.
    let start = std::time::Instant::now();
    while !api(
        s,
        Some(alice),
        "GET",
        &format!("/merge-requests/{n}/threads"),
        None,
    )
    .json()
    .as_array()
    .unwrap()
    .iter()
    .any(|t| !t["finding"].is_null())
    {
        assert!(
            start.elapsed().as_secs() < 30,
            "no finding thread\n{}",
            s.log()
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[test]
fn conf_agent_03_iteration_equals_the_individual_endpoints() {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let (alice, bob) = (token(&s, "alice"), token(&s, "bob"));
    let r = api(
        &s,
        Some(&alice),
        "PATCH",
        "",
        Some(json!({"visibility": "public"})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let r = api(
        &s,
        Some(&alice),
        "PUT",
        "/collaborators/bob",
        Some(json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let r = api(
        &s,
        Some(&alice),
        "POST",
        "/protections",
        Some(json!({"pattern": "main", "required_approvals": 1,
                    "required_status_contexts": ["ci", "lint"]})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("git");
    std::fs::create_dir_all(&w).unwrap();
    let url = s.url(&DEMO);
    git.ok(&w, &["init", "-q", "-b", "main"]);
    write(&w, "f.txt", "one\ntwo\nthree\n");
    git.ok(&w, &["add", "."]);
    git.ok(&w, &["commit", "-q", "-m", "init"]);
    git.ok(&w, &["push", "-q", &url, "main"]);

    // Git: two commits, then the second amended.
    git.ok(&w, &["switch", "-q", "-c", "git-topic"]);
    write(&w, "f.txt", "one\n2\nthree\n");
    git.ok(&w, &["commit", "-qam", "Change two"]);
    write(&w, "g.txt", "gee\n");
    git.ok(&w, &["add", "."]);
    git.ok(&w, &["commit", "-qm", "Add g"]);
    git.ok(&w, &["push", "-q", &url, "git-topic"]);
    let mut mrs = vec![("git", create(&s, &alice, "git-topic"))];
    write(&w, "g.txt", "gee whiz\n");
    git.ok(&w, &["commit", "-qa", "--amend", "--no-edit"]);
    git.ok(&w, &["push", "-q", "--force", &url, "git-topic"]);
    wait(&s, &alice, mrs[0].1, 2);

    // Mixed: the first revision from Git, the second from jj; and jj alone.
    // Its own message: the same parent, tree, and message as git-topic's
    // first commit within the same second would be that very commit, which
    // jj keeps immutable as part of the untracked git-topic@origin.
    git.ok(&w, &["switch", "-q", "-c", "mixed-topic", "main"]);
    write(&w, "f.txt", "one\n2\nthree\n");
    git.ok(&w, &["commit", "-qam", "Change two, mixed"]);
    git.ok(&w, &["push", "-q", &url, "mixed-topic"]);
    if let Some((version, bin)) = jj_binaries().into_iter().last() {
        let jj = Jj::new(&version, &bin);
        let j = d.path().join("jj");
        jj.ok(d.path(), &["git", "clone", &url, j.to_str().unwrap()]);
        let n = create(&s, &alice, "mixed-topic");
        jj.ok(&j, &["bookmark", "track", "mixed-topic@origin"]);
        jj.ok(&j, &["edit", "mixed-topic"]);
        write(&j, "f.txt", "one\ntwo, from jj\nthree\n");
        jj.ok(&j, &["git", "push", "--bookmark", "mixed-topic"]);
        wait(&s, &alice, n, 2);
        mrs.push(("mixed", n));

        jj.ok(&j, &["new", "main@origin", "-m", "Change two"]);
        write(&j, "f.txt", "one\n2\nthree\n");
        jj.ok(&j, &["new", "-m", "Add g"]);
        write(&j, "g.txt", "gee\n");
        jj.ok(&j, &["bookmark", "create", "jj-topic", "-r", "@"]);
        jj.ok(&j, &["git", "push", "--bookmark", "jj-topic"]);
        let n = create(&s, &alice, "jj-topic");
        write(&j, "g.txt", "gee whiz\n");
        jj.ok(&j, &["git", "push", "--bookmark", "jj-topic"]);
        wait(&s, &alice, n, 2);
        mrs.push(("jj", n));
    }

    for &(made, n) in &mrs {
        review(&s, &alice, &bob, n);
        for (who, t) in [
            ("alice", Some(alice.as_str())),
            ("bob", Some(bob.as_str())),
            ("anonymous", None),
        ] {
            for since in [None, Some(1), Some(2)] {
                let got = iteration(&s, t, n, since);
                let want = parts(&s, t, n, since);
                assert_eq!(got, want, "{made} MR as {who}, since {since:?}");
            }
        }
        // What the fixture put in, so the comparison isn't vacuous.
        let it = iteration(&s, Some(&bob), n, Some(1));
        assert_eq!(it["revision"]["seq"], 2, "{made}");
        assert!(
            it["interdiff"]["text"].as_str().unwrap().contains("@@"),
            "{made}: {it}"
        );
        let failing: Vec<&str> = it["failing_checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["context"].as_str().unwrap())
            .collect();
        assert_eq!(failing, ["ci"], "{made}");
        assert_eq!(
            it["failing_checks"][0]["annotations"][0]["message"],
            "Test fails here"
        );
        assert_eq!(
            it["threads"].as_array().unwrap().len(),
            4,
            "{made}: {}",
            it["threads"]
        );
        let for_alice = iteration(&s, Some(&alice), n, None);
        assert_eq!(for_alice["threads"].as_array().unwrap().len(), 3, "{made}");
        assert_eq!(for_alice["mergeability"]["mergeable"], false);
        for bad in ["0", "3", "-1"] {
            let r = api(
                &s,
                Some(&alice),
                "GET",
                &format!("/merge-requests/{n}/iteration?since={bad}"),
                None,
            );
            assert_eq!(r.status, 422, "{bad}: {}", r.body);
            assert_eq!(r.json()["errors"][0]["field"], "since", "{}", r.body);
        }
    }
}

#[test]
fn conf_agent_03_iteration_authorization_and_limits() {
    let s = TestServer::start_full(&[DEMO], "[limits]\niteration_interdiff_max_bytes = 64\n");
    s.create_user("carol", PASSWORD, false);
    let (alice, carol) = (token(&s, "alice"), token(&s, "carol"));
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let n = open_mr(&s, &alice, w);
    write(
        w,
        "g.txt",
        "a much longer line than before, so the interdiff passes the limit\n",
    );
    git.ok(w, &["commit", "-qa", "--amend", "--no-edit"]);
    git.ok(w, &["push", "-q", "--force", &s.url(&DEMO), "topic"]);
    wait(&s, &alice, n, 2);

    // Past the limit the text is left out; the plain endpoint still has it.
    let it = iteration(&s, Some(&alice), n, Some(1));
    assert_eq!(
        it["interdiff"],
        json!({"from": 1, "to": 2, "text": null, "too_large": true})
    );
    let plain = api(
        &s,
        Some(&alice),
        "GET",
        &format!("/merge-requests/{n}/interdiff?from=1"),
        None,
    );
    assert_eq!(plain.status, 200);
    assert!(plain.body.len() > 64, "{}", plain.body);

    // Whoever can't read the merge request gets what every part gives them.
    let r = api(
        &s,
        Some(&alice),
        "PATCH",
        "",
        Some(json!({"visibility": "private"})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    for t in [None, Some(carol.as_str())] {
        for path in [
            "iteration",
            "iteration?since=1",
            "revisions",
            "threads",
            "mergeability",
            "interdiff?from=1",
        ] {
            let r = api(&s, t, "GET", &format!("/merge-requests/{n}/{path}"), None);
            assert_eq!(r.status, 404, "{path}: {}", r.body);
        }
        // A bad `since` doesn't reveal the merge request either.
        let r = api(
            &s,
            t,
            "GET",
            &format!("/merge-requests/{n}/iteration?since=9"),
            None,
        );
        assert_eq!(r.status, 404, "{}", r.body);
    }
    assert_eq!(iteration(&s, Some(&alice), n, None)["revision"]["seq"], 2);
}
