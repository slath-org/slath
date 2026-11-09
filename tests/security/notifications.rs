// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Notifications over HTTP and email (ISSUE-02): the inbox, the API with
//! its scopes, threaded emails with an unsubscribe link that works without
//! a session, no email to someone who lost access, and merge requests
//! (no review requests from drafts).

use std::time::Duration;

use serde_json::{Value, json};
use slath_tests::http::{create_repo, create_token, submit};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};
use slath_tests::smtp::SmtpServer;

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn conf_issue_02_inbox_email_and_unsubscribe() {
    let smtp = SmtpServer::start();
    let s = TestServer::start_full(&[], &smtp.config());
    for u in ["alice", "bob", "carol"] {
        s.create_user(u, PASSWORD, false);
    }
    s.db.execute("UPDATE users SET email_verified = true");
    let mut a = s.login("alice", PASSWORD);
    create_repo(&mut a, "alice", "secret", "private");
    for who in ["bob", "carol"] {
        let page = "/alice/secret/settings/access";
        assert_eq!(
            submit(&mut a, page, page, &[("username", who), ("role", "read")]).status,
            303
        );
    }
    let mut bob = s.login("bob", PASSWORD);
    let r = submit(
        &mut bob,
        "/alice/secret/issues/new",
        "/alice/secret/issues",
        &[("title", "Leaky title"), ("body", "Hi @carol")],
    );
    assert_eq!(r.status, 303, "{}", r.body);

    // alice (watching her repository) and carol (mentioned) are notified.
    wait_until("alice's inbox", || {
        a.get("/-/notifications").body.contains("Leaky title")
    });
    wait_until("carol's email", || {
        smtp.messages()
            .iter()
            .any(|m| m.to.iter().any(|t| t.contains("carol@")) && m.data.contains("Hi @carol"))
    });
    let msgs = smtp.messages();
    let to_carol = msgs
        .iter()
        .find(|m| m.to.iter().any(|t| t.contains("carol@")) && m.data.contains("Hi @carol"))
        .expect("carol was emailed");
    assert_eq!(
        to_carol.header("Subject").as_deref(),
        Some("[alice/secret] Leaky title (#1)")
    );
    let unsub = to_carol.header("List-Unsubscribe").unwrap();
    assert!(unsub.contains("/-/unsubscribe/"), "{unsub}");
    assert!(to_carol.header("In-Reply-To").is_some() || to_carol.header("Message-ID").is_some());
    assert!(
        !msgs
            .iter()
            .any(|m| m.to.iter().any(|t| t.contains("bob@")) && m.data.contains("Hi @carol")),
        "the actor isn't notified"
    );

    // The API lists it with user:read, and needs user:write to change it.
    let read = create_token(&mut a, "r", &["user:read", "repo:read"]);
    let r = a.api("GET", "/api/v1/notifications", Some(&read), None);
    assert_eq!(r.status, 200, "{}", r.body);
    let id = r.json()["items"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(r.json()["items"][0]["reason"], "watching");
    let r = a.api(
        "PATCH",
        &format!("/api/v1/notifications/{id}"),
        Some(&read),
        Some(&serde_json::json!({"unread": false})),
    );
    assert_eq!(r.status, 403);
    let write = create_token(&mut a, "w", &["user:write"]);
    let r = a.api(
        "PATCH",
        &format!("/api/v1/notifications/{id}"),
        Some(&write),
        Some(&serde_json::json!({"unread": false})),
    );
    assert_eq!(r.status, 204, "{}", r.body);
    // Someone else's notification ID is "not found".
    let bob_token = create_token(&mut bob, "w", &["user:write"]);
    let r = bob.api(
        "PATCH",
        &format!("/api/v1/notifications/{id}"),
        Some(&bob_token),
        Some(&serde_json::json!({"unread": true})),
    );
    assert_eq!(r.status, 404);

    // The unsubscribe link: GET only asks, POST works without a session.
    let link = unsub
        .trim_matches(|c| c == '<' || c == '>')
        .split(['<', '>', ','])
        .find(|p| p.contains("/-/unsubscribe/"))
        .unwrap()
        .trim()
        .to_owned();
    let path = &link[link.find("/-/unsubscribe/").unwrap()..];
    let mut mail_client = s.client();
    mail_client.default_headers.clear();
    let page = mail_client.get(path);
    assert_eq!(page.status, 200);
    assert!(page.body.contains("alice/secret#1"));
    let r = mail_client.request(
        "POST",
        path,
        &[],
        Some((
            "application/x-www-form-urlencoded",
            b"List-Unsubscribe=One-Click".to_vec(),
        )),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    // Change the MAC's last character to a different one (a fixed `A` would
    // leave about one token in 64 unchanged).
    let (head, last) = path.split_at(path.len() - 1);
    let forged = format!("{head}{}", if last == "A" { "B" } else { "A" });
    assert_eq!(mail_client.request("POST", &forged, &[], None).status, 404);

    // carol now ignores the thread; bob loses access. Neither hears more.
    let page = "/alice/secret/settings/access";
    let r = submit(&mut a, page, &format!("{page}/bob/remove"), &[]);
    assert!(r.status == 303 || r.status == 200, "{}", r.status);
    let before = smtp.messages().len();
    let r = submit(
        &mut a,
        "/alice/secret/issues/1",
        "/alice/secret/issues/1/comments",
        &[("body", "second round")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    std::thread::sleep(Duration::from_secs(3));
    let after: Vec<_> = smtp.messages().into_iter().skip(before).collect();
    assert!(
        after.iter().all(|m| !m
            .to
            .iter()
            .any(|t| t.contains("bob@") || t.contains("carol@"))),
        "{:?}",
        after.iter().map(|m| m.to.clone()).collect::<Vec<_>>()
    );
    assert!(
        !bob.get("/-/notifications?all=true")
            .body
            .contains("Leaky title")
    );
}

/// Waits until every queued fan-out (and the emails it queued) ran.
fn settle(s: &TestServer) {
    wait_until("queued notifications", || {
        s.db.query_one(
            "SELECT count(*)::text FROM jobs WHERE kind IN ('notify.fanout', 'email.send')
               AND state IN ('ready', 'running')",
        ) == "0"
    });
}

/// `(subject_type, number, reason)` of someone's notifications.
fn inbox(s: &TestServer, token: &str) -> Vec<(String, i64, String)> {
    let r = s
        .client()
        .api("GET", "/api/v1/notifications?all=true", Some(token), None);
    assert_eq!(r.status, 200, "{}", r.body);
    r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["subject_type"].as_str().unwrap().to_owned(),
                i["number"].as_i64().unwrap(),
                i["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn conf_issue_02_merge_request_notifications() {
    let smtp = SmtpServer::start();
    let s = TestServer::start_full(&[DEMO], &smtp.config());
    for u in ["bob", "carol", "dave"] {
        s.create_user(u, PASSWORD, false);
    }
    s.db.execute("UPDATE users SET email_verified = true");
    let token = |who: &str| {
        let mut c = s.login(who, PASSWORD);
        create_token(&mut c, who, &["user:read", "repo:write", "mrs:write"])
    };
    let (alice, bob, carol, dave) = (token("alice"), token("bob"), token("carol"), token("dave"));
    let api = |t: &str, method: &str, path: &str, body: Option<Value>| {
        s.client().api(
            method,
            &format!("/api/v1/repos/alice/demo{path}"),
            Some(t),
            body.as_ref(),
        )
    };
    for (who, role) in [("bob", "write"), ("carol", "read"), ("dave", "read")] {
        let r = api(
            &alice,
            "PUT",
            &format!("/collaborators/{who}"),
            Some(json!({"role": role})),
        );
        assert!(r.status < 300, "{}", r.body);
    }
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let url = s.url(&DEMO);
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "x\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "-c", "topic"]);
    std::fs::write(w.join("a.txt"), "a\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "Add a"]);
    git.ok(w, &["push", "-q", &url, "topic"]);

    // A draft asks no one to review yet.
    let r = api(
        &alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Draft work", "draft": true})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    let r = api(
        &alice,
        "PUT",
        &format!("/merge-requests/{n}/reviewers"),
        Some(json!({"reviewers": ["bob", "dave"]})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    settle(&s);
    assert!(inbox(&s, &bob).is_empty(), "{:?}", inbox(&s, &bob));
    assert!(inbox(&s, &dave).is_empty());

    // dave loses access; marking it ready asks only bob.
    let r = api(&alice, "DELETE", "/collaborators/dave", None);
    assert!(r.status < 300, "{}", r.body);
    let r = api(
        &alice,
        "PATCH",
        &format!("/merge-requests/{n}"),
        Some(json!({"draft": false})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    settle(&s);
    assert_eq!(
        inbox(&s, &bob),
        vec![("merge_request".to_owned(), n, "mr_ready".to_owned())]
    );
    assert!(inbox(&s, &dave).is_empty());
    let msgs = smtp.messages();
    let subject = format!("[alice/demo] Draft work (!{n})");
    let to_bob = msgs
        .iter()
        .find(|m| {
            m.to.iter().any(|t| t.contains("bob@"))
                && m.header("Subject").as_deref() == Some(subject.as_str())
        })
        .expect("bob was emailed");
    assert!(
        to_bob
            .data
            .contains(&format!("/alice/demo/merge-requests/{n}"))
    );

    // A review reaches the author; a mention of someone without access
    // reaches no one; carol, mentioned, hears about it.
    let r = api(
        &bob,
        "POST",
        &format!("/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "approve", "body": "Fine by me, @dave and @carol."})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    settle(&s);
    assert_eq!(
        inbox(&s, &alice),
        vec![("merge_request".to_owned(), n, "author".to_owned())]
    );
    assert_eq!(
        inbox(&s, &carol),
        vec![("merge_request".to_owned(), n, "mentioned".to_owned())]
    );
    assert!(inbox(&s, &dave).is_empty());
    assert!(
        !smtp
            .messages()
            .iter()
            .any(|m| m.to.iter().any(|t| t.contains("dave@"))
                && m.header("Subject").as_deref() == Some(subject.as_str())),
        "dave lost access"
    );

    // Merging tells the reviewer.
    let before = smtp.messages().len();
    let head = api(&alice, "GET", &format!("/merge-requests/{n}"), None).json()["head_oid"].clone();
    let r = api(
        &alice,
        "POST",
        &format!("/merge-requests/{n}/merge"),
        Some(json!({"expected_head_oid": head})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    settle(&s);
    assert!(
        smtp.messages()
            .iter()
            .skip(before)
            .any(|m| m.to.iter().any(|t| t.contains("bob@"))
                && m.data.contains("merged this merge request")),
        "bob hears about the merge"
    );
}
