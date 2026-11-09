// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Review threads (REVIEW-02): an unchanged line carries forward to its
//! new position, a changed line makes the thread outdated (never shown on
//! a guessed line), pending comments stay private until the review is
//! submitted, and resolving feeds the resolved-threads requirement.

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

fn token(s: &TestServer, user: &str) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, user, &["repo:write", "mrs:write"])
}

fn api(
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

fn lines(edit: Option<usize>) -> String {
    (1..=20)
        .map(|i| {
            if Some(i) == edit {
                format!("line {i}, edited\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect()
}

fn wait(s: &TestServer, token: &str, n: i64, k: i64) {
    let start = std::time::Instant::now();
    while api(s, token, "GET", &format!("/merge-requests/{n}"), None).json()["revisions"] != k {
        assert!(
            start.elapsed().as_secs() < 30,
            "no revision {k}\n{}",
            s.log()
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

fn thread<'a>(list: &'a Value, id: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap_or_else(|| panic!("{id} not in {list}"))
}

#[test]
fn conf_review_02_threads_carry_forward_or_become_outdated() {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let (alice, bob) = (token(&s, "alice"), token(&s, "bob"));
    let r = api(
        &s,
        &alice,
        "PUT",
        "/collaborators/bob",
        Some(json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let r = api(
        &s,
        &alice,
        "POST",
        "/protections",
        Some(json!({"pattern": "main", "required_approvals": 0, "require_resolved_threads": true})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let url = s.url(&DEMO);
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("f.txt"), lines(None)).unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "-c", "topic"]);
    std::fs::write(w.join("f.txt"), lines(Some(15))).unwrap();
    std::fs::write(w.join("g.txt"), lines(None)).unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "Change"]);
    git.ok(w, &["push", "-q", &url, "topic"]);
    let n = api(
        &s,
        &alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": "T"})),
    )
    .json()["number"]
        .as_i64()
        .unwrap();
    wait(&s, &alice, n, 1);
    let new_thread = |token: &str, body: Value| {
        let r = api(
            &s,
            token,
            "POST",
            &format!("/merge-requests/{n}/threads"),
            Some(body),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        r.json()["id"].as_str().unwrap().to_owned()
    };
    // Lines 5 and 10 of g.txt; line 15 of f.txt's old side (the removed line).
    let stays = new_thread(
        &bob,
        json!({"body": "Line 5 is fine", "path": "g.txt", "line_start": 5}),
    );
    let changes = new_thread(
        &bob,
        json!({"body": "Rename this", "path": "g.txt", "line_start": 10, "line_end": 11}),
    );
    let old_side = new_thread(
        &bob,
        json!({"body": "Why remove it?", "path": "f.txt", "side": "old", "line_start": 15}),
    );
    let general = new_thread(&bob, json!({"body": "Overall fine"}));
    let pending = new_thread(
        &bob,
        json!({"body": "Not sent yet", "path": "g.txt", "line_start": 2, "pending": true}),
    );
    // Bad anchors are refused.
    for bad in [
        json!({"body": "x", "path": "g.txt", "line_start": 99}),
        json!({"body": "x", "path": "nope.txt", "line_start": 1}),
        json!({"body": "x", "path": "g.txt", "side": "sideways", "line_start": 1}),
        json!({"body": "", "path": "g.txt", "line_start": 1}),
    ] {
        assert_eq!(
            api(
                &s,
                &bob,
                "POST",
                &format!("/merge-requests/{n}/threads"),
                Some(bad)
            )
            .status,
            422
        );
    }

    // Pending comments are only their author's until the review is sent.
    let for_alice = api(
        &s,
        &alice,
        "GET",
        &format!("/merge-requests/{n}/threads"),
        None,
    )
    .json();
    assert!(
        for_alice
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["id"] != pending.as_str()),
        "{for_alice}"
    );
    let for_bob = api(
        &s,
        &bob,
        "GET",
        &format!("/merge-requests/{n}/threads"),
        None,
    )
    .json();
    assert_eq!(thread(&for_bob, &pending)["comments"][0]["pending"], true);

    // Revision 2: two lines inserted at the top of g.txt, lines 10 and 11 changed.
    let mut g: Vec<String> = lines(None).lines().map(str::to_owned).collect();
    g[9] = "line 10, renamed".into();
    g.insert(0, "header a".into());
    g.insert(0, "header b".into());
    std::fs::write(w.join("g.txt"), g.join("\n") + "\n").unwrap();
    git.ok(w, &["commit", "-qa", "--amend", "--no-edit"]);
    git.ok(w, &["push", "-q", "--force", &url, "topic"]);
    wait(&s, &alice, n, 2);
    let list = api(
        &s,
        &alice,
        "GET",
        &format!("/merge-requests/{n}/threads"),
        None,
    )
    .json();
    let p = &thread(&list, &stays)["position"];
    assert_eq!(
        (
            p["state"].clone(),
            p["line_start"].clone(),
            p["from_revision"].clone()
        ),
        (json!("current"), json!(7), json!(1)),
        "{p}"
    );
    let p = &thread(&list, &changes)["position"];
    assert_eq!(p["state"], "outdated", "{p}");
    assert!(
        p["line_start"].is_null(),
        "an outdated thread has no line: {p}"
    );
    assert_eq!(thread(&list, &old_side)["position"]["state"], "current");
    assert_eq!(thread(&list, &general)["position"]["state"], "general");

    // Submitting the review publishes the pending comment.
    let r = api(
        &s,
        &bob,
        "POST",
        &format!("/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "comment"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let for_alice = api(
        &s,
        &alice,
        "GET",
        &format!("/merge-requests/{n}/threads"),
        None,
    )
    .json();
    assert_eq!(
        thread(&for_alice, &pending)["comments"][0]["pending"],
        false
    );

    // Unresolved threads block merging when the rule asks for it.
    let m = api(
        &s,
        &alice,
        "GET",
        &format!("/merge-requests/{n}/mergeability"),
        None,
    )
    .json();
    let threads = m["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "threads")
        .unwrap()
        .clone();
    assert_eq!(threads["state"], "unmet", "{m}");
    assert_eq!(threads["reason"], "5 threads are unresolved.");
    // The author resolves them; a reply works too.
    let r = api(
        &s,
        &alice,
        "POST",
        &format!("/merge-requests/{n}/threads/{changes}/comments"),
        Some(json!({"body": "Done in r2"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    for id in [&stays, &changes, &old_side, &general, &pending] {
        let r = api(
            &s,
            &alice,
            "PATCH",
            &format!("/merge-requests/{n}/threads/{id}"),
            Some(json!({"resolved": true})),
        );
        assert_eq!(r.status, 204, "{}", r.body);
    }
    let m = api(
        &s,
        &alice,
        "GET",
        &format!("/merge-requests/{n}/mergeability"),
        None,
    )
    .json();
    assert_eq!(m["mergeable"], true, "{m}");
    let list = api(
        &s,
        &alice,
        "GET",
        &format!("/merge-requests/{n}/threads"),
        None,
    )
    .json();
    assert_eq!(thread(&list, &changes)["resolved_by"], "alice");
    assert_eq!(
        thread(&list, &changes)["comments"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
