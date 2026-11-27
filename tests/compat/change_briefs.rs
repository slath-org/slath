// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Change briefs through the API: they belong to a version's commit, keep
//! every edit, are read by everyone who can read the merge request and
//! written by its author and writers while it's open, refuse secrets with
//! their place, and reach MCP as untrusted text.

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

struct World {
    s: TestServer,
    alice: String,
    bob: String,
    carol: String,
    git: GitClient,
    url: String,
    w: tempfile::TempDir,
    n: i64,
}

fn token(s: &TestServer, user: &str, scopes: &[&str]) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, user, scopes)
}

impl World {
    /// alice/demo (private) with bob a writer and carol a reader, and an
    /// MR by alice of one change, "Add a".
    fn start() -> Self {
        let s = TestServer::start();
        for u in ["bob", "carol", "dave"] {
            s.create_user(u, PASSWORD, false);
        }
        let all = ["repo:write", "mrs:write"];
        let mut world = Self {
            alice: token(&s, "alice", &all),
            bob: token(&s, "bob", &all),
            carol: token(&s, "carol", &all),
            url: s.url(&DEMO),
            s,
            git: GitClient::new(),
            w: tempfile::tempdir().unwrap(),
            n: 0,
        };
        for (who, role) in [("bob", "write"), ("carol", "read")] {
            let r = world.api(
                &world.alice,
                "PUT",
                &format!("/collaborators/{who}"),
                Some(json!({"role": role})),
            );
            assert!(r.status < 300, "{}", r.body);
        }
        let w = world.w.path().to_owned();
        world.git.ok(&w, &["init", "-q", "-b", "main"]);
        world.commit("README", "demo\n", "init");
        world.push("main");
        world.git.ok(&w, &["switch", "-q", "-c", "topic"]);
        world.commit("a.txt", "a\n", "Add a");
        world.push("topic");
        let r = world.api(
            &world.alice,
            "POST",
            "/merge-requests",
            Some(json!({"source_branch": "topic", "title": "Add a"})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        world.n = r.json()["number"].as_i64().unwrap();
        world.wait_revisions(1);
        world
    }

    fn api(
        &self,
        token: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> slath_tests::http::Resp {
        self.s.client().api(
            method,
            &format!("/api/v1/repos/alice/demo{path}"),
            Some(token),
            body.as_ref(),
        )
    }

    fn commit(&self, file: &str, text: &str, msg: &str) {
        let w = self.w.path();
        std::fs::write(w.join(file), text).unwrap();
        self.git.ok(w, &["add", "-A"]);
        self.git.ok(w, &["commit", "-q", "-m", msg]);
    }

    fn push(&self, branch: &str) {
        self.git
            .ok(self.w.path(), &["push", "-q", "--force", &self.url, branch]);
    }

    fn wait_revisions(&self, k: i64) {
        let start = std::time::Instant::now();
        while self
            .api(
                &self.alice,
                "GET",
                &format!("/merge-requests/{}", self.n),
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

    /// The change's display ID and its current commit.
    fn change(&self) -> (String, String) {
        let r = self.api(
            &self.alice,
            "GET",
            &format!("/merge-requests/{}/stack", self.n),
            None,
        );
        let e = &r.json()["entries"][0];
        (
            e["change_id"].as_str().unwrap().to_owned(),
            e["commit_oid"].as_str().unwrap().to_owned(),
        )
    }

    fn briefs_path(&self, change: &str) -> String {
        format!("/merge-requests/{}/changes/{change}/briefs", self.n)
    }

    fn write(&self, token: &str, change: &str, body: Value) -> slath_tests::http::Resp {
        self.api(token, "POST", &self.briefs_path(change), Some(body))
    }

    fn list(&self, token: &str, change: &str, query: &str) -> Vec<Value> {
        let r = self.api(
            token,
            "GET",
            &format!("{}{query}", self.briefs_path(change)),
            None,
        );
        assert_eq!(r.status, 200, "{}", r.body);
        r.json()["items"].as_array().unwrap().clone()
    }
}

fn brief(intent: &str) -> Value {
    json!({"schema": "slath-brief/1", "intent": intent, "approach": "Line one\nLine two",
           "verification": "Ran the tests", "gaps": ""})
}

#[test]
fn conf_agent_15_briefs_belong_to_versions_and_keep_every_edit() {
    let w = World::start();
    let (id, v1) = w.change();
    let r = w.write(&w.alice, &id, brief("Add a"));
    assert_eq!(r.status, 201, "{}", r.body);
    let b = r.json();
    assert_eq!(b["commit"], v1.as_str());
    assert_eq!(b["version"], 1);
    assert_eq!(b["author"], "alice");
    assert_eq!(b["approach"], "Line one\nLine two");
    // An empty optional field is absent.
    assert_eq!(b["gaps"], Value::Null);
    assert_eq!(b["untrusted_content"], true);
    assert_eq!(b["origin"], "writer");
    // Writing again adds an edit; the newest comes first.
    let r = w.write(&w.bob, &id[..8], brief("Add a, edited"));
    assert_eq!(r.status, 201, "{}", r.body);
    let all = w.list(&w.alice, &id, "");
    assert_eq!(all.len(), 2);
    assert_eq!(all[0]["intent"], "Add a, edited");
    assert_eq!(all[0]["author"], "bob");

    // A new version starts without a brief; the old one keeps its own.
    w.git.ok(
        w.w.path(),
        &["commit", "-q", "--amend", "-m", "Add a, again"],
    );
    w.push("topic");
    w.wait_revisions(2);
    let (_, v2) = w.change();
    assert!(w.list(&w.alice, &id, &format!("?commit={v2}")).is_empty());
    assert_eq!(w.list(&w.alice, &id, &format!("?commit={v1}")).len(), 2);
    let r = w.write(&w.alice, &id, brief("Version two"));
    assert_eq!(r.json()["commit"], v2.as_str());
    assert_eq!(r.json()["version"], 2);
    // An earlier version can still get an edit, by its commit.
    let mut late = brief("Version one, late");
    late["commit"] = json!(v1);
    assert_eq!(w.write(&w.alice, &id, late).json()["version"], 1);
    // The change can be named by a version's commit too.
    assert_eq!(w.list(&w.alice, &v2, "").len(), 4);

    // Refused: another commit, another schema, unknown keys, no intent.
    let mut wrong = brief("x");
    wrong["commit"] = json!("0".repeat(40));
    let r = w.write(&w.alice, &id, wrong);
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("/commit"), "{}", r.body);
    for bad in [
        json!({"schema": "slath-brief/2", "intent": "x"}),
        json!({"schema": "slath-brief/1", "intent": "x", "risks": "y"}),
        json!({"schema": "slath-brief/1", "intent": " "}),
        json!({"schema": "slath-brief/1", "intent": "x", "approach": "y".repeat(16 * 1024 + 1)}),
    ] {
        let r = w.write(&w.alice, &id, bad.clone());
        assert_eq!(r.status, 422, "{bad}: {}", r.body);
    }
    let r = w.write(&w.alice, &id, brief(&"x".repeat(70 * 1024)));
    assert!(matches!(r.status, 413 | 422), "{} {}", r.status, r.body);
    assert_eq!(w.list(&w.alice, &id, "").len(), 4);
    // An unknown change is 404.
    assert_eq!(w.write(&w.alice, "zzzzzzzz", brief("x")).status, 404);
}

#[test]
fn conf_agent_15_merge_request_readers_read_briefs_and_its_updaters_write_them() {
    let w = World::start();
    let (id, _) = w.change();
    assert_eq!(w.write(&w.alice, &id, brief("By the author")).status, 201);
    // A reader reads them but can't write; others get 404, signed out too.
    assert_eq!(w.list(&w.carol, &id, "").len(), 1);
    assert_eq!(w.write(&w.carol, &id, brief("x")).status, 403);
    let dave = token(&w.s, "dave", &["repo:write", "mrs:write"]);
    assert_eq!(w.api(&dave, "GET", &w.briefs_path(&id), None).status, 404);
    assert_eq!(w.write(&dave, &id, brief("x")).status, 404);
    let anon = w.s.client().api(
        "GET",
        &format!("/api/v1/repos/alice/demo{}", w.briefs_path(&id)),
        None,
        None,
    );
    assert_eq!(anon.status, 404);
    // A writer writes with push rights; without the scope for it, not.
    assert_eq!(w.write(&w.bob, &id, brief("By a writer")).status, 201);
    let bob_mrs = token(&w.s, "bob", &["mrs:write"]);
    assert_eq!(w.write(&bob_mrs, &id, brief("x")).status, 403);
    // The author's token needs mrs:write.
    let alice_read = token(&w.s, "alice", &["repo:read"]);
    assert_eq!(w.write(&alice_read, &id, brief("x")).status, 403);
    // A closed merge request's briefs stay as they are.
    let r = w.api(
        &w.alice,
        "PATCH",
        &format!("/merge-requests/{}", w.n),
        Some(json!({"state": "closed"})),
    );
    assert!(r.status < 300, "{}", r.body);
    assert_eq!(w.write(&w.alice, &id, brief("x")).status, 409);
    assert_eq!(w.list(&w.carol, &id, "").len(), 2);
    // MCP reads them as untrusted text.
    let r = w.s.client().api_with(
        "POST",
        "/api/mcp",
        Some(&w.carol),
        Some(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "list_change_briefs",
            "arguments": {"ns": "alice", "repo": "demo", "number": w.n, "change": id}}}),
        ),
        &[
            ("accept", "application/json, text/event-stream"),
            ("mcp-protocol-version", "2025-06-18"),
        ],
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains("By a writer"), "{}", r.body);
    assert!(r.body.contains("untrusted_content"), "{}", r.body);
}

#[test]
fn conf_agent_06_a_brief_with_a_secret_is_refused_with_its_location() {
    let w = World::start();
    let (id, _) = w.change();
    let secret = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
    let mut b = brief("Deploy");
    b["verification"] = json!(format!("Deployed\nwith {secret} to staging"));
    let r = w.write(&w.alice, &id, b.clone());
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("/verification"), "{}", r.body);
    assert!(r.body.contains("\"secret\""), "{}", r.body);
    assert!(r.body.contains("Line 2"), "{}", r.body);
    assert!(!r.body.contains(&secret), "{}", r.body);
    assert!(w.list(&w.alice, &id, "").is_empty());
    assert_eq!(
        w.s.db
            .query_one("SELECT count(*)::text FROM change_briefs t WHERE t::text LIKE '%ghp_%'"),
        "0"
    );
    // The redacted retry is accepted.
    b["verification"] = json!("Deployed with [redacted] to staging");
    assert_eq!(w.write(&w.alice, &id, b).status, 201);
}

#[test]
fn conf_agent_15_briefs_on_the_change_page_and_their_form() {
    use slath_tests::http::submit;
    let w = World::start();
    let (id, _) = w.change();
    let page = format!("/alice/demo/merge-requests/{}/changes/{id}", w.n);
    let form = format!("{page}/brief");
    // Without a brief: writers see one line to write it, readers nothing.
    let mut bob = w.s.login("bob", PASSWORD);
    let mut carol = w.s.login("carol", PASSWORD);
    let r = bob.get(&page);
    assert!(r.body.contains("Write a brief</a>"), "{}", r.body);
    assert!(!carol.get(&page).body.contains("Write a brief"));
    assert_eq!(carol.get(&form).status, 404);

    // Writing through the form returns to the change's page.
    let r = submit(
        &mut bob,
        &form,
        &form,
        &[
            ("intent", "Add the a file"),
            ("approach", "One line\nper letter"),
            ("verification", ""),
            ("gaps", "No b yet"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.header("location"), Some(format!("{page}#brief").as_str()));
    let shown = carol.get(&page).body;
    for text in [
        "<h2 id=\"brief-heading\">Brief</h2>",
        "Author&#39;s claim",
        "Add the a file",
        "One line\nper letter",
        "Known gaps",
        "No b yet",
    ] {
        assert!(shown.contains(text), "{text}\n{shown}");
    }
    assert!(!shown.contains("id=\"brief-verification\""), "{shown}");
    assert!(!shown.contains("Edit the brief"));
    // The form opens prefilled for an edit.
    let f = bob.get(&form);
    assert!(f.body.contains("Edit the brief of change"), "{}", f.body);
    assert!(f.body.contains(">No b yet</textarea>"), "{}", f.body);

    // A secret is refused on the form, which keeps what was typed.
    let secret = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
    let typed = format!("Ran it\nwith {secret}");
    let r = submit(
        &mut bob,
        &form,
        &form,
        &[("intent", "Add the a file"), ("verification", &typed)],
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("href=\"#verification\""), "{}", r.body);
    assert!(r.body.contains("id=\"verification-error\""), "{}", r.body);
    assert!(r.body.contains("Line 2"), "{}", r.body);
    assert!(r.body.contains("aria-invalid=\"true\""), "{}", r.body);
    assert_eq!(w.list(&w.alice, &id, "").len(), 1);
    // An empty intent too.
    let r = submit(&mut bob, &form, &form, &[("intent", " ")]);
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("id=\"intent-error\""), "{}", r.body);

    // An edit, then a new version: comparing shows how the brief changed.
    let r = submit(
        &mut bob,
        &form,
        &form,
        &[
            ("intent", "Add the a file, carefully"),
            ("gaps", "No b yet"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(carol.get(&page).body.contains("Edited (2 edits)"));
    w.git.ok(
        w.w.path(),
        &["commit", "-q", "--amend", "-m", "Add a, again"],
    );
    w.push("topic");
    w.wait_revisions(2);
    let v2 = carol.get(&page).body;
    assert!(
        !v2.contains("brief-heading"),
        "a new version starts without one"
    );
    assert!(w.write(&w.alice, &id, json!({"schema": "slath-brief/1", "intent": "Add the a file, carefully", "approach": "Only now", "gaps": "b comes next"})).status == 201);
    let cmp = carol.get(&format!("{page}?from=1&rev=2")).body;
    assert!(cmp.contains("Brief changed"), "{cmp}");
    assert!(cmp.contains("<h3>Known gaps</h3>"), "{cmp}");
    assert!(cmp.contains("Removed: </span>No b yet"), "{cmp}");
    assert!(cmp.contains("Added: </span>b comes next"), "{cmp}");
    // Only version 2's brief has an approach: all of it is added.
    assert!(cmp.contains("<h3>Approach</h3>"), "{cmp}");
    assert!(cmp.contains("Added: </span>Only now"), "{cmp}");
    assert!(
        !cmp.contains("<h3>Intent</h3>"),
        "the same intent shows nothing"
    );
    // An older version's page offers no form; a closed merge request none.
    assert!(
        !bob.get(&format!("{page}?rev=1"))
            .body
            .contains("Edit the brief")
    );
    let r = w.api(
        &w.alice,
        "PATCH",
        &format!("/merge-requests/{}", w.n),
        Some(json!({"state": "closed"})),
    );
    assert!(r.status < 300, "{}", r.body);
    assert!(!bob.get(&page).body.contains("Edit the brief"));
    assert_eq!(bob.get(&form).status, 404);
}

/// Starts a thread on `field` of brief edit `brief` as `token`.
fn on_field(w: &World, token: &str, brief: &str, field: &str) -> slath_tests::http::Resp {
    w.api(
        token,
        "POST",
        &format!("/merge-requests/{}/threads", w.n),
        Some(json!({"body": "Which lines?", "brief": {"id": brief, "field": field}})),
    )
}

fn threads(w: &World) -> Vec<Value> {
    let r = w.api(
        &w.alice,
        "GET",
        &format!("/merge-requests/{}/threads", w.n),
        None,
    );
    r.json().as_array().unwrap().clone()
}

#[test]
fn conf_agent_15_threads_anchor_to_brief_fields() {
    let w = World::start();
    let (id, _) = w.change();
    let b = w.write(&w.alice, &id, brief("Add a")).json();
    let brief_id = b["id"].as_str().unwrap().to_owned();

    // A reader comments on a field; the thread keeps the field's text.
    let r = on_field(&w, &w.carol, &brief_id, "approach");
    assert_eq!(r.status, 201, "{}", r.body);
    let t = r.json();
    assert_eq!(t["path"], Value::Null);
    assert_eq!(t["brief"]["id"], brief_id.as_str());
    assert_eq!(t["brief"]["change"], id.as_str());
    assert_eq!(t["brief"]["version"], 1);
    assert_eq!(t["brief"]["field"], "approach");
    assert_eq!(t["brief"]["current"], true);
    let kept = w.s.db.query_one(&format!(
        "SELECT context->>'lines' FROM mr_threads WHERE id = '{}'",
        t["id"].as_str().unwrap()
    ));
    assert_eq!(kept, r#"["Line one", "Line two"]"#);

    // Refused: a field the edit lacks, an unknown field, a path as well,
    // and a brief of another merge request.
    let path = json!({"body": "x", "path": "a.txt", "line_start": 1, "line_end": 1,
                      "brief": {"id": brief_id, "field": "intent"}});
    for r in [
        on_field(&w, &w.carol, &brief_id, "gaps"),
        on_field(&w, &w.carol, &brief_id, "risks"),
        on_field(&w, &w.carol, &uuid::Uuid::now_v7().to_string(), "intent"),
        w.api(
            &w.carol,
            "POST",
            &format!("/merge-requests/{}/threads", w.n),
            Some(path),
        ),
    ] {
        assert_eq!(r.status, 422, "{}", r.body);
        assert!(r.body.contains("\"field\":\"brief\""), "{}", r.body);
    }
    // Who can't read the merge request gets 404.
    let dave = token(&w.s, "dave", &["repo:write", "mrs:write"]);
    assert_eq!(on_field(&w, &dave, &brief_id, "intent").status, 404);
    assert_eq!(threads(&w).len(), 1);

    // A newer edit replaces the brief: the thread is on an earlier one.
    assert_eq!(w.write(&w.bob, &id, brief("Add a, edited")).status, 201);
    assert_eq!(threads(&w)[0]["brief"]["current"], false);
}

#[test]
fn conf_agent_15_brief_fields_take_comments_on_the_change_page() {
    use slath_tests::http::submit;
    let w = World::start();
    let (id, _) = w.change();
    let b = w.write(&w.alice, &id, brief("Add a")).json();
    let brief_id = b["id"].as_str().unwrap().to_owned();
    let tid = on_field(&w, &w.carol, &brief_id, "approach").json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // The conversation locates the thread and quotes the field.
    let mut carol = w.s.login("carol", PASSWORD);
    let conversation = format!("/alice/demo/merge-requests/{}", w.n);
    let conv = carol.get(&conversation).body;
    let short: String = id.chars().take(8).collect();
    assert!(
        conv.contains(&format!("Brief of change {short}, version 1: Approach")),
        "{conv}"
    );
    assert!(
        conv.contains("<pre class=\"thread-quote\">Line one\nLine two</pre>"),
        "{conv}"
    );

    // On the change's page, each field lists its threads and has a
    // Comment link that opens a form under it.
    let page = format!("{conversation}/changes/{id}");
    let shown = carol.get(&page).body;
    assert!(
        shown.contains(&format!("#thread-{tid}\">Thread</a>")),
        "{shown}"
    );
    assert!(
        shown.contains(&format!("{page}?brief_comment=intent#brief-intent")),
        "{shown}"
    );
    assert!(
        !shown.contains("name=\"brief_field\""),
        "no form until opened"
    );
    let opened = format!("{page}?brief_comment=intent");
    assert!(
        carol
            .get(&opened)
            .body
            .contains("name=\"brief_field\" value=\"intent\"")
    );
    let r = submit(
        &mut carol,
        &opened,
        &format!("{conversation}/threads"),
        &[
            ("brief", &brief_id),
            ("brief_field", "intent"),
            ("back", &page),
            ("mode", "single"),
            ("body", "Why a?"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let fields: Vec<String> = threads(&w)
        .iter()
        .filter_map(|t| t["brief"]["field"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(fields, ["approach", "intent"]);
    // Signed out, the page offers no Comment link.
    let anon = w.s.client().get(&page);
    assert!(!anon.body.contains("brief_comment"), "{}", anon.body);

    // Once a newer edit replaces the brief, they're on an earlier one.
    assert_eq!(w.write(&w.bob, &id, brief("Add a, edited")).status, 201);
    assert!(
        carol
            .get(&conversation)
            .body
            .contains("On an earlier brief")
    );
}

#[test]
fn conf_agent_14_a_briefs_verification_shows_the_checks_on_its_commit() {
    let w = World::start();
    let (id, v1) = w.change();
    let r = w.write(&w.alice, &id, brief("Add a"));
    assert_eq!(r.status, 201, "{}", r.body);
    let page = format!("/alice/demo/merge-requests/{}/changes/{id}", w.n);
    let mut carol = w.s.login("carol", PASSWORD);
    let shown = carol.get(&page).body;
    assert!(shown.contains("Ran the tests"), "{shown}");
    assert!(shown.contains("No checks on this commit."), "{shown}");

    let ci = token(&w.s, "bob", &["statuses:write"]);
    let report = |oid: &str, context: &str, state: &str| {
        let r = w.api(
            &ci,
            "POST",
            &format!("/statuses/{oid}"),
            Some(json!({"state": state, "context": context})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
    };
    report(&v1, "ci", "pending");
    report(&v1, "ci", "success");
    report(&v1, "lint", "failure");
    let shown = carol.get(&page).body;
    // The latest status of each context, with its reporter.
    assert!(
        shown.contains("Checks on this commit: <code>ci</code> success by"),
        "{shown}"
    );
    assert!(shown.contains("<code>lint</code> failure by"), "{shown}");
    assert!(!shown.contains("pending"), "{shown}");

    // A new version: its own commit's checks, never the earlier one's.
    w.git.ok(
        w.w.path(),
        &["commit", "-q", "--amend", "-m", "Add a, again"],
    );
    w.push("topic");
    w.wait_revisions(2);
    let (_, v2) = w.change();
    let r = w.write(&w.alice, &id, brief("Version two"));
    assert_eq!(r.status, 201, "{}", r.body);
    let shown = carol.get(&page).body;
    assert!(shown.contains("No checks on this commit."), "{shown}");
    report(&v2, "e2e", "error");
    let shown = carol.get(&page).body;
    assert!(shown.contains("<code>e2e</code> error by"), "{shown}");
    assert!(!shown.contains("<code>lint</code>"), "{shown}");
    let first = carol.get(&format!("{page}?rev=1")).body;
    assert!(first.contains("<code>lint</code> failure by"), "{first}");
    assert!(!first.contains("<code>e2e</code>"), "{first}");
}
