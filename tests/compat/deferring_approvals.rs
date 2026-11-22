// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Approvals that defer (REVIEW-19): an approval that leaves the decision
//! to a user or a team never counts toward requirements, shows as the
//! reviewer's claim, and asks whom it names for a review.

use serde_json::{Value, json};
use slath_tests::http::{Client, create_repo, create_token, submit};
use slath_tests::server::{GitClient, PASSWORD, TestRepo, TestServer};

const APP: TestRepo = TestRepo {
    namespace: "acme",
    name: "app",
};

struct Setup {
    s: TestServer,
    alice: String,
    bob: String,
    n: i64,
}

impl Setup {
    fn api(&self, token: &str, method: &str, path: &str, body: Option<Value>) -> Value {
        let r = self.s.client().api(
            method,
            &format!("/api/v1/repos/acme/app{path}"),
            Some(token),
            body.as_ref(),
        );
        assert!(r.status < 300, "{method} {path}: {} {}", r.status, r.body);
        if r.body.is_empty() {
            Value::Null
        } else {
            r.json()
        }
    }

    fn review(&self, token: &str, body: Value) -> slath_tests::http::Resp {
        self.s.client().api(
            "POST",
            &format!("/api/v1/repos/acme/app/merge-requests/{}/reviews", self.n),
            Some(token),
            Some(&body),
        )
    }

    fn requirement(&self, key: &str) -> (String, String) {
        let m = self.api(
            &self.alice,
            "GET",
            &format!("/merge-requests/{}/mergeability", self.n),
            None,
        );
        let r = m["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .unwrap_or_else(|| panic!("no {key} in {m}"));
        (
            r["state"].as_str().unwrap().to_owned(),
            r["reason"].as_str().unwrap().to_owned(),
        )
    }

    /// Whether the merge request is in `user`'s review requests inbox.
    fn asked(&self, user: &str) -> bool {
        self.s
            .login(user, PASSWORD)
            .get("/-/review-requests")
            .body
            .contains("Add a")
    }
}

fn post(c: &mut Client, page: &str, action: &str, fields: &[(&str, &str)]) {
    let r = submit(c, page, action, fields);
    assert_eq!(r.status, 303, "{action}: {}", r.body);
}

/// The private repository acme/app with one required approval; bob and
/// carol write, dave reads, eve has no access, and the team acme/leads
/// has carol. Alice opens a merge request.
fn setup() -> Setup {
    let s = TestServer::start();
    for u in ["bob", "carol", "dave", "eve"] {
        s.create_user(u, PASSWORD, false);
    }
    let mut a = s.login("alice", PASSWORD);
    post(
        &mut a,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", "acme"), ("display_name", "")],
    );
    assert_eq!(create_repo(&mut a, "acme", "app", "private").status, 303);
    for (who, role) in [("bob", "member"), ("carol", "member"), ("dave", "member")] {
        post(
            &mut a,
            "/acme/-/members",
            "/acme/-/members",
            &[("username", who), ("role", role)],
        );
    }
    post(
        &mut a,
        "/acme/-/teams",
        "/acme/-/teams",
        &[("name", "leads"), ("description", "")],
    );
    post(
        &mut a,
        "/acme/-/teams/leads",
        "/acme/-/teams/leads/members",
        &[("username", "carol")],
    );
    let alice = create_token(&mut a, "alice", &["repo:write", "mrs:write"]);
    let token = |u: &str| create_token(&mut s.login(u, PASSWORD), u, &["repo:write", "mrs:write"]);
    let mut u = Setup {
        bob: token("bob"),
        alice,
        n: 0,
        s,
    };
    for (who, role) in [("bob", "write"), ("carol", "write"), ("dave", "read")] {
        u.api(
            &u.alice,
            "PUT",
            &format!("/collaborators/{who}"),
            Some(json!({"role": role})),
        );
    }
    u.api(
        &u.alice,
        "POST",
        "/protections",
        Some(json!({"pattern": "main", "required_approvals": 1, "block_force_push": false})),
    );
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let url = u.s.url(&APP);
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "x\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "-c", "topic"]);
    std::fs::write(w.join("a.txt"), "a\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "Add a"]);
    git.ok(w, &["push", "-q", &url, "topic"]);
    let m = u.api(
        &u.alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Add a"})),
    );
    u.n = m["number"].as_i64().unwrap();
    let start = std::time::Instant::now();
    while u.api(&u.alice, "GET", &format!("/merge-requests/{}", u.n), None)["revisions"] != 1 {
        assert!(start.elapsed().as_secs() < 30, "no revision\n{}", u.s.log());
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    u
}

#[test]
fn conf_review_19_a_deferring_approval_never_counts_and_requests_the_named() {
    let u = setup();
    let carol = create_token(
        &mut u.s.login("carol", PASSWORD),
        "carol",
        &["repo:write", "mrs:write"],
    );

    // Bob first asks for changes, then approves and leaves it to carol:
    // his request for changes is withdrawn, the approval counts for
    // nothing, and carol is asked.
    assert_eq!(
        u.review(&u.bob, json!({"verdict": "request_changes", "body": "Hm."}))
            .status,
        201
    );
    let r = u.review(&u.bob, json!({"verdict": "approve", "defer_to": "@carol"}));
    assert_eq!(r.status, 201, "{}", r.body);
    let v = r.json();
    assert_eq!(
        (&v["defer_to"], &v["deferred"], &v["counts"], &v["stale"]),
        (&json!("carol"), &json!(true), &json!(false), &json!(false))
    );
    assert_eq!(
        u.requirement("approvals"),
        (
            "unmet".into(),
            "0 of 1 required approvals. bob approved and left the decision to carol.".into()
        )
    );
    assert_eq!(u.requirement("changes_requested").0, "met");
    assert!(u.asked("carol"));

    // The page shows it as bob's claim.
    let page =
        u.s.login("alice", PASSWORD)
            .get(&format!("/acme/app/merge-requests/{}", u.n))
            .body;
    assert!(
        page.contains("approved revision 1 and left the decision to carol"),
        "{page}"
    );
    assert!(page.contains("approved, left to carol"), "{page}");

    // Carol decides.
    assert_eq!(u.review(&carol, json!({"verdict": "approve"})).status, 201);
    assert_eq!(u.requirement("approvals").0, "met");
    assert!(!u.asked("carol"));

    // Leaving it to a team asks the team's members.
    let r = u.review(
        &u.bob,
        json!({"verdict": "approve", "defer_to": "acme/leads"}),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["defer_to"], "acme/leads");
    assert!(u.asked("carol"));
    assert!(!u.asked("dave"));
}

#[test]
fn conf_review_19_whom_an_approval_may_defer_to() {
    let u = setup();
    for (body, code) in [
        // Only approvals defer.
        (
            json!({"verdict": "request_changes", "body": "x", "defer_to": "carol"}),
            "verdict",
        ),
        (
            json!({"verdict": "comment", "body": "x", "defer_to": "carol"}),
            "verdict",
        ),
        // Not oneself, not the author, only readers, only teams of the
        // repository's organization.
        (json!({"verdict": "approve", "defer_to": "bob"}), "self"),
        (json!({"verdict": "approve", "defer_to": "alice"}), "author"),
        (json!({"verdict": "approve", "defer_to": "eve"}), "unknown"),
        (
            json!({"verdict": "approve", "defer_to": "nobody"}),
            "unknown",
        ),
        (
            json!({"verdict": "approve", "defer_to": "acme/none"}),
            "unknown",
        ),
        (
            json!({"verdict": "approve", "defer_to": "other/leads"}),
            "unknown",
        ),
    ] {
        let r = u.review(&u.bob, body.clone());
        assert_eq!(r.status, 422, "{body}: {}", r.body);
        let p = r.json();
        let e = &p["errors"][0];
        assert_eq!(
            (&e["field"], &e["code"]),
            (&json!("defer_to"), &json!(code)),
            "{body}: {p}"
        );
    }
    assert!(!u.asked("carol"));
    // A reader may be named, though their own approval wouldn't count.
    let r = u.review(&u.bob, json!({"verdict": "approve", "defer_to": "dave"}));
    assert_eq!(r.status, 201, "{}", r.body);
    assert!(u.asked("dave"));
    assert_eq!(u.requirement("approvals").0, "unmet");
}

#[test]
fn conf_review_19_the_review_form_leaves_the_decision_to_someone() {
    let u = setup();
    let mut bob = u.s.login("bob", PASSWORD);
    let page = format!("/acme/app/merge-requests/{}", u.n);
    let action = format!("{page}/reviews");
    let mut send = |fields: &[(&str, &str)]| submit(&mut bob, &page, &action, fields);
    // The other options ignore "Who decides", which is hidden for them.
    let r = send(&[
        ("verdict", "comment"),
        ("body", "Hm."),
        ("defer_to", "carol"),
    ]);
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(!u.asked("carol"));
    // The option needs a name; an error keeps the option, the name, and
    // the comment, and names the field.
    let r = send(&[
        ("verdict", "defer"),
        ("body", "Over to you."),
        ("defer_to", " "),
    ]);
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.body.contains("Name a user or a team who decides."),
        "{}",
        r.body
    );
    let r = send(&[
        ("verdict", "defer"),
        ("body", "Over to you."),
        ("defer_to", "eve"),
    ]);
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("id=\"review-defer-error\""), "{}", r.body);
    assert!(
        r.body
            .contains(r#"id="verdict-defer" type="radio" name="verdict" value="defer" checked"#),
        "{}",
        r.body
    );
    assert!(r.body.contains(r#"value="eve""#), "{}", r.body);
    assert!(r.body.contains(">Over to you.</textarea>"), "{}", r.body);
    // Chosen with a name, it records a deferring approval.
    let r = send(&[("verdict", "defer"), ("body", ""), ("defer_to", "carol")]);
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(u.asked("carol"));
    assert_eq!(u.requirement("approvals").0, "unmet");
}

#[test]
fn conf_review_19_who_decides_suggests_people_and_teams() {
    let u = setup();
    let mut bob = u.s.login("bob", PASSWORD);
    // The page asks for people and the organization's teams in one field.
    let page = bob.get(&format!("/acme/app/merge-requests/{}", u.n)).body;
    assert!(
        page.contains(r#"data-suggest-users="acme/app" data-suggest-single data-suggest-teams"#),
        "{page}"
    );
    let values = |c: &mut Client, kind: &str, q: &str| -> Vec<String> {
        let r = c.get(&format!("/-/ui/references?repo=acme/app&kind={kind}&q={q}"));
        assert_eq!(r.status, 200, "{}", r.body);
        r.json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["value"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(values(&mut bob, "user", "ca"), vec!["@carol".to_owned()]);
    // Teams match by their name, as org members see them.
    assert_eq!(
        values(&mut bob, "team", "le"),
        vec!["@acme/leads".to_owned()]
    );
    assert!(values(&mut bob, "team", "x").is_empty());
    // Someone who can't read the repository gets nothing.
    let mut eve = u.s.login("eve", PASSWORD);
    assert_eq!(
        eve.get("/-/ui/references?repo=acme/app&kind=team&q=")
            .status,
        404
    );
}
