// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Issues (ISSUE-01, ISSUE-02): permissions per role, timeline events,
//! numbering, references that reveal nothing to those who can't read the
//! target, and cross-repository mentions shown only to readers of the
//! source.

use serde_json::json;
use slath_tests::http::{Client, create_repo, create_token, submit};
use slath_tests::server::{PASSWORD, TestServer};

fn post(
    c: &mut Client,
    page: &str,
    action: &str,
    fields: &[(&str, &str)],
) -> slath_tests::http::Resp {
    submit(c, page, action, fields)
}

fn new_issue(c: &mut Client, repo: &str, title: &str, body: &str) -> String {
    let r = post(
        c,
        &format!("/{repo}/issues/new"),
        &format!("/{repo}/issues"),
        &[("title", title), ("body", body)],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    r.header("location").unwrap().to_owned()
}

fn add_collaborator(owner: &mut Client, repo: &str, user: &str, role: &str) {
    let page = format!("/{repo}/settings/access");
    assert_eq!(
        post(owner, &page, &page, &[("username", user), ("role", role)]).status,
        303
    );
}

/// alice owns alice/pub (public) and alice/priv (private); bob, carol, and
/// dave exist.
fn setup() -> (TestServer, Client) {
    let s = TestServer::start_with(&[]);
    for u in ["alice", "bob", "carol", "dave"] {
        s.create_user(u, PASSWORD, false);
    }
    let mut a = s.login("alice", PASSWORD);
    create_repo(&mut a, "alice", "pub", "public");
    create_repo(&mut a, "alice", "priv", "private");
    (s, a)
}

#[test]
fn conf_issue_01_lifecycle_and_permissions_per_role() {
    let (s, mut a) = setup();
    let mut bob = s.login("bob", PASSWORD); // read via public visibility
    let mut carol = s.login("carol", PASSWORD);
    add_collaborator(&mut a, "alice/pub", "carol", "write");
    let mut dave = s.login("dave", PASSWORD);
    add_collaborator(&mut a, "alice/pub", "dave", "maintain");

    // Any signed-in reader opens issues; numbers count up per repository.
    let one = new_issue(
        &mut bob,
        "alice/pub",
        "<script>alert(1)</script> broken",
        "Steps:\n\n1. run",
    );
    assert_eq!(one, "/alice/pub/issues/1");
    let two = new_issue(&mut a, "alice/pub", "Second", "");
    assert_eq!(two, "/alice/pub/issues/2");
    assert_eq!(
        new_issue(&mut a, "alice/priv", "Private one", ""),
        "/alice/priv/issues/1"
    );
    let page = s.client().get(&one);
    assert_eq!(page.status, 200, "anonymous readers see public issues");
    assert!(
        page.body.contains("&#60;script&#62;alert(1)")
            || page.body.contains("&lt;script&gt;alert(1)")
    );
    assert!(!page.body.contains("<script>alert(1)"));
    assert!(page.body.contains("Sign in</a> to comment"));
    assert_eq!(
        s.client()
            .post_form(&format!("{one}/comments"), &[("body", "x")])
            .status,
        403,
        "no anonymous writes"
    );

    // Authors edit and close their own; others need triage.
    assert_eq!(
        post(
            &mut bob,
            &two,
            &format!("{two}/state"),
            &[("state", "closed")]
        )
        .status,
        403
    );
    assert_eq!(
        post(
            &mut bob,
            &one,
            &format!("{one}/edit"),
            &[("title", "Broken build"), ("body", "edited")]
        )
        .status,
        303
    );
    assert_eq!(
        post(
            &mut bob,
            &one,
            &format!("{one}/state"),
            &[("state", "closed"), ("reason", "not_planned")]
        )
        .status,
        303
    );
    assert_eq!(
        post(
            &mut carol,
            &one,
            &format!("{one}/state"),
            &[("state", "open")]
        )
        .status,
        303,
        "triage reopens"
    );
    let page = a.get(&one);
    assert!(page.body.contains("changed the title from"));
    assert!(page.body.contains("closed this as not planned"));
    assert!(page.body.contains("reopened this"));

    // Comments: authors edit their own; maintainers hide and delete.
    let r = post(
        &mut bob,
        &one,
        &format!("{one}/comments"),
        &[("body", "Me too")],
    );
    assert_eq!(r.status, 303);
    let cid = r
        .header("location")
        .unwrap()
        .split("#comment-")
        .nth(1)
        .unwrap()
        .to_owned();
    assert_eq!(
        post(
            &mut carol,
            &one,
            &format!("{one}/comments/{cid}/edit"),
            &[("body", "x")]
        )
        .status,
        403
    );
    assert_eq!(
        post(&mut carol, &one, &format!("{one}/comments/{cid}/hide"), &[]).status,
        403,
        "write can't moderate"
    );
    assert_eq!(
        post(&mut dave, &one, &format!("{one}/comments/{cid}/hide"), &[]).status,
        303
    );
    assert!(
        !s.client().get(&one).body.contains("Me too"),
        "hidden from others"
    );
    assert!(
        bob.get(&one).body.contains("Me too"),
        "the author still sees it"
    );
    assert_eq!(
        post(&mut bob, &one, &format!("{one}/comments/{cid}/delete"), &[]).status,
        303
    );
    assert!(bob.get(&one).body.contains("This comment was deleted."));

    // Private repositories don't exist for others.
    assert_eq!(bob.get("/alice/priv/issues").status, 404);
    assert_eq!(bob.get("/alice/priv/issues/1").status, 404);
    assert_eq!(
        s.client().get("/alice/priv/1").status,
        303,
        "numbers redirect (then 404)"
    );
    assert_eq!(bob.get("/alice/priv/issues/1/edit").status, 404);
    // Disabled issues are gone.
    s.db.execute("UPDATE repositories SET issues_enabled = false");
    assert_eq!(a.get("/alice/pub/issues").status, 404);
    assert!(!a.get("/alice/pub").body.contains("/alice/pub/issues"));
}

#[test]
fn conf_issue_02_references_reveal_nothing_without_access() {
    let (s, mut a) = setup();
    let secret = new_issue(&mut a, "alice/priv", "Secret plans", "");
    assert_eq!(secret, "/alice/priv/issues/1");
    let mut bob = s.login("bob", PASSWORD);
    // bob refers to the private issue from a public one.
    let public = new_issue(
        &mut bob,
        "alice/pub",
        "Public",
        "See alice/priv#1 and @carol and @nobody-here",
    );
    let bob_view = bob.get(&public).body;
    assert!(!bob_view.contains("Secret plans"));
    assert!(!bob_view.contains("href=\"/alice/priv/issues/1\""));
    assert!(bob_view.contains("alice/priv#1"), "plain text");
    assert!(bob_view.contains("href=\"/carol\""));
    assert!(!bob_view.contains("href=\"/nobody-here\""));
    // alice can read both: a link with the title.
    let alice_view = a.get(&public).body;
    assert!(alice_view.contains("href=\"/alice/priv/issues/1\""));
    assert!(alice_view.contains("title=\"Secret plans (open)\""));
    // bob couldn't read the target, so nothing was recorded there.
    assert!(!a.get(&secret).body.contains("mentioned this in"));

    // alice refers to the public issue from the private one: the event is
    // recorded in the public issue but shown only to readers of the source.
    assert_eq!(
        post(
            &mut a,
            &secret,
            &format!("{secret}/comments"),
            &[("body", "Related: alice/pub#1")]
        )
        .status,
        303
    );
    assert!(a.get(&public).body.contains("mentioned this in"));
    assert!(!bob.get(&public).body.contains("mentioned this in"));
    let anon = s.client().get(&public).body;
    assert!(!anon.contains("href=\"/alice/priv") && !anon.contains("mentioned this in"));
}

#[test]
fn issue_api_follows_the_same_rules() {
    let (s, mut a) = setup();
    let token = create_token(&mut a, "issues", &["issues:write", "repo:read"]);
    let read_only = create_token(&mut a, "ro", &["repo:read"]);
    let mut c = s.client();
    let r = c.api(
        "POST",
        "/api/v1/repos/alice/priv/issues",
        Some(&token),
        Some(&json!({"title": "Via API", "body": "#1"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["number"], 1);
    let r = c.api(
        "POST",
        "/api/v1/repos/alice/priv/issues",
        Some(&read_only),
        Some(&json!({"title": "x"})),
    );
    assert_eq!(r.status, 403, "issues:write is needed");
    let r = c.api(
        "POST",
        "/api/v1/repos/alice/priv/issues",
        Some(&token),
        Some(&json!({"title": ""})),
    );
    assert_eq!(r.status, 422);
    let r = c.api(
        "PATCH",
        "/api/v1/repos/alice/priv/issues/1",
        Some(&token),
        Some(&json!({"state": "closed", "state_reason": "duplicate"})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["state_reason"], "duplicate");
    let r = c.api(
        "POST",
        "/api/v1/repos/alice/priv/issues/1/comments",
        Some(&token),
        Some(&json!({"body": "done"})),
    );
    assert_eq!(r.status, 201);
    let list = c.api(
        "GET",
        "/api/v1/repos/alice/priv/issues?state=all",
        Some(&read_only),
        None,
    );
    assert_eq!(list.json()["items"].as_array().unwrap().len(), 1);
    // Anonymous and other users: not found.
    assert_eq!(
        c.api("GET", "/api/v1/repos/alice/priv/issues/1", None, None)
            .status,
        404
    );
    let mut bob = s.login("bob", PASSWORD);
    let bob_token = create_token(&mut bob, "t", &["issues:write", "repo:read"]);
    assert_eq!(
        c.api(
            "GET",
            "/api/v1/repos/alice/priv/issues/1/comments",
            Some(&bob_token),
            None
        )
        .status,
        404
    );
    assert_eq!(
        c.api(
            "POST",
            "/api/v1/repos/alice/priv/issues",
            Some(&bob_token),
            Some(&json!({"title": "x"}))
        )
        .status,
        404
    );
}

/// Labels and milestones need the maintain role; triage needs write;
/// assignees must be able to read the repository; filters combine.
#[test]
fn labels_milestones_and_assignees_via_api() {
    let (s, mut a) = setup();
    add_collaborator(&mut a, "alice/priv", "carol", "write");
    let token = create_token(&mut a, "issues", &["issues:write", "repo:read"]);
    let mut carol = s.login("carol", PASSWORD);
    let carol_token = create_token(&mut carol, "t", &["issues:write", "repo:read"]);
    let mut c = s.client();
    let api = |c: &mut Client, m: &str, path: &str, t: &str, body: Option<serde_json::Value>| {
        c.api(
            m,
            &format!("/api/v1/repos/alice/priv{path}"),
            Some(t),
            body.as_ref(),
        )
    };
    let r = api(
        &mut c,
        "POST",
        "/labels",
        &token,
        Some(json!({"name": "crash", "color": "#D73A4A"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["color"], "d73a4a");
    let r = api(
        &mut c,
        "POST",
        "/labels",
        &carol_token,
        Some(json!({"name": "ui"})),
    );
    assert_eq!(r.status, 403, "write doesn't manage labels");
    let r = api(
        &mut c,
        "POST",
        "/labels",
        &token,
        Some(json!({"name": "CRASH"})),
    );
    assert_eq!(r.status, 422, "names are unique regardless of case");
    let r = api(
        &mut c,
        "POST",
        "/milestones",
        &token,
        Some(json!({"title": "v1", "due_on": "2026-12-31"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let ms = r.json()["id"].as_str().unwrap().to_owned();

    for t in ["One", "Two"] {
        assert_eq!(
            api(&mut c, "POST", "/issues", &token, Some(json!({"title": t}))).status,
            201
        );
    }
    // carol (write) triages.
    let r = api(
        &mut c,
        "PATCH",
        "/issues/1",
        &carol_token,
        Some(json!({"labels": ["crash"], "milestone": "v1", "assignees": ["carol"]})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let j = r.json();
    assert_eq!(j["labels"], json!(["crash"]));
    assert_eq!(j["milestone"], "v1");
    assert_eq!(j["assignees"], json!(["carol"]));
    // bob can't read alice/priv, so he can't be assigned.
    let r = api(
        &mut c,
        "PATCH",
        "/issues/2",
        &token,
        Some(json!({"assignees": ["bob"]})),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        api(
            &mut c,
            "PATCH",
            "/issues/2",
            &token,
            Some(json!({"labels": ["crash"]}))
        )
        .status,
        200
    );

    let list = |c: &mut Client, q: &str| -> Vec<i64> {
        let r = c.api(
            "GET",
            &format!("/api/v1/repos/alice/priv/issues?{q}"),
            Some(&token),
            None,
        );
        assert_eq!(r.status, 200, "{}", r.body);
        let mut n: Vec<i64> = r.json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["number"].as_i64().unwrap())
            .collect();
        n.sort_unstable();
        n
    };
    assert_eq!(list(&mut c, "labels=crash"), [1, 2]);
    assert_eq!(list(&mut c, "labels=crash&milestone=v1"), [1]);
    assert_eq!(list(&mut c, "labels=crash&assignee=carol"), [1]);
    assert_eq!(
        list(&mut c, "labels=crash&assignee=alice"),
        Vec::<i64>::new()
    );

    // Closing the milestone and deleting the label.
    let r = api(
        &mut c,
        "PATCH",
        &format!("/milestones/{ms}"),
        &token,
        Some(json!({"state": "closed"})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["state"], "closed");
    assert_eq!(
        api(&mut c, "DELETE", "/labels/crash", &carol_token, None).status,
        403
    );
    assert_eq!(
        api(&mut c, "DELETE", "/labels/crash", &token, None).status,
        204
    );
    assert_eq!(list(&mut c, "labels=crash"), Vec::<i64>::new());

    // Those who can't read the repository see nothing.
    let mut bob = s.login("bob", PASSWORD);
    let bob_token = create_token(&mut bob, "t", &["issues:write", "repo:read"]);
    assert_eq!(api(&mut c, "GET", "/labels", &bob_token, None).status, 404);
    assert_eq!(
        api(
            &mut c,
            "POST",
            "/milestones",
            &bob_token,
            Some(json!({"title": "x"}))
        )
        .status,
        404
    );
    assert_eq!(s.client().get("/alice/priv/labels").status, 404);
}
