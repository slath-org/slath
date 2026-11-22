// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The readiness snapshot behind merge request lists (REVIEW-16): the open
//! list groups each merge request under what its readiness card shows, after every kind of
//! change that affects it; the snapshot is rebuildable, `fsck` finds and
//! repairs a wrong one, and it never allows a merge.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::server::PASSWORD;

use crate::change_identity::{Ctx, commit_file, seed};

/// One readiness group of the open list.
#[derive(Debug, PartialEq, Eq)]
struct Group {
    tone: String,
    headline: String,
    count: i64,
    numbers: Vec<i64>,
}

/// The open list's groups in their order.
fn groups(c: &Ctx) -> Vec<Group> {
    let body =
        c.s.login("alice", PASSWORD)
            .get("/alice/demo/merge-requests")
            .body;
    groups_in(&body)
}

/// The numbers a list page shows, in their order.
fn numbers_in(body: &str) -> Vec<i64> {
    body.split(r#"class="item-title" href="/alice/demo/merge-requests/"#)
        .skip(1)
        .map(|r| r.split_once('"').unwrap().0.parse().unwrap())
        .collect()
}

/// A list page's groups in their order.
fn groups_in(body: &str) -> Vec<Group> {
    body.split(r#"<h2 class="item-group-heading""#)
        .skip(1)
        .map(|g| {
            let tone = g
                .split_once(r#"<span class="item-group-tone tone-"#)
                .and_then(|(_, r)| r.split_once('"'))
                .map(|(t, _)| t.to_owned())
                .unwrap_or_default();
            let (_, rest) = g.split_once("</span><span>").unwrap();
            let (headline, rest) = rest.split_once("</span>").unwrap();
            let count = rest
                .split_once(r#"<span class="count">"#)
                .and_then(|(_, r)| r.split_once('<'))
                .map(|(n, _)| n.parse().unwrap())
                .unwrap();
            let section = g.split_once("</section>").map_or(g, |(s, _)| s);
            let numbers = numbers_in(section);
            Group {
                tone,
                headline: headline.replace("&#x27;", "'"),
                count,
                numbers,
            }
        })
        .collect()
}

/// The only open merge request's group in the list: tone and headline.
fn listed(c: &Ctx) -> (String, String) {
    let mut g = groups(c);
    assert_eq!(g.len(), 1, "{g:?}");
    let g = g.remove(0);
    assert_eq!(g.count, 1, "{g:?}");
    (g.tone, g.headline)
}

/// The only open merge request's check and approval states in the list,
/// each a tone and words.
fn row_states(c: &Ctx) -> Vec<(String, String)> {
    let body =
        c.s.login("alice", PASSWORD)
            .get("/alice/demo/merge-requests")
            .body;
    let Some((_, rest)) = body.split_once(r#"<p class="item-meta item-status">"#) else {
        return Vec::new();
    };
    let row = rest.split_once("</p>").unwrap().0;
    row.split(r#"status-inline tone-"#)
        .skip(1)
        .map(|s| {
            let (tone, rest) = s.split_once('"').unwrap();
            let text = rest
                .split_once("</span><span>")
                .and_then(|(_, r)| r.split_once("</span>"))
                .unwrap()
                .0;
            (tone.to_owned(), text.to_owned())
        })
        .collect()
}

fn states(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(t, w)| ((*t).to_owned(), (*w).to_owned()))
        .collect()
}

/// The readiness card's headline on the merge request's page.
fn card(c: &Ctx, n: i64) -> String {
    let body =
        c.s.login("alice", PASSWORD)
            .get(&format!("/alice/demo/merge-requests/{n}"))
            .body;
    let (_, rest) = body
        .split_once(r#"class="readiness-title">"#)
        .unwrap_or_else(|| panic!("no card: {body}"));
    rest.split_once('<').unwrap().0.to_owned()
}

/// The list shows `tone` and `headline`, and so does the card.
fn shows(c: &Ctx, n: i64, tone: &str, headline: &str) {
    assert_eq!(card(c, n), headline, "card");
    let got = listed(c);
    assert_eq!(got, (tone.to_owned(), headline.to_owned()), "list");
}

fn protect(c: &Ctx, body: Value) {
    let r = c.api("POST", "/protections", Some(&body));
    assert_eq!(r.status, 200, "{}", r.body);
}

fn bob(c: &Ctx) -> String {
    c.s.create_user("bob", PASSWORD, false);
    let r = c.api("PUT", "/collaborators/bob", Some(&json!({"role": "write"})));
    assert!(r.status < 300, "{}", r.body);
    let mut b = c.s.login("bob", PASSWORD);
    slath_tests::http::create_token(&mut b, "bob", &["repo:write", "mrs:write"])
}

/// One open merge request from `topic`, adding a.txt.
fn topic(c: &Ctx, w: &Path) -> (i64, String) {
    c.git.ok(w, &["switch", "-q", "-c", "topic", "origin/main"]);
    let head = commit_file(&c.git, w, "a.txt", "topic\n", "Add a");
    c.push(w, "topic");
    let n = c.open_mr("topic");
    c.stack(n, 1);
    (n, head)
}

#[test]
fn conf_review_16_the_list_shows_the_cards_readiness_after_every_change() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    seed(&c, d.path());
    let w = d.path().join("w");
    c.clone(&w);
    let (n, head) = topic(&c, &w);
    shows(&c, n, "done", "Ready to merge");
    // Without review rules, checks, or reviews, the row says nothing more.
    assert_eq!(row_states(&c), states(&[]));

    // Protection of the target (all its merge requests).
    protect(&c, json!({"pattern": "main", "required_approvals": 1}));
    shows(&c, n, "waiting", "Approval required");
    assert_eq!(row_states(&c), states(&[("waiting", "Approval required")]));
    // A collaborator and their review.
    let bob = bob(&c);
    let r = c.s.client().api(
        "POST",
        &format!("/api/v1/repos/alice/demo/merge-requests/{n}/reviews"),
        Some(&bob),
        Some(&json!({"verdict": "approve"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    shows(&c, n, "done", "Ready to merge");
    assert_eq!(row_states(&c), states(&[("done", "Approved")]));
    // The merge request itself.
    let r = c.api(
        "PATCH",
        &format!("/merge-requests/{n}"),
        Some(&json!({"draft": true})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    shows(&c, n, "waiting", "Draft");
    let r = c.api(
        "PATCH",
        &format!("/merge-requests/{n}"),
        Some(&json!({"draft": false})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    shows(&c, n, "done", "Ready to merge");
    // Statuses of its head.
    // Reported by bob: the author's own statuses don't count.
    let ci = {
        let mut b = c.s.login("bob", PASSWORD);
        slath_tests::http::create_token(&mut b, "ci", &["statuses:write"])
    };
    protect(
        &c,
        json!({"pattern": "main", "required_approvals": 1, "required_status_contexts": ["ci"]}),
    );
    shows(&c, n, "waiting", "Checks running");
    for (state, tone, headline, checks) in [
        (
            "failure",
            "blocking",
            "Checks failing",
            ("blocking", "1 of 1 check failed"),
        ),
        (
            "success",
            "done",
            "Ready to merge",
            ("done", "The check passed"),
        ),
    ] {
        let r = c.s.client().api(
            "POST",
            &format!("/api/v1/repos/alice/demo/statuses/{head}"),
            Some(&ci),
            Some(&json!({"context": "ci", "state": state})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        shows(&c, n, tone, headline);
        assert_eq!(row_states(&c), states(&[checks, ("done", "Approved")]));
    }
    // A push to the target.
    c.git
        .ok(&w, &["switch", "-q", "-c", "other", "origin/main"]);
    commit_file(&c.git, &w, "a.txt", "main\n", "Add a on main");
    c.push(&w, "other:main");
    shows(&c, n, "blocking", "Conflicts with main");
}

#[test]
fn conf_review_16_the_snapshot_is_rebuildable_and_never_allows_a_merge() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    seed(&c, d.path());
    let w = d.path().join("w");
    c.clone(&w);
    protect(&c, json!({"pattern": "main", "required_approvals": 1}));
    let (n, head) = topic(&c, &w);
    shows(&c, n, "waiting", "Approval required");

    // A wrong snapshot shows in the list, and only there.
    let wrong = "UPDATE mr_readiness SET tone = 'done', headline = 'Ready to merge'";
    c.s.db.execute(wrong);
    assert_eq!(listed(&c), ("done".into(), "Ready to merge".into()));
    assert_eq!(card(&c, n), "Approval required");
    let r = c.api(
        "POST",
        &format!("/merge-requests/{n}/merge"),
        Some(&json!({"strategy": "rebase", "expected_head_oid": head})),
    );
    assert_eq!(r.status, 409, "{}", r.body);

    // fsck finds it, and --reconcile stores the live result.
    let (ok, out, err) = c.s.slath(&["fsck", "--output", "json"], None);
    assert!(!ok, "{out}{err}");
    let report: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        report["repositories"][0]["readiness_differences"], 1,
        "{report}"
    );
    let (ok, out, err) = c.s.slath(&["fsck", "--reconcile"], None);
    assert!(ok, "{out}{err}");
    assert!(out.contains("rebuilt "), "{out}");
    shows(&c, n, "waiting", "Approval required");
    let (ok, out, err) = c.s.slath(&["fsck"], None);
    assert!(ok, "{out}{err}");

    // A snapshot of an older format is computed again.
    c.s.db.execute(&format!("{wrong}, built_format = 1"));
    shows(&c, n, "waiting", "Approval required");

    // Any change makes the list compute it again.
    c.s.db.execute(wrong);
    let r = c.api(
        "PATCH",
        &format!("/merge-requests/{n}"),
        Some(&json!({"title": "Renamed"})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    shows(&c, n, "waiting", "Approval required");
}

#[test]
fn conf_review_16_the_open_list_is_grouped_by_the_cards_headline() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    seed(&c, d.path());
    let w = d.path().join("w");
    c.clone(&w);
    let open = |branch: &str, file: &str| {
        c.git.ok(&w, &["switch", "-q", "-c", branch, "origin/main"]);
        commit_file(&c.git, &w, file, "topic\n", &format!("Add {file}"));
        c.push(&w, branch);
        let n = c.open_mr(branch);
        c.stack(n, 1);
        n
    };
    let ready = open("ready", "a.txt");
    let draft = open("drafted", "b.txt");
    let conflict = open("conflict", "c.txt");
    let ready_too = open("ready-too", "d.txt");
    let r = c.api(
        "PATCH",
        &format!("/merge-requests/{draft}"),
        Some(&json!({"draft": true})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    c.git
        .ok(&w, &["switch", "-q", "-c", "other", "origin/main"]);
    commit_file(&c.git, &w, "c.txt", "main\n", "Add c on main");
    c.push(&w, "other:main");

    // Ready to merge first, then what needs action, then waiting; newest
    // first within a group.
    let group = |tone: &str, headline: &str, numbers: Vec<i64>| Group {
        tone: tone.into(),
        headline: headline.into(),
        count: numbers.len() as i64,
        numbers,
    };
    assert_eq!(
        groups(&c),
        [
            group("done", "Ready to merge", vec![ready_too, ready]),
            group("blocking", "Conflicts with main", vec![conflict]),
            group("waiting", "Draft", vec![draft]),
        ]
    );
    for (n, headline) in [
        (ready, "Ready to merge"),
        (conflict, "Conflicts with main"),
        (draft, "Draft"),
    ] {
        assert_eq!(card(&c, n), headline);
    }
    // A merge request that changes moves to its new group.
    let r = c.api(
        "PATCH",
        &format!("/merge-requests/{draft}"),
        Some(&json!({"draft": false})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        groups(&c),
        [
            group("done", "Ready to merge", vec![ready_too, draft, ready]),
            group("blocking", "Conflicts with main", vec![conflict]),
        ]
    );
    // Other states aren't grouped.
    let all =
        c.s.login("alice", PASSWORD)
            .get("/alice/demo/merge-requests?state=all")
            .body;
    assert!(!all.contains("item-group-heading"), "{all}");
}

#[test]
fn conf_review_16_views_list_exactly_the_readable_matching_merge_requests() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    seed(&c, d.path());
    let w = d.path().join("w");
    c.clone(&w);
    let bob = bob(&c);
    let push = |branch: &str, file: &str| {
        c.git.ok(&w, &["switch", "-q", "-c", branch, "origin/main"]);
        commit_file(&c.git, &w, file, "topic\n", &format!("Add {file}"));
        c.push(&w, branch);
    };
    // Alice's, ready to merge.
    push("mine", "a.txt");
    let mine = c.open_mr("mine");
    c.stack(mine, 1);
    // Bob's, with Alice asked to review it.
    push("bobs", "b.txt");
    let api_bob = |method: &str, path: &str, body: Value| {
        c.s.client().api(
            method,
            &format!("/api/v1/repos/alice/demo{path}"),
            Some(&bob),
            Some(&body),
        )
    };
    let r = api_bob(
        "POST",
        "/merge-requests",
        json!({"source_branch": "bobs", "title": "bobs"}),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let bobs = r.json()["number"].as_i64().unwrap();
    c.stack(bobs, 1);
    let r = api_bob(
        "PUT",
        &format!("/merge-requests/{bobs}/reviewers"),
        json!({"reviewers": ["alice"]}),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    // Alice's draft, without activity for 20 days.
    push("old", "c.txt");
    let old = c.open_mr("old");
    c.stack(old, 1);
    let r = c.api(
        "PATCH",
        &format!("/merge-requests/{old}"),
        Some(&json!({"draft": true})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    c.s.db.execute(&format!(
        "UPDATE merge_requests SET updated_at = now() - interval '20 days' WHERE number = {old}"
    ));

    let mut alice = c.s.login("alice", PASSWORD);
    let list = |alice: &mut slath_tests::http::Client, query: &str| {
        let r = alice.get(&format!("/alice/demo/merge-requests{query}"));
        assert_eq!(r.status, 200, "{query}: {}", r.body);
        r.body
    };
    // Grouped as usual: ready ones first, newest first among them.
    for (query, want) in [
        ("", vec![bobs, mine, old]),
        ("?view=review-requests", vec![bobs]),
        ("?view=mine", vec![mine, old]),
        ("?view=stalled", vec![old]),
        ("?view=ready", vec![bobs, mine]),
        ("?view=ready&author=alice", vec![mine]),
        ("?view=mine&author=bob", vec![]),
        ("?view=nonsense", vec![bobs, mine, old]),
    ] {
        assert_eq!(numbers_in(&list(&mut alice, query)), want, "{query}");
    }
    // The state menu counts the view's merge requests, and links keep it.
    let page = list(&mut alice, "?view=mine");
    assert!(
        page.contains(r#"href="?state=merged&#38;view=mine""#),
        "{page}"
    );
    assert!(
        page.contains(r#"Open <span class="count">2</span>"#),
        "{page}"
    );
    assert!(numbers_in(&list(&mut alice, "?state=merged&view=mine")).is_empty());
    // Bob is asked for nothing; his merge request is his.
    let mut bob_ui = c.s.login("bob", PASSWORD);
    assert!(numbers_in(&list(&mut bob_ui, "?view=review-requests")).is_empty());
    assert_eq!(numbers_in(&list(&mut bob_ui, "?view=mine")), vec![bobs]);

    // A view is a filter, never a grant: someone who can't read the
    // repository gets nothing, and personal views need an account.
    c.s.create_user("carol", PASSWORD, false);
    let mut carol = c.s.login("carol", PASSWORD);
    for query in ["?view=ready", "?view=stalled", "?view=mine"] {
        let r = carol.get(&format!("/alice/demo/merge-requests{query}"));
        assert_eq!(r.status, 404, "{query}");
    }
    let r = c.api("PATCH", "", Some(&json!({"visibility": "public"})));
    assert!(r.status < 300, "{}", r.body);
    let mut anonymous = c.s.client();
    let r = anonymous.get("/alice/demo/merge-requests?view=ready");
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(numbers_in(&r.body), vec![bobs, mine]);
    assert!(!r.body.contains("view=mine"), "{}", r.body);
    let r = anonymous.get("/alice/demo/merge-requests?view=mine");
    assert_eq!(r.status, 401, "{}", r.body);
    assert!(r.body.contains("Sign in to continue"), "{}", r.body);
}
