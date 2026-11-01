// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The M2 surfaces across all their entry points (RBAC-01, RBAC-02,
//! ISSUE-02, SEC-04): a private organization repository's issues, labels,
//! attachments, search hits, reference suggestions, and notifications are
//! available to a team member and to nobody else, and stop the moment the
//! team grant goes away; titles, bodies, labels, milestones, and comments
//! never render as markup.

use slath_tests::http::{Client, Resp, create_repo, create_token, multipart, submit};
use slath_tests::server::{PASSWORD, TestServer};

fn post(c: &mut Client, page: &str, action: &str, fields: &[(&str, &str)]) -> Resp {
    submit(c, page, action, fields)
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !f() {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(30),
            "timed out: {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Everything about acme/vault one person can reach, by surface.
struct Probe {
    issue_page: u16,
    issue_api: u16,
    labels_api: u16,
    attachment: u16,
    search_page_hit: bool,
    search_api_hit: bool,
    references: u16,
    inbox_hit: bool,
    notifications_api_hit: bool,
}

fn probe(c: &mut Client, token: Option<&str>, attachment: &str) -> Probe {
    let api = |c: &mut Client, path: &str| c.api("GET", path, token, None);
    let search_api = api(c, "/api/v1/search/issues?q=quokkavault");
    let notifications = api(c, "/api/v1/notifications?all=true");
    Probe {
        issue_page: c.get("/acme/vault/issues/1").status,
        issue_api: api(c, "/api/v1/repos/acme/vault/issues/1").status,
        labels_api: api(c, "/api/v1/repos/acme/vault/labels").status,
        attachment: c.get(attachment).status,
        search_page_hit: c
            .get("/-/search?q=quokkavault")
            .body
            .contains("Quokkavault plan"),
        search_api_hit: search_api.body.contains("Quokkavault plan"),
        references: api(c, "/api/v1/repos/acme/vault/references?kind=item&q=").status,
        inbox_hit: c
            .get("/-/notifications?all=true")
            .body
            .contains("Quokkavault plan"),
        notifications_api_hit: notifications.body.contains("Quokkavault plan"),
    }
}

fn assert_nothing(p: &Probe, who: &str) {
    assert_eq!(p.issue_page, 404, "{who}: issue page");
    assert_eq!(p.issue_api, 404, "{who}: issue API");
    assert_eq!(p.labels_api, 404, "{who}: labels API");
    assert_eq!(p.attachment, 404, "{who}: attachment");
    assert!(!p.search_page_hit, "{who}: search page");
    assert!(!p.search_api_hit, "{who}: search API");
    assert!(
        matches!(p.references, 401 | 404),
        "{who}: references {}",
        p.references
    );
    assert!(!p.inbox_hit, "{who}: inbox");
    assert!(!p.notifications_api_hit, "{who}: notifications API");
}

#[test]
fn conf_rbac_01_m2_surfaces_follow_team_access() {
    let s = TestServer::start_with(&[]);
    for u in ["alice", "bob", "mallory"] {
        s.create_user(u, PASSWORD, false);
    }
    let mut a = s.login("alice", PASSWORD);
    assert_eq!(
        post(
            &mut a,
            "/-/orgs/new",
            "/-/orgs",
            &[("name", "acme"), ("display_name", "")]
        )
        .status,
        303
    );
    assert_eq!(
        post(
            &mut a,
            "/acme/-/members",
            "/acme/-/members",
            &[("username", "bob"), ("role", "member")]
        )
        .status,
        303
    );
    create_repo(&mut a, "acme", "vault", "private");
    assert_eq!(
        post(
            &mut a,
            "/acme/-/teams",
            "/acme/-/teams",
            &[("name", "core"), ("description", "")]
        )
        .status,
        303
    );
    assert_eq!(
        post(
            &mut a,
            "/acme/-/teams/core",
            "/acme/-/teams/core/members",
            &[("username", "bob")]
        )
        .status,
        303
    );
    assert_eq!(
        post(
            &mut a,
            "/acme/vault/settings/access",
            "/acme/vault/settings/access/teams",
            &[("team", "core"), ("role", "read")]
        )
        .status,
        303
    );

    let mut bob = s.login("bob", PASSWORD);
    // bob watches the repository, so he is notified about new issues.
    assert_eq!(
        post(
            &mut bob,
            "/acme/vault",
            "/acme/vault/subscription",
            &[("state", "watching")]
        )
        .status,
        303
    );
    let ta = create_token(&mut a, "t", &["issues:write", "repo:write"]);
    let r = a.api(
        "POST",
        "/api/v1/repos/acme/vault/labels",
        Some(&ta),
        Some(&serde_json::json!({"name": "secret-label", "color": "aa0000"})),
    );
    assert!(r.status == 201 || r.status == 200, "{}", r.body);
    let (ct, body) = multipart(&[("file", Some("plan.png"), b"\x89PNG\r\n\x1a\n0000")]);
    let auth = format!("Bearer {ta}");
    let up = a.request(
        "POST",
        "/api/v1/repos/acme/vault/attachments",
        &[("authorization", &auth), ("cookie", "")],
        Some((&ct, body)),
    );
    assert_eq!(up.status, 201, "{}", up.body);
    let attachment = up.json()["url"].as_str().unwrap().to_owned();
    let r = a.api(
        "POST",
        "/api/v1/repos/acme/vault/issues",
        Some(&ta),
        Some(&serde_json::json!({"title": "Quokkavault plan", "body": format!("See {}", up.json()["markdown"].as_str().unwrap())})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let tb = create_token(&mut bob, "t", &["repo:read", "user:read"]);
    wait_until("bob's notification", || {
        bob.get("/-/notifications")
            .body
            .contains("Quokkavault plan")
    });

    // bob (team grant) reaches every surface.
    let p = probe(&mut bob, Some(&tb), &attachment);
    assert_eq!(p.issue_page, 200);
    assert_eq!(p.issue_api, 200);
    assert_eq!(p.labels_api, 200);
    assert_eq!(p.attachment, 200);
    assert!(p.search_page_hit && p.search_api_hit);
    assert_eq!(p.references, 200);
    assert!(p.inbox_hit && p.notifications_api_hit);

    // mallory (signed in, token) and anonymous visitors reach none.
    let mut m = s.login("mallory", PASSWORD);
    let tm = create_token(&mut m, "t", &["repo:read", "user:read"]);
    assert_nothing(&probe(&mut m, Some(&tm), &attachment), "mallory");
    assert_nothing(&probe(&mut s.client(), None, &attachment), "anonymous");

    // Deleting the team revokes everything at once, old notifications included.
    assert_eq!(
        post(
            &mut a,
            "/acme/-/teams/core",
            "/acme/-/teams/core/delete",
            &[]
        )
        .status,
        303
    );
    assert_nothing(
        &probe(&mut bob, Some(&tb), &attachment),
        "bob after the team was deleted",
    );
    // And new activity doesn't reach him either.
    let r = a.api(
        "POST",
        "/api/v1/repos/acme/vault/issues/1/comments",
        Some(&ta),
        Some(&serde_json::json!({"body": "more"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert_eq!(
        s.db.query_one("SELECT n.unread::text FROM notifications n JOIN users u ON u.id = n.user_id JOIN namespaces x ON x.id = u.namespace_id WHERE x.name = 'bob'"),
        "false",
        "bob read it before; the new comment didn't bump it"
    );
}

const PAYLOADS: &[&str] = &[
    "<img src=x onerror=alert(1)>",
    "\"><svg/onload=alert(1)>",
    "</script><script>alert(1)</script>",
    "hx-get=\"/-/logout\" hx-trigger=\"load\"",
    "{{7*7}}",
    "[x](javascript:alert(1))",
    "<div hx-get=\"/-/logout\" hx-trigger=\"load\">x</div>",
];

const UNESCAPED: &[&str] = &[
    "<img src=x onerror",
    "<svg/onload",
    "<script>alert",
    "href=\"javascript:",
    "=49",
];

fn assert_inert(body: &str, what: &str) {
    for bad in UNESCAPED {
        assert!(!body.contains(bad), "{what}: found {bad:?}");
    }
    // Text and attribute values may mention htmx; no tag may carry an htmx
    // or event-handler attribute.
    for tag in body.split('<').skip(1) {
        let tag = tag.split('>').next().unwrap_or("");
        for name in attribute_names(tag) {
            assert!(
                !(name.starts_with("hx-") && name != "hx-ignore") && !name.starts_with("on"),
                "{what}: attribute {name} in <{tag}>"
            );
        }
    }
}

/// The attribute names of a tag's inside (`a href="x" class=y`), skipping
/// quoted values.
fn attribute_names(tag: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut chars = tag.chars().peekable();
    // Skip the element name.
    while chars.next_if(|c| !c.is_whitespace()).is_some() {}
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut name = String::new();
        while let Some(c) = chars.next_if(|c| !c.is_whitespace() && *c != '=') {
            name.push(c);
        }
        if name.is_empty() && chars.peek().is_none() {
            break;
        }
        if !name.is_empty() {
            names.push(name.to_ascii_lowercase());
        }
        if chars.next_if_eq(&'=').is_some() {
            match chars.peek().copied() {
                Some(q @ ('"' | '\'')) => {
                    chars.next();
                    while chars.next().is_some_and(|c| c != q) {}
                }
                _ => while chars.next_if(|c| !c.is_whitespace()).is_some() {},
            }
        } else if name.is_empty() {
            chars.next();
        }
    }
    names
}

/// Stored XSS through issue titles, descriptions, comments, labels, and
/// milestones (Gitea CVE-2024-6886 and CVE-2025-68942-style name XSS):
/// every page and API answer that shows them escapes or sanitizes them.
#[test]
fn conf_sec_04_xss_issue_titles_bodies_labels() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PASSWORD, false);
    let mut a = s.login("alice", PASSWORD);
    create_repo(&mut a, "alice", "pub", "public");
    let t = create_token(&mut a, "t", &["issues:write", "repo:write"]);
    for (i, p) in PAYLOADS.iter().enumerate() {
        let short: String = p.chars().take(40).collect();
        let _ = a.api(
            "POST",
            "/api/v1/repos/alice/pub/labels",
            Some(&t),
            Some(&serde_json::json!({
                "name": format!("{i}{short}"), "color": "00aa00", "description": p,
            })),
        );
        let _ = a.api(
            "POST",
            "/api/v1/repos/alice/pub/milestones",
            Some(&t),
            Some(&serde_json::json!({
                "title": format!("{i}{short}"), "description": p,
            })),
        );
        let r = a.api(
            "POST",
            "/api/v1/repos/alice/pub/issues",
            Some(&t),
            Some(&serde_json::json!({
                "title": format!("{p} title"), "body": format!("{p}\n\n~\"{i}{short}\" #1 @alice"),
            })),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        let r = a.api(
            "POST",
            &format!("/api/v1/repos/alice/pub/issues/{n}/comments"),
            Some(&t),
            Some(&serde_json::json!({"body": p})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
    }
    let mut anon = s.client();
    for path in [
        "/alice/pub/issues",
        "/alice/pub/issues?state=all",
        "/alice/pub/labels",
        "/alice/pub/milestones",
        "/-/search?q=title",
        "/-/notifications?all=true",
    ] {
        assert_inert(&a.get(path).body, path);
        assert_inert(&anon.get(path).body, path);
    }
    for n in 1..=PAYLOADS.len() {
        let path = format!("/alice/pub/issues/{n}");
        assert_inert(&anon.get(&path).body, &path);
        assert_inert(&a.get(&path).body, &path);
        let api = a.api(
            "GET",
            &format!("/api/v1/repos/alice/pub/issues/{n}"),
            Some(&t),
            None,
        );
        let html = api.json()["body_html"].as_str().unwrap_or("").to_owned();
        assert_inert(&html, "body_html");
    }
}
