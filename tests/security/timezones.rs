// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Times in the viewer's zone (A11Y-01, D-35): the same instant reads the
//! same on pages, in emails, and (converted from UTC) in the API; the zone
//! is chosen from the bundled database; dates without a time, such as a
//! milestone's due date, are the same calendar date in every zone.

use std::time::Duration;

use serde_json::json;
use slath_core::tz::Zone;
use slath_tests::http::{Client, create_token, submit};
use slath_tests::server::{DEMO, PASSWORD, TestServer};
use slath_tests::smtp::SmtpServer;

fn set_zone(c: &mut Client, zone: &str) -> slath_tests::http::Resp {
    submit(
        c,
        "/-/settings/appearance",
        "/-/settings/appearance",
        &[
            ("theme", "system"),
            ("density", "comfortable"),
            ("code_font_size", "14"),
            ("tool", "auto"),
            ("timezone", zone),
        ],
    )
}

fn unix(rfc3339: &str) -> i64 {
    time::OffsetDateTime::parse(rfc3339, &time::format_description::well_known::Rfc3339)
        .unwrap()
        .unix_timestamp()
}

#[test]
fn conf_a11y_01_times_follow_the_viewers_zone() {
    let smtp = SmtpServer::start();
    let s = TestServer::start_full(&[DEMO], &smtp.config());
    let mut alice = s.login("alice", PASSWORD);
    let token = create_token(&mut alice, "t", &["issues:write", "repo:write"]);

    // The setting: a name from the database; anything else is refused.
    let r = set_zone(&mut alice, "Mars/Olympus_Mons");
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.body.contains("Choose a time zone from the list."),
        "{}",
        r.body
    );
    assert_eq!(set_zone(&mut alice, "Europe/Berlin").status, 303);
    let page = alice.get("/-/settings/appearance").body;
    assert!(
        page.contains(r#"<option value="Europe/Berlin" selected>Berlin</option>"#),
        "{page}"
    );
    assert!(
        !page.contains("data-unset"),
        "a saved zone isn't proposed over"
    );
    assert!(
        page.contains(r#"<option value="Etc/GMT+12">UTC−12</option>"#),
        "{page}"
    );

    // An issue's time: the API in UTC, the page in Berlin time.
    let r = alice.api(
        "POST",
        "/api/v1/repos/alice/demo/issues",
        Some(&token),
        Some(&json!({"title": "When?"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let created = r.json()["created_at"].as_str().unwrap().to_owned();
    assert!(created.ends_with('Z'), "{created}");
    let berlin = Zone::named("Europe/Berlin")
        .unwrap()
        .local(unix(&created))
        .exact();
    assert!(
        berlin.ends_with(" CET") || berlin.ends_with(" CEST"),
        "{berlin}"
    );
    let issue = alice.get("/alice/demo/issues/1").body;
    assert!(issue.contains(&berlin), "{berlin} not in the page");
    // Anonymous visitors, and alice's API, see UTC.
    let utc = Zone::utc().local(unix(&created)).exact();
    let anon = s.client().get("/alice/demo/issues/1");
    if anon.status == 200 {
        assert!(anon.body.contains(&utc), "{utc}");
    }

    // An email in alice's zone: the security notice of an MFA reset.
    let before = time::OffsetDateTime::now_utc().unix_timestamp();
    let (ok, _, err) = s.slath(&["admin", "reset-mfa", "--username", "alice"], None);
    assert!(ok, "{err}");
    let after = time::OffsetDateTime::now_utc().unix_timestamp();
    let m = smtp.wait_for_text(
        "two-factor authentication of your account",
        Duration::from_secs(20),
    );
    let zone = Zone::named("Europe/Berlin").unwrap();
    let expected: Vec<String> = (before..=after + 60)
        .step_by(30)
        .map(|t| format!("Time: {}", zone.local(t).exact()))
        .collect();
    let text = m.data.replace("=\r\n", "");
    assert!(
        expected.iter().any(|e| text.contains(e)),
        "{expected:?} not in:\n{text}"
    );
}

/// A milestone due on 1 December is due on 1 December for a viewer in
/// UTC+14 and one in UTC−12, though their clocks are two days apart.
#[test]
fn conf_a11y_01_due_dates_are_calendar_dates_in_every_zone() {
    let s = TestServer::start_full(&[DEMO], "");
    let mut alice = s.login("alice", PASSWORD);
    let token = create_token(&mut alice, "t", &["issues:write", "repo:write"]);
    let r = alice.api(
        "PATCH",
        "/api/v1/repos/alice/demo",
        Some(&token),
        Some(&json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let r = alice.api(
        "POST",
        "/api/v1/repos/alice/demo/milestones",
        Some(&token),
        Some(&json!({"title": "Release", "due_on": "2026-12-01"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let mut seen = Vec::new();
    for (user, zone) in [("kiri", "Pacific/Kiritimati"), ("baker", "Etc/GMT+12")] {
        s.create_user(user, PASSWORD, false);
        let mut c = s.login(user, PASSWORD);
        assert_eq!(set_zone(&mut c, zone).status, 303, "{zone}");
        let page = c.get("/alice/demo/milestones").body;
        let due = page
            .split("Due <time")
            .nth(1)
            .and_then(|p| p.split("</time>").next())
            .unwrap_or_else(|| panic!("{zone}: no due date in {page}"))
            .to_owned();
        seen.push(due);
    }
    assert_eq!(seen[0], seen[1]);
    assert!(seen[0].contains("2026-12-01"), "{}", seen[0]);
}
