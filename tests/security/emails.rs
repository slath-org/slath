// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! An account's email addresses: several of them, one primary chosen by
//! the account, each confirmed by a link (or an administrator) before it
//! counts, and a confirmed address belonging to one account only.

use std::time::Duration;

use slath_tests::http::{Client, submit};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};
use slath_tests::smtp::{Received, SmtpServer};

const PW: &str = "correct horse battery staple";

/// Opens the confirmation link in `m` and confirms; returns the status.
fn confirm(c: &mut Client, m: &Received) -> u16 {
    let body = m.data.replace("=\r\n", "").replace("=3D", "=");
    let start = body
        .find("/-/verify-email?token=")
        .expect("a confirmation link");
    let path = body[start..]
        .split(char::is_whitespace)
        .next()
        .unwrap()
        .to_owned();
    let token = path.split("token=").nth(1).unwrap().to_owned();
    submit(c, &path, "/-/verify-email", &[("token", &token)]).status
}

fn addresses(s: &TestServer, user: &str) -> String {
    s.db.query_one(&format!(
        "SELECT coalesce(string_agg(email || ':' || verified, ',' ORDER BY email), '') FROM user_emails
         WHERE user_id = (SELECT u.id FROM users u JOIN namespaces n ON n.id = u.namespace_id WHERE n.name = '{user}')"
    ))
}

fn primary(s: &TestServer, user: &str) -> String {
    s.db.query_one(&format!(
        "SELECT email || ':' || email_verified FROM users
         WHERE namespace_id = (SELECT id FROM namespaces WHERE name = '{user}')"
    ))
}

#[test]
fn several_addresses_one_primary_chosen_by_the_account() {
    let smtp = SmtpServer::start();
    let s = TestServer::start_full(&[], &smtp.config());
    s.create_user("alice", PW, false);
    s.create_user("bob", PW, false);
    let m = smtp.wait_for(2, Duration::from_secs(20));
    let alice_mail = m
        .iter()
        .find(|m| m.to == vec!["<alice@example.org>"])
        .unwrap();
    assert_eq!(confirm(&mut s.client(), alice_mail), 200);
    let mut a = s.login("alice", PW);

    // A second address waits for its link; it can't be primary before.
    let r = submit(
        &mut a,
        "/-/settings/emails",
        "/-/settings/emails",
        &[("email", "Alice@Work.example")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(addresses(&s, "alice"), "Alice@Work.example:false");
    let r = submit(
        &mut a,
        "/-/settings/emails",
        "/-/settings/emails/primary",
        &[("email", "alice@work.example")],
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("Confirm this address before"), "{}", r.body);
    // The same address twice is refused.
    let r = submit(
        &mut a,
        "/-/settings/emails",
        "/-/settings/emails",
        &[("email", "alice@work.example")],
    );
    assert_eq!(r.status, 422);

    let m = smtp.wait_for(3, Duration::from_secs(20));
    assert_eq!(m[2].to, vec!["<Alice@Work.example>"]);
    assert_eq!(confirm(&mut s.client(), &m[2]), 200);
    assert_eq!(addresses(&s, "alice"), "Alice@Work.example:true");
    let page = a.get("/-/settings/emails").body;
    assert!(
        page.contains("alice@example.org</bdi> <span class=\"badge\">Primary</span>"),
        "{page}"
    );
    assert_eq!(page.matches(">Confirmed<").count(), 2, "{page}");

    // Another account can add it, but not confirm it.
    let mut b = s.login("bob", PW);
    let r = submit(
        &mut b,
        "/-/settings/emails",
        "/-/settings/emails",
        &[("email", "alice@work.example")],
    );
    assert_eq!(r.status, 303);
    let m = smtp.wait_for(4, Duration::from_secs(20));
    assert_eq!(confirm(&mut s.client(), &m[3]), 409);
    assert_eq!(addresses(&s, "bob"), "alice@work.example:false");

    // The primary address can't go while there are others.
    let r = submit(
        &mut a,
        "/-/settings/emails",
        "/-/settings/emails/remove",
        &[("email", "alice@example.org")],
    );
    assert_eq!(r.status, 422);
    assert!(
        r.body.contains("Make another address primary"),
        "{}",
        r.body
    );

    // Choosing another primary swaps them, and tells the old one.
    let r = submit(
        &mut a,
        "/-/settings/emails",
        "/-/settings/emails/primary",
        &[("email", "alice@work.example")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(primary(&s, "alice"), "Alice@Work.example:true");
    assert_eq!(addresses(&s, "alice"), "alice@example.org:true");
    let m = smtp.wait_for(5, Duration::from_secs(20));
    assert_eq!(m[4].to, vec!["<alice@example.org>"]);
    assert!(
        m[4].data
            .contains("Subject: Your Slath primary email address was changed"),
        "{}",
        m[4].data
    );

    // The old one can go now; another account's address can't.
    let r = submit(
        &mut a,
        "/-/settings/emails",
        "/-/settings/emails/remove",
        &[("email", "bob@example.org")],
    );
    assert_eq!(r.status, 404);
    let r = submit(
        &mut a,
        "/-/settings/emails",
        "/-/settings/emails/remove",
        &[("email", "alice@example.org")],
    );
    assert_eq!(r.status, 303);
    assert_eq!(addresses(&s, "alice"), "");
}

#[test]
fn administrators_confirm_addresses_and_commits_with_any_of_them_show_the_account() {
    let s = TestServer::start();
    // Without email, the command line confirms them.
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "add-email",
            "--username",
            "alice",
            "--email",
            "alice@work.example",
        ],
        None,
    );
    assert!(ok, "{out}{err}");
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "add-email",
            "--username",
            "alice",
            "--email",
            "a.example@home.example",
        ],
        None,
    );
    assert!(ok, "{out}{err}");
    s.create_user("bob", PASSWORD, false);
    let (ok, _, err) = s.slath(
        &[
            "admin",
            "add-email",
            "--username",
            "bob",
            "--email",
            "alice@work.example",
        ],
        None,
    );
    assert!(
        !ok && err.contains("Another account has already confirmed"),
        "{err}"
    );
    assert_eq!(
        addresses(&s, "alice"),
        "a.example@home.example:true,alice@work.example:true"
    );

    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    git.ok(w, &["init", "-q", "-b", "main"]);
    for (i, email) in ["alice@work.example", "a.example@home.example"]
        .iter()
        .enumerate()
    {
        std::fs::write(w.join("f.txt"), format!("{i}\n")).unwrap();
        git.ok(w, &["add", "f.txt"]);
        git.ok(
            w,
            &[
                "-c",
                &format!("user.email={email}"),
                "commit",
                "-q",
                "-m",
                &format!("Commit {i}"),
            ],
        );
    }
    git.ok(w, &["push", "-q", &s.url(&DEMO), "main"]);
    let page = s.login("alice", PASSWORD).get("/alice/demo/commits").body;
    assert_eq!(
        page.matches("<a class=\"account\" href=\"/alice\"").count(),
        2,
        "{page}"
    );
}
