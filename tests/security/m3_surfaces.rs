// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The M3 surfaces against the authorization matrix (RBAC-01, RBAC-02,
//! REVIEW-04, REVIEW-06): every merge request action, commit statuses,
//! webhooks, and forks, through the API for every role, visibility, and
//! repository state, and for every token scope. Allowed means the request
//! got past authorization (a stale head still answers 409); denied means
//! 404 where the repository is invisible and 403 where it's visible. An
//! approval is allowed when it counts: anyone who can review may submit
//! one, but only writers' approvals count. Write scopes don't include
//! reading, so each scope is probed together with `repo:read`.
//!
//! And stored XSS through the content M3 shows (SEC-04): commit messages,
//! ref names, file paths in diffs, suggestion blocks, merge request
//! titles, descriptions, review comments, and statuses.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::http::{Client, create_token, submit};
use slath_tests::server::{GitClient, PASSWORD, TestRepo, TestServer};

use crate::m2_surfaces::assert_inert;

const PROJ: TestRepo = TestRepo {
    namespace: "alice",
    name: "proj",
};

/// (name, role as a collaborator).
const PEOPLE: &[(&str, &str)] = &[
    ("stranger", "none"),
    ("reader", "read"),
    ("writer", "write"),
    ("maintainer", "maintain"),
    ("manager", "admin"),
];

const ZERO: &str = "0000000000000000000000000000000000000000";

/// The matrix's verdict for one cell, if it has that cell.
fn expected(matrix: &str, cell: &str) -> Option<String> {
    let prefix = format!("{cell},");
    matrix
        .lines()
        .find(|l| l.starts_with(&prefix))
        .map(|l| l[prefix.len()..].to_owned())
}

/// Whether a status agrees with a verdict. The API tells anonymous
/// writers to authenticate, whether or not the repository exists, which
/// reveals nothing.
fn agrees(status: u16, verdict: &str, anon: bool) -> bool {
    match verdict {
        "allow" => !matches!(status, 401 | 403 | 404),
        "deny_hidden" => status == 404 || (anon && status == 401),
        "deny_visible" => status == 403 || (anon && status == 401),
        _ => false,
    }
}

struct Fixture {
    head: String,
    suggestion: String,
}

/// One request that exercises `action`. Each allowed request is harmless:
/// heads are stale, the merge request is a draft, names are unique.
fn request(action: &str, f: &Fixture, who: &str, n: usize) -> (&'static str, String, Value) {
    let mr = "/api/v1/repos/alice/proj/merge-requests/1";
    match action {
        "mr.create" => (
            "POST",
            "/api/v1/repos/alice/proj/merge-requests".into(),
            json!({"source_branch": "topic", "title": "again"}),
        ),
        "mr.comment" => ("POST", format!("{mr}/comments"), json!({"body": "hi"})),
        "mr.review" => (
            "POST",
            format!("{mr}/reviews"),
            json!({"verdict": "comment", "body": "looked"}),
        ),
        "mr.approve" => (
            "POST",
            format!("{mr}/reviews"),
            json!({"verdict": "approve"}),
        ),
        "mr.suggest" => (
            "POST",
            format!("{mr}/threads"),
            json!({"body": "```suggestion\nT\n```", "path": "topic.txt", "side": "new",
                   "line_start": 1, "line_end": 1}),
        ),
        "mr.apply_suggestion" => (
            "POST",
            format!("{mr}/suggestions/apply"),
            json!({"comments": [f.suggestion], "expected_head_oid": ZERO}),
        ),
        "mr.triage" => ("PUT", format!("{mr}/reviewers"), json!({"reviewers": []})),
        "mr.update_branch" => (
            "POST",
            format!("{mr}/update-branch"),
            json!({"method": "merge", "expected_head_oid": ZERO}),
        ),
        "mr.merge" => (
            "POST",
            format!("{mr}/merge"),
            json!({"expected_head_oid": ZERO}),
        ),
        "mr.auto_merge" => ("PUT", format!("{mr}/auto-merge"), json!({})),
        "status.write" => (
            "POST",
            format!("/api/v1/repos/alice/proj/statuses/{}", f.head),
            json!({"state": "success", "context": format!("probe/{who}")}),
        ),
        "webhooks.manage" => (
            "GET",
            "/api/v1/repos/alice/proj/webhooks".into(),
            Value::Null,
        ),
        "repo.fork" => (
            "POST",
            "/api/v1/repos/alice/proj/forks".into(),
            json!({"namespace": who, "name": format!("fork-{n}")}),
        ),
        _ => unreachable!("{action}"),
    }
}

const ACTIONS: &[&str] = &[
    "mr.create",
    "mr.comment",
    "mr.review",
    "mr.approve",
    "mr.suggest",
    "mr.apply_suggestion",
    "mr.triage",
    "mr.update_branch",
    "mr.merge",
    "mr.auto_merge",
    "status.write",
    "webhooks.manage",
    "repo.fork",
];

const SCOPES: &[&str] = &[
    "repo:read",
    "repo:write",
    "issues:write",
    "mrs:write",
    "statuses:write",
    "user:read",
    "user:write",
];

/// The request's status. An accepted approval that doesn't count (`who`
/// isn't among the counting approvers) is a denial (403).
fn call(
    c: &mut Client,
    token: Option<&str>,
    req: &(&str, String, Value),
    who: &str,
    owner: &str,
) -> u16 {
    let body = (!req.2.is_null()).then_some(&req.2);
    let r = c.api(req.0, &req.1, token, body);
    if r.status < 300 && req.2["verdict"] == "approve" {
        let m = c
            .api(
                "GET",
                "/api/v1/repos/alice/proj/merge-requests/1/mergeability",
                Some(owner),
                None,
            )
            .json();
        let counting = m["coverage"]["approvals"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|a| a["reviewer"] == who);
        if !counting {
            return 403;
        }
    }
    r.status
}

/// `main` and `topic` on the server, draft merge request !1 from `topic`
/// with a suggestion by alice.
fn setup(s: &TestServer, alice_token: &str, dir: &Path) -> Fixture {
    let git = GitClient::new();
    let w = dir.join("w");
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "proj\n").unwrap();
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "init"]);
    git.ok(&w, &["switch", "-q", "-c", "topic"]);
    std::fs::write(w.join("topic.txt"), "t\n").unwrap();
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "Topic"]);
    git.ok(&w, &["push", "-q", &s.url(&PROJ), "main", "topic"]);
    let head = git.ok(&w, &["rev-parse", "HEAD"]).trim().to_owned();
    let mut c = s.client();
    let r = c.api(
        "POST",
        "/api/v1/repos/alice/proj/merge-requests",
        Some(alice_token),
        Some(&json!({"source_branch": "topic", "title": "Topic", "draft": true})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let start = std::time::Instant::now();
    while c
        .api(
            "GET",
            "/api/v1/repos/alice/proj/merge-requests/1",
            Some(alice_token),
            None,
        )
        .json()["revisions"]
        != 1
    {
        assert!(start.elapsed().as_secs() < 30, "no first revision");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let r = c.api(
        "POST",
        "/api/v1/repos/alice/proj/merge-requests/1/threads",
        Some(alice_token),
        Some(
            &json!({"body": "```suggestion\nT\n```", "path": "topic.txt", "side": "new",
                     "line_start": 1, "line_end": 1}),
        ),
    );
    assert!(r.status < 300, "{}", r.body);
    let suggestion = r.json()["comments"][0]["id"].as_str().unwrap().to_owned();
    Fixture { head, suggestion }
}

#[test]
fn conf_rbac_01_merge_request_actions_agree_with_the_policy() {
    let matrix = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/core/tests/matrix.csv"),
    )
    .unwrap();
    let s = TestServer::start_with(&[PROJ]);
    let dir = tempfile::tempdir().unwrap();
    let mut alice = s.login("alice", PASSWORD);
    let alice_token = create_token(&mut alice, "setup", &["repo:write", "mrs:write"]);
    let f = setup(&s, &alice_token, dir.path());
    let all = ["repo:write", "mrs:write", "statuses:write", "user:read"];
    let mut people = Vec::new();
    for (name, role) in PEOPLE {
        s.create_user(name, PASSWORD, false);
        if *role != "none" {
            let r = submit(
                &mut alice,
                "/alice/proj/settings/access",
                "/alice/proj/settings/access",
                &[("username", name), ("role", role)],
            );
            assert_eq!(r.status, 303, "{name}: {}", r.body);
        }
        let mut web = s.login(name, PASSWORD);
        let token = create_token(&mut web, "probe", &all);
        let scoped: Vec<(&str, String)> = if *role == "admin" {
            let mut v: Vec<(&str, String)> = SCOPES
                .iter()
                .map(|sc| (*sc, create_token(&mut web, sc, &[sc, "repo:read"])))
                .collect();
            // Writing the source branch is a push: those actions need
            // `repo:write` too, as the API documents.
            v.push((
                "mrs:write+push",
                create_token(&mut web, "push", &["mrs:write", "repo:write"]),
            ));
            v
        } else {
            Vec::new()
        };
        people.push((*name, *role, token, scoped));
    }

    let mut failures = Vec::new();
    let mut checked = 0;
    let mut n = 0;
    let mut c = s.client();
    for vis in ["public", "private"] {
        for state in ["active", "archived"] {
            s.db.execute(&format!(
                "UPDATE repositories SET visibility = '{vis}', state = '{state}'
                  WHERE name = 'proj'"
            ));
            for action in ACTIONS {
                let cell = format!("{action},{vis},{state},anon,none,session");
                if let Some(want) = expected(&matrix, &cell) {
                    n += 1;
                    let got = call(
                        &mut c,
                        None,
                        &request(action, &f, "anon", n),
                        "anon",
                        &alice_token,
                    );
                    checked += 1;
                    if !agrees(got, &want, true) {
                        failures.push(format!("{cell}: got {got}, policy {want}"));
                    }
                }
                for (name, role, token, scoped) in &people {
                    let cell = format!("{action},{vis},{state},user,{role},session");
                    let want = expected(&matrix, &cell)
                        .unwrap_or_else(|| panic!("matrix has no cell {cell}"));
                    n += 1;
                    let got = call(
                        &mut c,
                        Some(token),
                        &request(action, &f, name, n),
                        name,
                        &alice_token,
                    );
                    checked += 1;
                    if !agrees(got, &want, false) {
                        failures.push(format!("{cell} ({name}): got {got}, policy {want}"));
                    }
                    for (scope, t) in scoped {
                        let writes_source =
                            matches!(*action, "mr.update_branch" | "mr.apply_suggestion");
                        let scope = match (*scope, writes_source) {
                            ("mrs:write", true) | ("mrs:write+push", false) => continue,
                            ("mrs:write+push", true) => "mrs:write",
                            (s, _) => s,
                        };
                        let cell = format!("{action},{vis},{state},user,{role},token:{scope}");
                        let Some(want) = expected(&matrix, &cell) else {
                            continue;
                        };
                        n += 1;
                        let got = call(
                            &mut c,
                            Some(t),
                            &request(action, &f, name, n),
                            name,
                            &alice_token,
                        );
                        checked += 1;
                        if !agrees(got, &want, false) {
                            failures.push(format!("{cell}: got {got}, policy {want}"));
                        }
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {checked} disagree:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(checked > 300, "only {checked} cells checked");
}

const PAYLOAD: &str = "<img src=x onerror=alert(1)>";

/// Markup that must never reach a page or `*_html` field unescaped,
/// besides what `assert_inert` looks for. Ref names can't hold spaces,
/// so their payload is written with slashes.
const RAW: &[&str] = &["<img/src=x", "<svg/onload", "<img src=x"];

fn assert_escaped(body: &str, what: &str) {
    assert_inert(body, what);
    for bad in RAW {
        assert!(!body.contains(bad), "{what}: found {bad:?}");
    }
}

/// Stored XSS through what M3 shows from Git and from people: commit
/// messages, ref names, file paths in diffs, suggestion blocks, merge
/// request titles and descriptions, review and thread comments, and
/// status contexts, descriptions, and links.
#[test]
fn conf_sec_04_xss_commits_refs_paths_suggestions() {
    let s = TestServer::start_with(&[PROJ]);
    let mut alice = s.login("alice", PASSWORD);
    let t = create_token(
        &mut alice,
        "t",
        &["repo:write", "mrs:write", "statuses:write"],
    );
    let r = alice.api(
        "PATCH",
        "/api/v1/repos/alice/proj",
        Some(&t),
        Some(&json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "proj\n").unwrap();
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "init"]);
    git.ok(&w, &["push", "-q", &s.url(&PROJ), "main"]);
    let branch = "x/<img/src=x/onerror=alert(1)>";
    git.ok(&w, &["switch", "-q", "-c", branch]);
    let path = format!("{PAYLOAD}.txt");
    std::fs::write(w.join(&path), "one\ntwo\n").unwrap();
    std::fs::write(w.join("plain.txt"), "one\ntwo\n").unwrap();
    git.ok(&w, &["add", "-A"]);
    git.ok(
        &w,
        &[
            "commit",
            "-q",
            "-m",
            &format!(
                "{PAYLOAD} subject\n\n\"><svg/onload=alert(1)> body\n[x](javascript:alert(1))"
            ),
        ],
    );
    let out = git
        .cmd(&w)
        .args(["push", &s.url(&PROJ), branch])
        .output()
        .unwrap();
    // Slath may refuse the name; if it takes it, it must show it safely.
    let pushed = out.status.success();
    let source = if pushed {
        branch.to_owned()
    } else {
        git.ok(&w, &["branch", "-q", "-m", "plain-name"]);
        git.ok(&w, &["push", "-q", &s.url(&PROJ), "plain-name"]);
        "plain-name".to_owned()
    };
    let head = git.ok(&w, &["rev-parse", "HEAD"]).trim().to_owned();
    let r = alice.api(
        "POST",
        "/api/v1/repos/alice/proj/merge-requests",
        Some(&t),
        Some(
            &json!({"source_branch": source, "title": format!("{PAYLOAD} title"),
                     "description": format!("{PAYLOAD}\n\n<script>alert(1)</script>")}),
        ),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let mr = "/api/v1/repos/alice/proj/merge-requests/1";
    let start = std::time::Instant::now();
    while alice.api("GET", mr, Some(&t), None).json()["revisions"] != 1 {
        assert!(start.elapsed().as_secs() < 30, "no first revision");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    for (file, body) in [
        (path.as_str(), format!("```suggestion\n{PAYLOAD}\n```")),
        ("plain.txt", format!("{PAYLOAD} comment")),
    ] {
        let r = alice.api(
            "POST",
            &format!("{mr}/threads"),
            Some(&t),
            Some(&json!({"body": body, "path": file, "side": "new",
                         "line_start": 1, "line_end": 1})),
        );
        assert!(r.status < 300, "{file}: {}", r.body);
    }
    let r = alice.api(
        "POST",
        &format!("{mr}/comments"),
        Some(&t),
        Some(&json!({"body": format!("\"><svg/onload=alert(1)> {PAYLOAD}")})),
    );
    assert!(r.status < 300, "{}", r.body);
    let r = alice.api(
        "POST",
        &format!("/api/v1/repos/alice/proj/statuses/{head}"),
        Some(&t),
        Some(
            &json!({"state": "failure", "context": format!("ci/{PAYLOAD}"),
                     "description": PAYLOAD, "target_url": "javascript:alert(1)"}),
        ),
    );
    assert!(
        r.status < 300 || r.status == 422,
        "status: {} {}",
        r.status,
        r.body
    );

    let encoded = |s: &str| {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>()
    };
    let pages = [
        "/alice/proj/merge-requests".to_owned(),
        "/alice/proj/merge-requests/1".to_owned(),
        "/alice/proj/merge-requests/1/commits".to_owned(),
        "/alice/proj/merge-requests/1/changes".to_owned(),
        format!("/alice/proj/commit/{head}"),
        format!("/alice/proj/commits/{}", encoded(&source)),
        "/alice/proj/branches".to_owned(),
        format!("/alice/proj/compare?source={}", encoded(&source)),
    ];
    let mut anon = s.client();
    for page in &pages {
        for (who, c) in [("alice", &mut alice), ("anonymous", &mut anon)] {
            let r = c.get(page);
            assert_eq!(r.status, 200, "{who} {page}");
            assert_escaped(&r.body, &format!("{who} {page}"));
        }
    }
    // The API's rendered fields.
    let m = alice.api("GET", mr, Some(&t), None).json();
    assert_escaped(
        m["description_html"].as_str().unwrap_or(""),
        "description_html",
    );
    let threads = alice
        .api("GET", &format!("{mr}/threads"), Some(&t), None)
        .json();
    for th in threads.as_array().into_iter().flatten() {
        for c in th["comments"].as_array().into_iter().flatten() {
            assert_escaped(c["body_html"].as_str().unwrap_or(""), "thread body_html");
        }
    }
    if !pushed {
        eprintln!("the payload branch name was refused at push");
    }
}
