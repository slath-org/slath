// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Findings (AGENT-02): line annotations on commit statuses are checked
//! against the commit and stored with their status.

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

fn token(s: &TestServer, user: &str) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, user, &["repo:write", "mrs:write", "statuses:write"])
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

/// Pushes `main` with a 10-line `src/lib.rs` and returns its commit.
fn push_main(s: &TestServer, w: &std::path::Path) -> String {
    let git = GitClient::new();
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(w.join("src")).unwrap();
    let text: String = (1..=10).map(|i| format!("line {i}\n")).collect();
    std::fs::write(w.join("src/lib.rs"), text).unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &s.url(&DEMO), "main"]);
    git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn note(path: &str, line: i64) -> Value {
    json!({"path": path, "line_start": line, "severity": "warning", "message": "Unused variable"})
}

#[test]
fn conf_agent_02_annotations_are_checked_and_stored() {
    let s = TestServer::start_full(&[DEMO], "[limits]\nstatus_annotations = 3\n");
    let alice = token(&s, "alice");
    let d = tempfile::tempdir().unwrap();
    let head = push_main(&s, d.path());
    let post = |annotations: Value| {
        api(
            &s,
            &alice,
            "POST",
            &format!("/statuses/{head}"),
            Some(json!({"state": "failure", "context": "lint", "annotations": annotations})),
        )
    };
    let field = |r: &slath_tests::http::Resp| r.json()["errors"][0]["field"].clone();

    // Every annotation is checked; one bad one rejects the whole status.
    for (bad, at) in [
        (
            json!([note("src/lib.rs", 1), note("src/nope.rs", 1)]),
            "annotations[1].path",
        ),
        (json!([note("../etc/passwd", 1)]), "annotations[0].path"),
        (json!([note("src/lib.rs", 11)]), "annotations[0].line_end"),
        (json!([note("src/lib.rs", 0)]), "annotations[0].line_start"),
        (
            json!([{"path": "src/lib.rs", "line_start": 1, "severity": "fatal", "message": "x"}]),
            "annotations[0].severity",
        ),
        (
            json!([{"path": "src/lib.rs", "line_start": 1, "severity": "notice", "category": "style", "message": "x"}]),
            "annotations[0].category",
        ),
        (
            json!([{"path": "src/lib.rs", "line_start": 1, "severity": "notice", "message": "x".repeat(4097)}]),
            "annotations[0].message",
        ),
        (
            json!([
                note("src/lib.rs", 1),
                note("src/lib.rs", 2),
                note("src/lib.rs", 3),
                note("src/lib.rs", 4)
            ]),
            "annotations",
        ),
    ] {
        let r = post(bad);
        assert_eq!(r.status, 422, "{}", r.body);
        assert_eq!(field(&r), at, "{}", r.body);
    }
    let list = api(
        &s,
        &alice,
        "GET",
        &format!("/commits/{head}/statuses"),
        None,
    )
    .json();
    assert_eq!(list, json!([]), "a rejected status leaves nothing");

    // A valid status keeps its annotations in order, with the defaults.
    let r = post(json!([
        {"path": "src/lib.rs", "line_start": 9, "line_end": 10, "severity": "failure",
         "category": "security", "message": "Secret in code\n\tline two"},
        note("src/lib.rs", 2),
    ]));
    assert_eq!(r.status, 201, "{}", r.body);
    slath_tests::contract::check("POST", "/api/v1/repos/{ns}/{repo}/statuses/{oid}", &r);
    let list = api(
        &s,
        &alice,
        "GET",
        &format!("/commits/{head}/statuses"),
        None,
    );
    slath_tests::contract::check(
        "GET",
        "/api/v1/repos/{ns}/{repo}/commits/{oid}/statuses",
        &list,
    );
    assert_eq!(
        list.json()[0]["annotations"],
        json!([
            {"path": "src/lib.rs", "line_start": 9, "line_end": 10, "severity": "failure",
             "category": "security", "message": "Secret in code\n\tline two"},
            {"path": "src/lib.rs", "line_start": 2, "line_end": 2, "severity": "warning",
             "category": "other", "message": "Unused variable"},
        ])
    );
}
