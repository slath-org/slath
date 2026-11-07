// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The opt-in update check: no request unless enabled (SEC-07), and the
//! answer shown to site administrators only.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use slath_tests::server::TestServer;

const PW: &str = "correct horse battery staple";

/// A release feed on a local port; counts the requests it gets.
fn feed(body: &'static str) -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/releases", l.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(mut s) = stream else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            while r.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, hits)
}

const FEED: &str = r#"[
  {"tag_name": "v9.9.9", "html_url": "https://example.org/releases/v9.9.9", "draft": false, "prerelease": false},
  {"tag_name": "v10.0.0-rc.1", "html_url": "https://example.org/rc", "draft": false, "prerelease": true},
  {"tag_name": "v0.0.1", "html_url": "https://example.org/old", "draft": false, "prerelease": false}
]"#;

#[test]
fn conf_sec_07_the_update_check_makes_no_request_unless_enabled() {
    let (url, hits) = feed(FEED);
    let s = TestServer::start_full(&[], &format!("[upgrade]\nfeed_url = \"{url}\"\n"));
    s.create_user("boss", PW, true);
    let mut admin = s.login("boss", PW);
    for page in ["/-/admin/health", "/-/admin/upgrades"] {
        let r = admin.get(page);
        assert_eq!(r.status, 200);
        assert!(!r.body.contains("is available"), "{page}");
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(hits.load(Ordering::SeqCst), 0, "no request while disabled");
}

#[test]
fn enabled_update_check_shows_the_newest_release_to_admins_only() {
    let (url, hits) = feed(FEED);
    let s = TestServer::start_full(
        &[],
        &format!("[upgrade]\ncheck_for_updates = true\nfeed_url = \"{url}\"\n"),
    );
    s.create_user("boss", PW, true);
    s.create_user("alice", PW, false);
    let mut admin = s.login("boss", PW);
    let start = std::time::Instant::now();
    let page = loop {
        let r = admin.get("/-/admin/upgrades");
        if r.body.contains("is available") {
            break r;
        }
        assert!(start.elapsed().as_secs() < 20, "{}", s.log());
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    assert!(
        page.body.contains("Slath 9.9.9 is available"),
        "{}",
        page.body
    );
    assert!(
        page.body
            .contains("href=\"https://example.org/releases/v9.9.9\"")
    );
    assert!(
        admin
            .get("/-/admin/health")
            .body
            .contains("Slath 9.9.9 is available")
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1, "one request at start");
    // Other users don't see administration at all.
    let mut alice = s.login("alice", PW);
    assert_eq!(alice.get("/-/admin/upgrades").status, 404);
}
