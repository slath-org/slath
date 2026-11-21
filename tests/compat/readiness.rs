// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The readiness snapshot behind merge request lists (REVIEW-16): each row
//! shows what the merge request's readiness card shows, after every kind of
//! change that affects it; the snapshot is rebuildable, `fsck` finds and
//! repairs a wrong one, and it never allows a merge.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::server::PASSWORD;

use crate::change_identity::{Ctx, commit_file, seed};

/// The only open merge request's icon in the list: tone and tooltip.
fn listed(c: &Ctx) -> (String, String) {
    let body =
        c.s.login("alice", PASSWORD)
            .get("/alice/demo/merge-requests")
            .body;
    let at = body
        .find(r#"<span class="item-checks tone-"#)
        .unwrap_or_else(|| panic!("no readiness icon: {body}"));
    let rest = &body[at + r#"<span class="item-checks tone-"#.len()..];
    let (tone, rest) = rest.split_once('"').unwrap();
    let tip = rest
        .split_once(r#"data-tip=""#)
        .and_then(|(_, r)| r.split_once('"'))
        .map(|(t, _)| t.replace("&#x27;", "'"))
        .unwrap();
    (tone.to_owned(), tip)
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

    // Protection of the target (all its merge requests).
    protect(&c, json!({"pattern": "main", "required_approvals": 1}));
    shows(&c, n, "waiting", "Approval required");
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
    for (state, tone, headline) in [
        ("failure", "blocking", "Checks failing"),
        ("success", "done", "Ready to merge"),
    ] {
        let r = c.s.client().api(
            "POST",
            &format!("/api/v1/repos/alice/demo/statuses/{head}"),
            Some(&ci),
            Some(&json!({"context": "ci", "state": state})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        shows(&c, n, tone, headline);
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
