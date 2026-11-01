// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search over HTTP (RBAC-01, ISSUE-02): the page and the API return only
//! what the caller can read; a token without repo:read searches like an
//! anonymous visitor.

use slath_tests::http::{create_repo, create_token, submit};
use slath_tests::server::{PASSWORD, TestServer};

#[test]
fn conf_rbac_01_search_shows_only_readable_matches() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PASSWORD, false);
    s.create_user("bob", PASSWORD, false);
    let mut a = s.login("alice", PASSWORD);
    create_repo(&mut a, "alice", "open", "public");
    create_repo(&mut a, "alice", "hidden", "private");
    for (repo, title) in [("open", "Public walrus"), ("hidden", "Secret walrus")] {
        let r = submit(
            &mut a,
            &format!("/alice/{repo}/issues/new"),
            &format!("/alice/{repo}/issues"),
            &[("title", title), ("body", "")],
        );
        assert_eq!(r.status, 303, "{}", r.body);
    }

    let page = a.get("/-/search?q=walrus");
    assert!(page.body.contains("Secret walrus") && page.body.contains("Public walrus"));
    let mut bob = s.login("bob", PASSWORD);
    for c in [&mut bob, &mut s.client()] {
        let page = c.get("/-/search?q=walrus");
        assert_eq!(page.status, 200);
        assert!(page.body.contains("Public walrus"));
        assert!(!page.body.contains("Secret walrus"), "{}", page.body);
        let page = c.get("/-/search?q=hid&type=repos");
        assert!(!page.body.contains("alice/hidden"));
        assert_eq!(c.get("/-/search?q=walrus&repo=alice/hidden").status, 404);
    }

    let full = create_token(&mut a, "full", &["repo:read"]);
    let r = a.api("GET", "/api/v1/search/issues?q=walrus", Some(&full), None);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 2);
    let narrow = create_token(&mut a, "narrow", &["user:read"]);
    let r = a.api("GET", "/api/v1/search/issues?q=walrus", Some(&narrow), None);
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 1);
    assert!(!r.body.contains("Secret"));
    let r = a.api("GET", "/api/v1/search/repos?q=hidden", None, None);
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 0);
    let r = a.api("GET", "/api/v1/search/repos?q=hidden", Some(&full), None);
    assert_eq!(r.json()["items"][0]["visibility"], "private");
}
