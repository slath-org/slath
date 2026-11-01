// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Outbound email: queued delivery with retries, headers, and email
//! features hidden without SMTP.

use std::time::Duration;

use slath_tests::http::submit;
use slath_tests::server::TestServer;
use slath_tests::smtp::SmtpServer;

const PW: &str = "correct horse battery staple";

#[test]
fn email_is_retried_after_temporary_failures() {
    let smtp = SmtpServer::start();
    let s = TestServer::start_full(&[], &smtp.config());
    s.create_user("boss", PW, true);
    let mut boss = s.login("boss", PW);
    let page = boss.get("/-/admin/health");
    assert!(page.body.contains("Send a test message"), "{}", page.body);
    smtp.fail_next(1);
    let r = submit(
        &mut boss,
        "/-/admin/health",
        "/-/admin/health/test-email",
        &[("to", "ops@example.org")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    // The first attempt gets 451; the job retries with backoff.
    // (The account's own confirmation link went out at creation.)
    let start = std::time::Instant::now();
    let m = loop {
        let m: Vec<_> = smtp
            .messages()
            .into_iter()
            .filter(|m| m.to == vec!["<ops@example.org>"])
            .collect();
        if !m.is_empty() {
            break m;
        }
        assert!(start.elapsed() < Duration::from_secs(40), "no test message");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        m[0].header("Subject").as_deref(),
        Some("Slath test message")
    );
    assert_eq!(
        m[0].header("Auto-Submitted").as_deref(),
        Some("auto-generated")
    );
    assert!(m[0].header("Message-ID").unwrap().ends_with("@127.0.0.1>"));
    assert!(m[0].data.contains("outgoing email works"));
    assert_eq!(
        s.db.query_one(
            "SELECT attempts::text || ' ' || state FROM jobs \
             WHERE kind = 'email.send' AND payload->>'to' = 'ops@example.org'"
        ),
        "2 done"
    );
    // The queued body is not stored in plain text.
    let payload = s.db.query_one(
        "SELECT payload::text FROM jobs WHERE kind = 'email.send' AND payload->>'to' = 'ops@example.org'",
    );
    assert!(!payload.contains("outgoing email works"), "{payload}");

    // The CLI sends synchronously and reports failures.
    let (ok, out, err) = s.slath(
        &["admin", "send-test-email", "--to", "cli@example.org"],
        None,
    );
    assert!(ok, "{out}{err}");
    assert!(
        smtp.messages()
            .iter()
            .any(|m| m.to == vec!["<cli@example.org>"])
    );
    smtp.fail_next(1);
    let (ok, _, err) = s.slath(
        &["admin", "send-test-email", "--to", "cli@example.org"],
        None,
    );
    assert!(!ok && err.contains("451"), "{err}");
}

#[test]
fn without_smtp_email_features_are_hidden() {
    let s = TestServer::start_with(&[]);
    s.create_user("boss", PW, true);
    let mut boss = s.login("boss", PW);
    let page = boss.get("/-/admin/health");
    assert!(page.body.contains("not configured"));
    assert!(!page.body.contains("Send a test message"));
    let r = submit(
        &mut boss,
        "/-/admin/health",
        "/-/admin/health/test-email",
        &[("to", "ops@example.org")],
    );
    assert_eq!(r.status, 404);
    let (ok, _, err) = s.slath(&["admin", "send-test-email", "--to", "x@example.org"], None);
    assert!(!ok && err.contains("not configured"), "{err}");
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM jobs WHERE kind = 'email.send'"),
        "0"
    );
}
