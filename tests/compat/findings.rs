// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Findings (AGENT-02): line annotations on commit statuses are checked
//! against the commit and stored with their status. They become threads
//! only on the open merge request revisions of the same repository whose
//! head is the annotated commit, resolve when a later status of the same
//! context no longer reports them, and never count as verdicts or
//! requirements.

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

fn token(s: &TestServer, user: &str) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, user, &["repo:write", "mrs:write", "statuses:write"])
}

fn api(
    s: &TestServer,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> slath_tests::http::Resp {
    s.client().api(
        method,
        &format!("/api/v1/repos/alice/demo{path}"),
        Some(token),
        body.as_ref(),
    )
}

/// Pushes `main` with a 10-line `src/lib.rs` and returns its commit.
fn push_main(s: &TestServer, w: &std::path::Path) -> String {
    let git = GitClient::new();
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(w.join("src")).unwrap();
    let text: String = (1..=10).map(|i| format!("line {i}\n")).collect();
    std::fs::write(w.join("src/lib.rs"), text).unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &s.url(&DEMO), "main"]);
    git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn note(path: &str, line: i64) -> Value {
    json!({"path": path, "line_start": line, "severity": "warning", "message": "Unused variable"})
}

#[test]
fn conf_agent_02_annotations_are_checked_and_stored() {
    let s = TestServer::start_full(&[DEMO], "[limits]\nstatus_annotations = 3\n");
    let alice = token(&s, "alice");
    let d = tempfile::tempdir().unwrap();
    let head = push_main(&s, d.path());
    let post = |annotations: Value| {
        api(
            &s,
            &alice,
            "POST",
            &format!("/statuses/{head}"),
            Some(json!({"state": "failure", "context": "lint", "annotations": annotations})),
        )
    };
    let field = |r: &slath_tests::http::Resp| r.json()["errors"][0]["field"].clone();

    // Every annotation is checked; one bad one rejects the whole status.
    for (bad, at) in [
        (
            json!([note("src/lib.rs", 1), note("src/nope.rs", 1)]),
            "annotations[1].path",
        ),
        (json!([note("../etc/passwd", 1)]), "annotations[0].path"),
        (json!([note("src/lib.rs", 11)]), "annotations[0].line_end"),
        (json!([note("src/lib.rs", 0)]), "annotations[0].line_start"),
        (
            json!([{"path": "src/lib.rs", "line_start": 1, "severity": "fatal", "message": "x"}]),
            "annotations[0].severity",
        ),
        (
            json!([{"path": "src/lib.rs", "line_start": 1, "severity": "notice", "category": "style", "message": "x"}]),
            "annotations[0].category",
        ),
        (
            json!([{"path": "src/lib.rs", "line_start": 1, "severity": "notice", "message": "x".repeat(4097)}]),
            "annotations[0].message",
        ),
        (
            json!([
                note("src/lib.rs", 1),
                note("src/lib.rs", 2),
                note("src/lib.rs", 3),
                note("src/lib.rs", 4)
            ]),
            "annotations",
        ),
    ] {
        let r = post(bad);
        assert_eq!(r.status, 422, "{}", r.body);
        assert_eq!(field(&r), at, "{}", r.body);
    }
    let list = api(
        &s,
        &alice,
        "GET",
        &format!("/commits/{head}/statuses"),
        None,
    )
    .json();
    assert_eq!(list, json!([]), "a rejected status leaves nothing");

    // A valid status keeps its annotations in order, with the defaults.
    let r = post(json!([
        {"path": "src/lib.rs", "line_start": 9, "line_end": 10, "severity": "failure",
         "category": "security", "message": "Secret in code\n\tline two"},
        note("src/lib.rs", 2),
    ]));
    assert_eq!(r.status, 201, "{}", r.body);
    slath_tests::contract::check("POST", "/api/v1/repos/{ns}/{repo}/statuses/{oid}", &r);
    let list = api(
        &s,
        &alice,
        "GET",
        &format!("/commits/{head}/statuses"),
        None,
    );
    slath_tests::contract::check(
        "GET",
        "/api/v1/repos/{ns}/{repo}/commits/{oid}/statuses",
        &list,
    );
    assert_eq!(
        list.json()[0]["annotations"],
        json!([
            {"path": "src/lib.rs", "line_start": 9, "line_end": 10, "severity": "failure",
             "category": "security", "message": "Secret in code\n\tline two"},
            {"path": "src/lib.rs", "line_start": 2, "line_end": 2, "severity": "warning",
             "category": "other", "message": "Unused variable"},
        ])
    );
}

/// alice/demo with `main`, bob (writer) with a merge request from `topic`,
/// and the bot `lint` (write role, which reporting statuses needs).
struct Setup {
    s: TestServer,
    alice: String,
    bob: String,
    bot: String,
    dir: tempfile::TempDir,
    main: String,
}

impl Setup {
    fn new(protection: Value) -> Self {
        let s = TestServer::start();
        s.create_user("bob", PASSWORD, false);
        let (alice, bob) = (token(&s, "alice"), token(&s, "bob"));
        let r = api(
            &s,
            &alice,
            "PUT",
            "/collaborators/bob",
            Some(json!({"role": "write"})),
        );
        assert!(r.status < 300, "{}", r.body);
        let (ok, out, err) = s.slath(
            &[
                "admin",
                "create-bot",
                "--owner",
                "alice",
                "--name",
                "lint",
                "--token-scopes",
                "repo:read,statuses:write",
                "--repo",
                "alice/demo",
                "--output",
                "json",
            ],
            None,
        );
        assert!(ok, "{out}{err}");
        let bot = serde_json::from_str::<Value>(&out).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned();
        let r = api(
            &s,
            &alice,
            "PUT",
            "/collaborators/lint",
            Some(json!({"role": "write"})),
        );
        assert!(r.status < 300, "{}", r.body);
        let r = api(&s, &alice, "POST", "/protections", Some(protection));
        assert_eq!(r.status, 200, "{}", r.body);
        let dir = tempfile::tempdir().unwrap();
        let main = push_main(&s, dir.path());
        Self {
            s,
            alice,
            bob,
            bot,
            dir,
            main,
        }
    }

    fn w(&self) -> &std::path::Path {
        self.dir.path()
    }

    /// Commits `text` as `src/lib.rs` on `branch` and pushes it.
    fn push(&self, branch: &str, text: &str, amend: bool) -> String {
        let git = GitClient::new();
        let w = self.w();
        if git
            .cmd(w)
            .args(["rev-parse", "--verify", "-q", branch])
            .status()
            .unwrap()
            .success()
        {
            git.ok(w, &["switch", "-q", branch]);
        } else {
            git.ok(w, &["switch", "-q", "-c", branch, "main"]);
        }
        std::fs::write(w.join("src/lib.rs"), text).unwrap();
        git.ok(w, &["add", "."]);
        if amend {
            git.ok(w, &["commit", "-q", "--amend", "--no-edit"]);
        } else {
            git.ok(w, &["commit", "-q", "-m", branch]);
        }
        git.ok(w, &["push", "-q", "--force", &self.s.url(&DEMO), branch]);
        git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn open(&self, branch: &str) -> i64 {
        let r = api(
            &self.s,
            &self.bob,
            "POST",
            "/merge-requests",
            Some(json!({"source_branch": branch, "title": branch})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        self.wait(n, 1);
        n
    }

    fn wait(&self, n: i64, k: i64) {
        let start = std::time::Instant::now();
        while api(
            &self.s,
            &self.alice,
            "GET",
            &format!("/merge-requests/{n}"),
            None,
        )
        .json()["revisions"]
            != k
        {
            assert!(
                start.elapsed().as_secs() < 30,
                "no revision {k}\n{}",
                self.s.log()
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    fn report(&self, token: &str, oid: &str, context: &str, annotations: Value) {
        let r = api(
            &self.s,
            token,
            "POST",
            &format!("/statuses/{oid}"),
            Some(json!({"state": "failure", "context": context, "annotations": annotations})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
    }

    /// The finding threads of a merge request, as the API shows them.
    fn findings(&self, n: i64) -> Vec<Value> {
        let r = api(
            &self.s,
            &self.alice,
            "GET",
            &format!("/merge-requests/{n}/threads"),
            None,
        );
        slath_tests::contract::check(
            "GET",
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/threads",
            &r,
        );
        r.json()
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| !t["finding"].is_null())
            .cloned()
            .collect()
    }

    fn requirement(&self, n: i64, key: &str) -> String {
        let m = api(
            &self.s,
            &self.alice,
            "GET",
            &format!("/merge-requests/{n}/mergeability"),
            None,
        )
        .json();
        m["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .unwrap_or_else(|| panic!("no {key} in {m}"))["state"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

fn text(edit: usize) -> String {
    (1..=10)
        .map(|i| {
            if i == edit {
                format!("line {i}, edited\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect()
}

fn message(t: &Value) -> &str {
    t["comments"][0]["body"].as_str().unwrap()
}

#[test]
fn conf_agent_02_findings_land_only_on_the_matching_revision() {
    let u = Setup::new(json!({"pattern": "main", "required_approvals": 0}));
    let h1 = u.push("topic", &text(3), false);
    let n = u.open("topic");
    let other = u.push("other", &text(5), false);
    let m = u.open("other");

    // Statuses on a commit that's no revision's head make no threads.
    u.report(&u.bot, &u.main, "lint", json!([note("src/lib.rs", 1)]));
    assert!(u.findings(n).is_empty());
    // On the head: threads on that revision only, by the bot.
    u.report(
        &u.bot,
        &h1,
        "lint",
        json!([note("src/lib.rs", 3), note("src/lib.rs", 3),
               {"path": "src/lib.rs", "line_start": 7, "line_end": 8, "severity": "failure",
                "category": "security", "message": "Looks like a key"}]),
    );
    let f = u.findings(n);
    assert_eq!(
        f.len(),
        2,
        "duplicates within a status make one thread: {f:?}"
    );
    assert_eq!(f[0]["revision"], 1);
    assert_eq!(f[0]["author"], "lint");
    assert_eq!(f[0]["line_start"], 3);
    assert_eq!(f[0]["side"], "new");
    assert_eq!(f[0]["position"]["state"], "current");
    assert_eq!(
        f[0]["finding"],
        json!({"context": "lint", "severity": "warning", "category": "other"})
    );
    assert_eq!(message(&f[0]), "Unused variable");
    assert_eq!(
        f[1]["finding"],
        json!({"context": "lint", "severity": "failure", "category": "security"})
    );
    assert_eq!(
        (f[1]["line_start"].clone(), f[1]["line_end"].clone()),
        (json!(7), json!(8))
    );
    assert!(u.findings(m).is_empty(), "never on another merge request");
    // The same report again changes nothing.
    u.report(
        &u.bot,
        &h1,
        "lint",
        json!([note("src/lib.rs", 3),
               {"path": "src/lib.rs", "line_start": 7, "line_end": 8, "severity": "failure",
                "category": "security", "message": "Looks like a key"}]),
    );
    assert_eq!(u.findings(n), f);

    // A new revision: the old findings stay on revision 1 and don't carry
    // forward, even on unchanged lines; the new head gets its own.
    let h2 = u.push("topic", &text(4), true);
    u.wait(n, 2);
    let f = u.findings(n);
    assert_eq!(f.len(), 2);
    assert!(
        f.iter().all(|t| t["revision"] == 1
            && t["position"]["state"] == "outdated"
            && t["resolved"] == false),
        "{f:?}"
    );
    u.report(&u.bot, &h2, "lint", json!([note("src/lib.rs", 3)]));
    let f = u.findings(n);
    assert_eq!(f.len(), 3);
    assert_eq!(f[2]["revision"], 2);
    assert_eq!(f[2]["position"]["state"], "current");
    // A late status on revision 1's head still lands on revision 1.
    u.report(&u.bot, &h1, "late", json!([note("src/lib.rs", 2)]));
    let late: Vec<_> = u
        .findings(n)
        .into_iter()
        .filter(|t| t["finding"]["context"] == "late")
        .collect();
    assert_eq!(late.len(), 1);
    assert_eq!(late[0]["revision"], 1);

    // The page marks findings and shows messages as plain text.
    u.report(
        &u.bot,
        &h2,
        "ui",
        json!([{"path": "src/lib.rs", "line_start": 1, "severity": "notice",
                "message": "**not bold** <b>x</b> [link](https://example.org)"}]),
    );
    let page =
        u.s.login("alice", PASSWORD)
            .get(&format!("/alice/demo/merge-requests/{n}"))
            .body;
    assert!(page.contains("Finding from ui (notice)"), "{page}");
    assert!(page.contains("Finding from late (warning)"), "{page}");
    assert!(
        page.contains("**not bold** &#60;b&#62;x&#60;/b&#62; [link](https://example.org)"),
        "{page}"
    );
    assert!(!page.contains("<b>x</b>"));

    // Closed merge requests get no new threads.
    let r = api(
        &u.s,
        &u.bob,
        "PATCH",
        &format!("/merge-requests/{m}"),
        Some(json!({"state": "closed"})),
    );
    assert!(r.status < 300, "{}", r.body);
    u.report(&u.bot, &other, "lint", json!([note("src/lib.rs", 5)]));
    assert!(u.findings(m).is_empty());
}

#[test]
fn conf_agent_02_findings_resolve_when_no_longer_reported() {
    let u = Setup::new(json!({"pattern": "main", "required_approvals": 0}));
    let h = u.push("topic", &text(3), false);
    let n = u.open("topic");
    let both = json!([note("src/lib.rs", 2), note("src/lib.rs", 3)]);
    u.report(&u.bot, &h, "lint", both.clone());
    u.report(&u.bot, &h, "tests", json!([note("src/lib.rs", 9)]));
    assert_eq!(u.findings(n).len(), 3);
    let by_line = |line: i64, ctx: &str| {
        u.findings(n)
            .into_iter()
            .find(|t| t["line_start"] == line && t["finding"]["context"] == ctx)
            .unwrap()
    };

    // A later lint status without line 2: that thread resolves, attributed
    // to the reporter; the other context's thread is untouched.
    u.report(&u.bot, &h, "lint", json!([note("src/lib.rs", 3)]));
    let two = by_line(2, "lint");
    assert_eq!(two["resolved"], true);
    assert_eq!(two["resolved_by"], "lint");
    assert_eq!(by_line(3, "lint")["resolved"], false);
    assert_eq!(by_line(9, "tests")["resolved"], false);
    // Reported again: reopened, no second thread.
    u.report(&u.bot, &h, "lint", both.clone());
    assert_eq!(by_line(2, "lint")["resolved"], false);
    assert_eq!(u.findings(n).len(), 3);

    // A person resolves one; a later report doesn't reopen it, and a clean
    // status resolves the rest.
    let three = by_line(3, "lint")["id"].as_str().unwrap().to_owned();
    let r = api(
        &u.s,
        &u.alice,
        "PATCH",
        &format!("/merge-requests/{n}/threads/{three}"),
        Some(json!({"resolved": true})),
    );
    assert!(r.status < 300, "{}", r.body);
    u.report(&u.bot, &h, "lint", both);
    assert_eq!(by_line(3, "lint")["resolved_by"], "alice");
    u.report(&u.bot, &h, "lint", json!([]));
    assert_eq!(by_line(2, "lint")["resolved"], true);
    assert_eq!(by_line(3, "lint")["resolved_by"], "alice");
    assert_eq!(by_line(9, "tests")["resolved"], false);

    // People can reply to findings like to any thread.
    let nine = by_line(9, "tests")["id"].as_str().unwrap().to_owned();
    let r = api(
        &u.s,
        &u.bob,
        "POST",
        &format!("/merge-requests/{n}/threads/{nine}/comments"),
        Some(json!({"body": "Fixed in the next push"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(by_line(9, "tests")["comments"].as_array().unwrap().len(), 2);
}

#[test]
fn conf_agent_02_findings_are_never_verdicts_or_requirements() {
    let u = Setup::new(
        json!({"pattern": "main", "required_approvals": 1, "require_resolved_threads": true}),
    );
    let h = u.push("topic", &text(3), false);
    let n = u.open("topic");
    u.report(&u.bot, &h, "lint", json!([note("src/lib.rs", 3)]));
    assert_eq!(u.findings(n).len(), 1);
    assert_eq!(
        u.requirement(n, "threads"),
        "met",
        "findings don't count as unresolved threads"
    );
    assert_eq!(u.requirement(n, "approvals"), "unmet");
    let reviews = api(
        &u.s,
        &u.alice,
        "GET",
        &format!("/merge-requests/{n}/reviews"),
        None,
    )
    .json();
    assert_eq!(reviews, json!([]), "a finding is no review");
    // A person's thread still counts.
    let r = api(
        &u.s,
        &u.alice,
        "POST",
        &format!("/merge-requests/{n}/threads"),
        Some(json!({"body": "Why?", "path": "src/lib.rs", "line_start": 3})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(u.requirement(n, "threads"), "unmet");
}

#[test]
fn conf_agent_02_statuses_in_a_fork_never_create_threads() {
    use crate::fork_merge_requests::{api as repos_api, commit, fork, http_url};
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let (alice, bob) = (token(&s, "alice"), token(&s, "bob"));
    let d = tempfile::tempdir().unwrap();
    push_main(&s, d.path());
    let r = repos_api(
        &s,
        &alice,
        "PATCH",
        "/alice/demo",
        Some(json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);
    fork(&s, &bob, json!({"namespace": "bob"}), "/bob/demo/fork");
    let git = GitClient::new();
    let b = d.path().join("bob");
    git.ok(
        d.path(),
        &["clone", "-q", &http_url(&s, &bob, "bob/demo"), "bob"],
    );
    git.ok(&b, &["switch", "-q", "-c", "topic"]);
    let head = commit(&git, &b, "a.txt");
    git.ok(&b, &["push", "-q", "origin", "topic"]);
    let r = repos_api(
        &s,
        &bob,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_repo": "bob/demo", "source_branch": "topic", "title": "A"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    let threads = || {
        repos_api(
            &s,
            &alice,
            "GET",
            &format!("/alice/demo/merge-requests/{n}/threads"),
            None,
        )
        .json()
    };
    let annotated =
        json!({"state": "failure", "context": "lint", "annotations": [note("a.txt", 1)]});
    // Bob reports on his fork's commit: the merge request in the original
    // reads statuses from the original only.
    let r = repos_api(
        &s,
        &bob,
        "POST",
        &format!("/bob/demo/statuses/{head}"),
        Some(annotated.clone()),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(threads(), json!([]));
    // Reported in the original, the same commit gets its finding.
    let r = repos_api(
        &s,
        &alice,
        "POST",
        &format!("/alice/demo/statuses/{head}"),
        Some(annotated),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let list = threads();
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    assert_eq!(list[0]["finding"]["context"], "lint");
}
