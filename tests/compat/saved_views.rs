// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Saved views of the merge request list (REVIEW-16): per user and
//! repository, only query strings the list writes, at most the configured
//! number, never visible to or deletable by anyone else, and never more
//! than a link: opening one lists what its owner can read then.

use serde_json::json;
use slath_tests::http::{Client, submit};
use slath_tests::server::{DEMO, PASSWORD, TestServer};

const LIST: &str = "/alice/demo/merge-requests";
const SAVE: &str = "/alice/demo/merge-requests/views";

fn save(c: &mut Client, page: &str, name: &str, query: &str) -> slath_tests::http::Resp {
    submit(c, page, SAVE, &[("name", name), ("query", query)])
}

/// The names in the View menu, saved ones first.
fn view_menu(c: &mut Client, page: &str) -> Vec<String> {
    let body = c.get(page).body;
    let (_, rest) = body
        .split_once(r#"aria-label="View""#)
        .unwrap_or_else(|| panic!("no view menu: {body}"));
    let menu = rest.split_once("</ul>").unwrap().0;
    menu.split(r#"class="menu-item""#)
        .skip(1)
        .map(|i| {
            let text = i.split_once('>').unwrap().1;
            text.split_once('<')
                .map_or(text, |(t, _)| t)
                .trim()
                .to_owned()
        })
        .collect()
}

#[test]
fn conf_review_16_saved_views_are_per_user_and_bounded() {
    let s = TestServer::start_opts(&[DEMO], "", &[("SLATH__LIMITS__SAVED_VIEWS_PER_REPO", "2")]);
    let mut alice = s.login("alice", PASSWORD);
    let page = format!("{LIST}?state=open&labels=bug&view=mine");

    // Saving stores the list's query and shows it.
    let r = save(
        &mut alice,
        &page,
        "My bugs",
        "state=open&labels=bug&view=mine",
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        r.header("location"),
        Some("/alice/demo/merge-requests?state=open&labels=bug&view=mine")
    );
    let body = alice.get(&page).body;
    assert!(
        body.contains("Delete the saved view <bdi>My bugs</bdi>"),
        "{body}"
    );
    assert!(!body.contains("Save this view"), "{body}");
    assert_eq!(
        view_menu(&mut alice, &page),
        [
            "My bugs",
            "All",
            "Review requests",
            "Mine",
            "Stalled (14 days)",
            "Ready to merge"
        ]
    );
    // The default list offers no saving; another list does.
    assert!(!alice.get(LIST).body.contains("Save this view"));
    assert!(
        alice
            .get(&format!("{LIST}?state=merged"))
            .body
            .contains("Save this view")
    );

    // Saving under the same name replaces; the limit counts names.
    let r = save(&mut alice, &page, "My bugs", "state=open&labels=bug");
    assert_eq!(r.status, 303, "{}", r.body);
    let r = save(&mut alice, &page, "Stalled ones", "state=open&view=stalled");
    assert_eq!(r.status, 303, "{}", r.body);
    let r = save(&mut alice, &page, "Third", "state=closed");
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.body.contains("You can save at most 2 views here"),
        "{}",
        r.body
    );
    assert!(r.body.contains(r#"value="Third""#), "{}", r.body);
    let r = save(&mut alice, &page, "Stalled ones", "state=all&view=stalled");
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        s.db.query_one(
            "SELECT string_agg(name || '=' || query, ',' ORDER BY name) FROM mr_saved_views"
        ),
        "My bugs=state=open&labels=bug,Stalled ones=state=all&view=stalled"
    );

    // Only the list's own query strings, and names that fit.
    let long = "x".repeat(61);
    for (name, query) in [
        ("Bad", "state=open\"><script>"),
        ("Bad", "next=//evil.example"),
        ("", "state=closed"),
        (long.as_str(), "state=closed"),
        ("Two\nlines", "state=closed"),
    ] {
        let r = save(&mut alice, &page, name, query);
        assert_eq!(r.status, 422, "{name}: {query}: {}", r.body);
    }

    // Nobody else sees, opens, or deletes them.
    s.create_user("bob", PASSWORD, false);
    let token = slath_tests::http::create_token(&mut alice, "views", &["repo:write"]);
    let r = s.client().api(
        "PUT",
        "/api/v1/repos/alice/demo/collaborators/bob",
        Some(&token),
        Some(&json!({"role": "read"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let id =
        s.db.query_one("SELECT id::text FROM mr_saved_views WHERE name = 'My bugs'");
    let mut bob = s.login("bob", PASSWORD);
    let bob_page = format!("{LIST}?state=closed");
    let r = bob.get(&bob_page);
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!r.body.contains("My bugs"), "{}", r.body);
    let r = submit(
        &mut bob,
        &bob_page,
        &format!("{LIST}/views/{id}/delete"),
        &[("query", "state=open")],
    );
    assert_eq!(r.status, 404, "{}", r.body);
    // Someone who can't read the repository can't save views of it.
    s.create_user("carol", PASSWORD, false);
    let mut carol = s.login("carol", PASSWORD);
    let r = submit(
        &mut carol,
        "/",
        SAVE,
        &[("name", "Mine"), ("query", "state=open")],
    );
    assert_eq!(r.status, 404, "{}", r.body);
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM mr_saved_views"),
        "2"
    );

    // Alice deletes hers and is back on that list.
    let r = submit(
        &mut alice,
        &format!("{LIST}?state=open&labels=bug"),
        &format!("{LIST}/views/{id}/delete"),
        &[("query", "state=open&labels=bug")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        r.header("location"),
        Some("/alice/demo/merge-requests?state=open&labels=bug")
    );
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM mr_saved_views"),
        "1"
    );
}
