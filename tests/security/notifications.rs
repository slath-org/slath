// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Notifications over HTTP and email (ISSUE-02): the inbox, the API with
//! its scopes, threaded emails with an unsubscribe link that works without
//! a session, and no email to someone who lost access.

use std::time::Duration;

use slath_tests::http::{create_repo, create_token, submit};
use slath_tests::server::{PASSWORD, TestServer};
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
    let forged = format!("{}A", &path[..path.len() - 1]);
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
