// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reference autocomplete over HTTP (EDITOR-02): both endpoints answer
//! only for readable repositories, never include email addresses, and
//! issue pages render label and milestone references per viewer.

use slath_tests::http::{create_repo, create_token, submit};
use slath_tests::server::{PASSWORD, TestServer};

#[test]
fn conf_editor_02_autocomplete_endpoints() {
    let s = TestServer::start_with(&[]);
    for u in ["alice", "bob"] {
        s.create_user(u, PASSWORD, false);
    }
    let mut a = s.login("alice", PASSWORD);
    create_repo(&mut a, "alice", "priv", "private");
    let r = submit(
        &mut a,
        "/alice/priv/issues/new",
        "/alice/priv/issues",
        &[("title", "Secret roadmap"), ("body", "")],
    );
    assert_eq!(r.status, 303, "{}", r.body);

    let r = a.get("/-/ui/references?repo=alice/priv&kind=item&q=road");
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["items"][0]["value"], "#1");
    assert_eq!(r.header("cache-control"), Some("private, no-store"));
    let r = a.get("/-/ui/references?repo=alice/priv&kind=user&q=");
    assert_eq!(r.status, 200);
    assert!(!r.body.contains("example"), "no emails: {}", r.body);
    assert!(r.body.contains("\"bob\""));

    let mut bob = s.login("bob", PASSWORD);
    assert_eq!(
        bob.get("/-/ui/references?repo=alice/priv&kind=item&q=")
            .status,
        404
    );
    assert_eq!(
        s.client()
            .get("/-/ui/references?repo=alice/priv&kind=item&q=")
            .status,
        401
    );
    assert_eq!(
        a.get("/-/ui/references?repo=alice/priv&kind=bogus").status,
        422
    );
    assert_eq!(
        bob.get("/-/ui/references?repo=alice/priv&kind=bogus")
            .status,
        404,
        "the kind is checked only after access"
    );

    let token = create_token(&mut a, "t", &["repo:read"]);
    let r = a.api(
        "GET",
        "/api/v1/repos/alice/priv/references?kind=item&q=1",
        Some(&token),
        None,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["items"][0]["label"], "Secret roadmap");
    let bob_token = create_token(&mut bob, "t", &["repo:read"]);
    let r = bob.api(
        "GET",
        "/api/v1/repos/alice/priv/references?kind=item&q=",
        Some(&bob_token),
        None,
    );
    assert_eq!(r.status, 404);
}
