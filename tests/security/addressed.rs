// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bots and review threads (`conf_agent_12_*`): a bot can't resolve or
//! reopen a thread a person started; it marks it addressed, which settles
//! nothing until a person resolves it.

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

#[test]
fn conf_agent_12_bots_mark_threads_addressed_and_people_resolve_them() {
    let s = TestServer::start_full(&[DEMO], "");
    s.create_user("erin", PASSWORD, false);
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            "alice",
            "--name",
            "alice-agent",
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
    let bot = serde_json::from_str::<Value>(&out).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let token = |user: &str| {
        let mut c = s.login(user, PASSWORD);
        slath_tests::http::create_token(&mut c, user, &["repo:write", "mrs:write"])
    };
    let (alice, erin) = (token("alice"), token("erin"));
    let call = |t: &str, method: &str, path: &str, body: Option<Value>| {
        s.client().api(
            method,
            &format!("/api/v1/repos/alice/demo{path}"),
            Some(t),
            body.as_ref(),
        )
    };
    let ok = |r: slath_tests::http::Resp| {
        assert!(r.status < 300, "{} {}", r.status, r.body);
        r
    };
    ok(call(
        &alice,
        "PUT",
        "/collaborators/alice-agent",
        Some(json!({"role": "write"})),
    ));
    ok(call(
        &alice,
        "PUT",
        "/collaborators/erin",
        Some(json!({"role": "read"})),
    ));
    ok(call(
        &alice,
        "POST",
        "/protections",
        Some(json!({"pattern": "main", "required_approvals": 0, "require_resolved_threads": true})),
    ));

    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "demo\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &s.url(&DEMO), "main"]);
    git.ok(w, &["switch", "-q", "-c", "fix"]);
    std::fs::write(w.join("README"), "demo, fixed\n").unwrap();
    git.ok(w, &["commit", "-qam", "fix"]);
    git.ok(
        w,
        &[
            "push",
            "-q",
            &format!("http://git:{bot}@{}/alice/demo.git", s.addr),
            "fix",
        ],
    );
    let n = ok(call(
        &bot,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "fix", "title": "Fix"})),
    ))
    .json()["number"]
        .as_i64()
        .unwrap();
    let mr = format!("/merge-requests/{n}");
    let start = std::time::Instant::now();
    while ok(call(&alice, "GET", &mr, None)).json()["revisions"] != 1 {
        assert!(start.elapsed().as_secs() < 30, "no revision");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    ok(call(&bot, "PATCH", &mr, Some(json!({"draft": false}))));

    let thread_of = |t: &str, body: &str| {
        ok(call(
            t,
            "POST",
            &format!("{mr}/threads"),
            Some(json!({"body": body})),
        ))
        .json()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let get = |id: &str| {
        ok(call(&alice, "GET", &format!("{mr}/threads"), None))
            .json()
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .cloned()
            .unwrap()
    };
    let threads_req = || {
        ok(call(&alice, "GET", &format!("{mr}/mergeability"), None)).json()["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == "threads")
            .cloned()
            .unwrap()["state"]
            .clone()
    };
    let resolve = |t: &str, id: &str, resolved: bool| {
        call(
            t,
            "PATCH",
            &format!("{mr}/threads/{id}"),
            Some(json!({"resolved": resolved})),
        )
    };
    let addressed = |t: &str, id: &str, method: &str| {
        call(t, method, &format!("{mr}/threads/{id}/addressed"), None)
    };

    // A person's thread: the bot can't resolve it, but can mark it.
    let q = thread_of(&alice, "Why this wording?");
    let r = resolve(&bot, &q, true);
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(
        r.body.contains("Mark it as addressed instead"),
        "{}",
        r.body
    );
    ok(addressed(&bot, &q, "PUT"));
    let t = get(&q);
    assert_eq!(
        (t["resolved"].clone(), t["addressed_by"].clone()),
        (json!(false), json!("alice-agent"))
    );
    assert!(t["addressed_at"].is_string(), "{t}");
    assert_eq!(threads_req(), "unmet", "addressed isn't resolved");
    let page = s
        .login("alice", PASSWORD)
        .get(&format!("/alice/demo/merge-requests/{n}"))
        .body;
    assert!(
        page.contains("marked as addressed by</span> <a class=\"account\" href=\"/alice-agent\">"),
        "{page}"
    );

    // Its own reply keeps the mark; a person's answer clears it.
    ok(call(
        &bot,
        "POST",
        &format!("{mr}/threads/{q}/comments"),
        Some(json!({"body": "Reworded."})),
    ));
    assert_eq!(get(&q)["addressed_by"], "alice-agent");
    ok(call(
        &alice,
        "POST",
        &format!("{mr}/threads/{q}/comments"),
        Some(json!({"body": "Not quite."})),
    ));
    assert_eq!(get(&q)["addressed_by"], Value::Null);

    // Marked again, a person resolves it, which clears the mark; the bot
    // can't reopen it, nor mark a resolved thread.
    ok(addressed(&bot, &q, "PUT"));
    ok(resolve(&alice, &q, true));
    let t = get(&q);
    assert_eq!(
        (t["resolved"].clone(), t["addressed_by"].clone()),
        (json!(true), Value::Null)
    );
    assert_eq!(threads_req(), "met");
    assert_eq!(resolve(&bot, &q, false).status, 409);
    assert_eq!(addressed(&bot, &q, "PUT").status, 409);

    // Clearing a mark, and who may set one: not a reader.
    let other = thread_of(&alice, "And this?");
    ok(addressed(&bot, &other, "PUT"));
    ok(addressed(&bot, &other, "DELETE"));
    assert_eq!(get(&other)["addressed_by"], Value::Null);
    let r = addressed(&erin, &other, "PUT");
    assert!(
        r.status == 403 || r.status == 404,
        "{} {}",
        r.status,
        r.body
    );
    // A pending answer clears it once its review is submitted.
    ok(addressed(&bot, &other, "PUT"));
    ok(call(
        &alice,
        "POST",
        &format!("{mr}/threads/{other}/comments"),
        Some(json!({"body": "Still unclear.", "pending": true})),
    ));
    assert_eq!(get(&other)["addressed_by"], "alice-agent");
    ok(call(
        &alice,
        "POST",
        &format!("{mr}/reviews"),
        Some(json!({"verdict": "comment"})),
    ));
    assert_eq!(get(&other)["addressed_by"], Value::Null);
    ok(resolve(&alice, &other, true));

    // A bot settles threads bots started.
    let own = thread_of(&bot, "Note to self: tidy up.");
    ok(resolve(&bot, &own, true));
    assert_eq!(get(&own)["resolved"], true);
}
