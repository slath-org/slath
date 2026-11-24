// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Agent context records through the API: they bind to a merge request's
//! change and stay with it when the change is rewritten, bind only for
//! uploaders who may update the merge request, honor the repository's
//! detail level, refuse secrets without keeping anything, are seen only
//! by writers, the person who asked, and the bot's owner, and never enter
//! the repository's Git objects.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

struct World {
    s: TestServer,
    alice: String,
    bob: String,
    carol: String,
    bot: String,
    git: GitClient,
    url: String,
    w: tempfile::TempDir,
}

fn token(s: &TestServer, user: &str) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, user, &["repo:write", "mrs:write"])
}

fn lines(tag: &str, edit: bool) -> String {
    (0..20)
        .map(|i| {
            if edit && i == 3 {
                format!("{tag} {i}, edited\n")
            } else {
                format!("{tag} {i}\n")
            }
        })
        .collect()
}

impl World {
    /// alice/demo with bob a writer, carol a reader, alice's bot alice-ci
    /// a writer, and an MR by alice of the stack "Add a", "Add b".
    fn start() -> (Self, i64) {
        let s = TestServer::start();
        s.create_user("bob", PASSWORD, false);
        s.create_user("carol", PASSWORD, false);
        let (ok, out, err) = s.slath(
            &[
                "admin",
                "create-bot",
                "--owner",
                "alice",
                "--name",
                "alice-ci",
                "--token-scopes",
                "repo:write",
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
        let world = Self {
            alice: token(&s, "alice"),
            bob: token(&s, "bob"),
            carol: token(&s, "carol"),
            bot,
            url: s.url(&DEMO),
            s,
            git: GitClient::new(),
            w: tempfile::tempdir().unwrap(),
        };
        for (who, role) in [("bob", "write"), ("carol", "read"), ("alice-ci", "write")] {
            let r = world.api(
                &world.alice,
                "PUT",
                &format!("/collaborators/{who}"),
                Some(json!({"role": role})),
            );
            assert!(r.status < 300, "{}", r.body);
        }
        let w = world.w.path();
        world.git.ok(w, &["init", "-q", "-b", "main"]);
        world.commit("README", "demo\n", "init");
        world.push("main");
        world.git.ok(w, &["switch", "-q", "-c", "topic"]);
        world.commit("a.txt", &lines("a", false), "Add a");
        world.commit("b.txt", &lines("b", false), "Add b");
        world.push("topic");
        let r = world.api(
            &world.alice,
            "POST",
            "/merge-requests",
            Some(json!({"source_branch": "topic", "title": "Add a and b"})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        world.wait_revisions(n, 1);
        (world, n)
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
        let w: &Path = self.w.path();
        std::fs::write(w.join(file), text).unwrap();
        self.git.ok(w, &["add", "-A"]);
        self.git.ok(w, &["commit", "-q", "-m", msg]);
    }

    fn push(&self, branch: &str) {
        self.git
            .ok(self.w.path(), &["push", "-q", "--force", &self.url, branch]);
    }

    fn wait_revisions(&self, n: i64, k: i64) {
        let start = std::time::Instant::now();
        while self
            .api(&self.alice, "GET", &format!("/merge-requests/{n}"), None)
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

    /// The display IDs of the current revision's changes, bottom first.
    fn changes(&self, n: i64) -> Vec<String> {
        self.api(
            &self.alice,
            "GET",
            &format!("/merge-requests/{n}/stack"),
            None,
        )
        .json()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["change_id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// A token of alice's that may manage her bots.
    fn alice_user_token(&self) -> String {
        let mut c = self.s.login("alice", PASSWORD);
        slath_tests::http::create_token(&mut c, "bots", &["user:write"])
    }

    fn upload(&self, token: &str, record: &Value) -> slath_tests::http::Resp {
        self.api(token, "POST", "/context-records", Some(record.clone()))
    }

    fn ids(&self, token: &str, query: &str) -> Vec<String> {
        let r = self.api(token, "GET", &format!("/context-records{query}"), None);
        assert_eq!(r.status, 200, "{}", r.body);
        r.json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The IDs of records that aren't tombstones, newest first.
    fn live_ids(&self, token: &str) -> Vec<String> {
        let r = self.api(token, "GET", "/context-records", None);
        assert_eq!(r.status, 200, "{}", r.body);
        r.json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["deleted"].is_null())
            .map(|i| i["id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Every object in the repository, as `git cat-file` lists them.
    fn objects(&self) -> String {
        self.git.ok(
            &self.s.repo_path(&DEMO),
            &["cat-file", "--batch-all-objects", "--batch-check"],
        )
    }
}

/// A record; a `null` binding is left out.
fn record(summary: &str, binding: Value) -> Value {
    let mut r = json!({
        "schema": "slath-context/1",
        "agent": {"name": "coder", "model": "some-model", "tool": "cli"},
        "initiated_by": "alice",
        "summary": {"intent": summary, "outcome": "Done", "open_items": ["Docs"]},
        "artifacts": [{"name": "report.txt", "sha256": "a".repeat(64)}],
        "redactions": [{"rule": "github-pat", "count": 2}],
        "binding": binding,
    });
    if r["binding"].is_null() {
        r.as_object_mut().unwrap().remove("binding");
    }
    r
}

#[test]
fn conf_agent_05_records_bind_to_changes_and_survive_rewrites() {
    let (w, n) = World::start();
    let before = w.objects();
    let changes = w.changes(n);
    let r = w.upload(
        &w.bot,
        &record(
            "Add a, carefully",
            json!({"merge_request": n, "change": changes[0]}),
        ),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    slath_tests::contract::check("POST", "/api/v1/repos/{ns}/{repo}/context-records", &r);
    let v = r.json();
    let id = v["id"].as_str().unwrap().to_owned();
    assert_eq!(v["uploader"], "alice-ci");
    assert_eq!(v["uploader_is_bot"], true);
    assert_eq!(v["initiated_by"], "alice");
    assert_eq!(v["agent"]["model"], "some-model");
    assert_eq!(v["binding"]["state"], "bound");
    assert_eq!(v["binding"]["merge_request"], n);
    assert_eq!(v["binding"]["change"], changes[0].as_str());
    assert_eq!(v["untrusted_content"], true);
    assert_eq!(v["origin"], "bot");
    assert_eq!(v["summary"]["intent"], "Add a, carefully");
    assert_eq!(v["redactions"][0]["count"], 2);

    // A commit binding finds the change of that commit.
    let head = w
        .git
        .ok(w.w.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let r = w.upload(&w.bot, &record("Add b", json!({"commit": head})));
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["binding"]["change"], changes[1].as_str());
    // A merge request that doesn't exist leaves the record unlinked.
    let r = w.upload(&w.bot, &record("Elsewhere", json!({"merge_request": 99})));
    assert_eq!(r.json()["binding"]["state"], "unlinked");
    let unlinked = r.json()["id"].as_str().unwrap().to_owned();

    // Nothing of it is in the repository's Git objects.
    assert_eq!(w.objects(), before);

    // Rewriting "Add a" keeps the change, and the record stays with it.
    let wd = w.w.path();
    w.git.ok(wd, &["switch", "-q", "--detach", "main"]);
    std::fs::write(wd.join("a.txt"), lines("a", true)).unwrap();
    w.git.ok(wd, &["add", "-A"]);
    w.git.ok(wd, &["commit", "-q", "-m", "Add a"]);
    std::fs::write(wd.join("b.txt"), lines("b", false)).unwrap();
    w.git.ok(wd, &["add", "-A"]);
    w.git.ok(wd, &["commit", "-q", "-m", "Add b"]);
    w.git.ok(wd, &["branch", "-q", "-f", "topic"]);
    w.git.ok(wd, &["switch", "-q", "topic"]);
    w.push("topic");
    w.wait_revisions(n, 2);
    assert_eq!(w.changes(n), changes);
    assert_eq!(
        w.ids(
            &w.alice,
            &format!("?merge_request={n}&change={}", changes[0])
        ),
        std::slice::from_ref(&id)
    );
    let mut all = w.ids(&w.alice, &format!("?merge_request={n}"));
    all.sort();
    assert_eq!(all.len(), 2);
    assert_eq!(w.ids(&w.alice, "?unlinked=true"), [unlinked]);
    let r = w.api(&w.alice, "GET", &format!("/context-records/{id}"), None);
    assert_eq!(r.status, 200, "{}", r.body);
    slath_tests::contract::check("GET", "/api/v1/repos/{ns}/{repo}/context-records/{id}", &r);
    assert_eq!(r.json()["summary"]["open_items"][0], "Docs");
}

#[test]
fn conf_agent_05_only_writers_the_asker_and_the_owner_see_records() {
    let (w, n) = World::start();
    let mut rec = record("Private plan", json!({"merge_request": n}));
    rec["initiated_by"] = json!("carol");
    let r = w.upload(&w.bot, &rec);
    assert_eq!(r.status, 201, "{}", r.body);
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let other = w.upload(&w.bot, &record("Plan", json!({"merge_request": n})));
    let other = other.json()["id"].as_str().unwrap().to_owned();
    // bob (a writer) and alice (the bot's owner) see both.
    for t in [&w.alice, &w.bob] {
        assert_eq!(w.ids(t, "").len(), 2);
        assert_eq!(
            w.api(t, "GET", &format!("/context-records/{other}"), None)
                .status,
            200
        );
    }
    // carol, a reader, sees only the one naming her.
    assert_eq!(w.ids(&w.carol, ""), std::slice::from_ref(&id));
    assert_eq!(
        w.api(&w.carol, "GET", &format!("/context-records/{id}"), None)
            .status,
        200
    );
    let r = w.api(&w.carol, "GET", &format!("/context-records/{other}"), None);
    assert_eq!(r.status, 404, "{}", r.body);
    // Readers can't upload, and anonymous clients see nothing.
    assert_eq!(
        w.upload(&w.carol, &record("x", json!({"merge_request": n})))
            .status,
        403
    );
    let r = w.s.client().api(
        "GET",
        "/api/v1/repos/alice/demo/context-records",
        None,
        None,
    );
    assert_eq!(r.status, 404, "{}", r.body);
    // Naming someone who can't read the repository is refused.
    w.s.create_user("dave", PASSWORD, false);
    let mut rec = record("x", json!({"merge_request": n}));
    rec["initiated_by"] = json!("dave");
    let r = w.upload(&w.bot, &rec);
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("/initiated_by"), "{}", r.body);
}

#[test]
fn conf_agent_05_detail_level_limits_and_schema() {
    let (w, n) = World::start();
    let mut rec = record("With transcript", json!({"merge_request": n}));
    rec["transcript"] = json!([
        {"kind": "prompt", "text": "Fix the bug"},
        {"kind": "tool_call", "text": "cargo test", "tool": "shell"}
    ]);
    let r = w.upload(&w.bot, &rec);
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("/transcript"), "{}", r.body);
    assert!(r.body.contains("summaries only"), "{}", r.body);
    // Only repository admins change the detail level.
    let r = w.s.client().api(
        "PATCH",
        "/api/v1/repos/alice/demo",
        Some(&w.bob),
        Some(&json!({"context_detail": "full"})),
    );
    assert_eq!(r.status, 403, "{}", r.body);
    let r = w.s.client().api(
        "PATCH",
        "/api/v1/repos/alice/demo",
        Some(&w.alice),
        Some(&json!({"context_detail": "full"})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["context_detail"], "full");
    let r = w.upload(&w.bot, &rec);
    assert_eq!(r.status, 201, "{}", r.body);
    let v = r.json();
    assert_eq!(v["detail"], "full");
    assert_eq!(v["transcript_entries"], 2);
    assert_eq!(v["transcript"][1]["tool"], "shell");

    // Unknown schemas and keys, and redaction values, are refused.
    let mut bad = record("x", json!({"merge_request": n}));
    bad["schema"] = json!("slath-context/2");
    let r = w.upload(&w.bot, &bad);
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("/schema"), "{}", r.body);
    let mut bad = record("x", json!({"merge_request": n}));
    bad["redactions"] = json!([{"rule": "r", "count": 1, "value": "hunter2"}]);
    assert_eq!(w.upload(&w.bot, &bad).status, 422);
    // A record over the size limit is refused.
    let big = record(&"x".repeat(2 * 1024 * 1024), json!({"merge_request": n}));
    let r = w.upload(&w.bot, &big);
    assert!(matches!(r.status, 413 | 422), "{} {}", r.status, r.body);
    assert_eq!(w.ids(&w.alice, "").len(), 1);
}

#[test]
fn conf_agent_06_a_record_with_a_secret_is_refused_with_its_location() {
    let (w, n) = World::start();
    let token = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
    let mut rec = record("Deploy", json!({"merge_request": n}));
    rec["summary"]["outcome"] = json!(format!("Used\nthe token {token} to deploy"));
    let r = w.upload(&w.bot, &rec);
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("/summary/outcome"), "{}", r.body);
    assert!(r.body.contains("\"secret\""), "{}", r.body);
    assert!(r.body.contains("Line 2"), "{}", r.body);
    assert!(!r.body.contains(&token), "{}", r.body);
    assert!(w.ids(&w.alice, "").is_empty());
    // The redacted retry is accepted.
    rec["summary"]["outcome"] = json!("Used the token [redacted] to deploy");
    assert_eq!(w.upload(&w.bot, &rec).status, 201);
}

/// Every block rule's sample, in each text field a record or an Entire
/// checkpoint carries it in, is refused with its location and rule, and
/// without the secret; nothing of a refused request is kept, and the
/// redacted retry goes through. A warn-only finding doesn't refuse.
#[test]
fn conf_agent_06_the_secret_corpus_is_refused_on_every_ingest_path() {
    let (w, n) = World::start();
    for body in [
        json!({"context_detail": "full"}),
        json!({"context_entire_import": true}),
    ] {
        let r = w.s.client().api(
            "PATCH",
            "/api/v1/repos/alice/demo",
            Some(&w.alice),
            Some(&body),
        );
        assert_eq!(r.status, 200, "{}", r.body);
    }
    let blobs = || w.s.db.query_one("SELECT count(*)::text FROM blobs");
    let blobs_before = blobs();
    let samples = slath_core::secret_scan::samples();
    let block: Vec<_> = samples
        .iter()
        .filter(|(rule, _)| *rule != "generic-assignment")
        .collect();
    assert!(block.len() >= 10, "{block:?}");
    let fields = [
        "/summary/intent",
        "/summary/outcome",
        "/summary/open_items/0",
        "/transcript/0/text",
    ];
    let refused = |r: &slath_tests::http::Resp, rule: &str, at: &str, text: &str| {
        assert_eq!(r.status, 422, "{rule} in {at}: {}", r.body);
        assert!(r.body.contains("\"secret\""), "{rule}: {}", r.body);
        assert!(r.body.contains(at), "{rule} in {at}: {}", r.body);
        assert!(r.body.contains(&format!("({rule})")), "{rule}: {}", r.body);
        // The excerpt is redacted: no line of the sample comes back whole.
        for line in text.lines().filter(|l| l.len() > 24) {
            assert!(!r.body.contains(line), "{rule}: {}", r.body);
        }
    };
    for (i, (rule, text)) in block.iter().enumerate() {
        // Through the API, in a different field each time.
        let at = fields[i % fields.len()];
        let mut rec = record("Deploy", json!({"merge_request": n}));
        rec["transcript"] = json!([{"kind": "prompt", "text": "Deploy it"}]);
        rec.pointer_mut(at)
            .unwrap()
            .clone_from(&json!(format!("Ran\n{text}")));
        // A retry under the same idempotency key is refused again, not
        // replayed or stored.
        for _ in 0..2 {
            let r = w.s.client().api_with(
                "POST",
                "/api/v1/repos/alice/demo/context-records",
                Some(&w.bot),
                Some(&rec),
                &[("Idempotency-Key", &format!("corpus-{i}"))],
            );
            refused(&r, rule, at, text);
        }

        // Through the Entire import: in the prompt, which the record leaves
        // out, and in the transcript, at the checkpoint's own places.
        let mut session = entire_session(n);
        session["prompt"] = json!(format!("Please\n{text}"));
        let r = w.api(
            &w.bot,
            "POST",
            "/context-records/import/entire",
            Some(session),
        );
        refused(&r, rule, "\"/prompt\"", text);
        let mut session = entire_session(n);
        session["transcript"] = json!(
            json!({"v": 1, "type": "user", "content": [{"text": format!("use\n{text}")}]})
                .to_string()
        );
        let r = w.api(
            &w.bot,
            "POST",
            "/context-records/import/entire",
            Some(session),
        );
        // A secret that shows only once the transcript's lines are decoded
        // (a key across lines) is found in the converted record.
        refused(&r, rule, "\"/transcript", text);
    }
    assert!(w.ids(&w.alice, "").is_empty());
    assert_eq!(blobs(), blobs_before);
    let audits =
        w.s.db
            .query_one("SELECT count(*)::text FROM audit_events WHERE action = 'context.create'");
    assert_eq!(audits, "0");

    // A warn-only finding is kept, like any other text.
    let (_, warn) = samples
        .iter()
        .find(|(rule, _)| *rule == "generic-assignment")
        .unwrap();
    let mut rec = record("Configure", json!({"merge_request": n}));
    rec["summary"]["outcome"] = json!(warn);
    assert_eq!(w.upload(&w.bot, &rec).status, 201);
    // The redacted retry of a refused record is accepted.
    let mut rec = record("Deploy", json!({"merge_request": n}));
    rec["summary"]["outcome"] = json!("Ran\nSLATH_TOKEN=[redacted]");
    assert_eq!(w.upload(&w.bot, &rec).status, 201);
    assert_eq!(w.ids(&w.alice, "").len(), 2);
}

/// The published JSON Schema accepts what the server accepts and refuses
/// what it refuses, for the shapes these tests upload.
#[test]
fn context_record_schema_matches_the_server() {
    let schema: Value =
        serde_json::from_str(include_str!("../../docs/api/context-record.schema.json")).unwrap();
    let v = jsonschema::draft202012::new(&schema).unwrap();
    let mut full = record("x", json!({"merge_request": 1, "change": "k".repeat(32)}));
    full["transcript"] =
        json!([{"kind": "tool_call", "text": "ls", "tool": "shell", "at": "2026-11-23T10:00:00Z"}]);
    for ok in [
        full,
        record("x", json!({"commit": "a".repeat(40)})),
        json!({"schema": "slath-context/1", "agent": {"name": "a"}, "summary": {"intent": "i"}}),
    ] {
        let errors: Vec<String> = v.iter_errors(&ok).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{errors:?}\n{ok}");
    }
    let mut bad = Vec::new();
    for (pointer, value) in [
        ("/schema", json!("slath-context/2")),
        ("/extra", json!(1)),
        (
            "/redactions",
            json!([{"rule": "r", "count": 1, "value": "x"}]),
        ),
        (
            "/binding",
            json!({"merge_request": 1, "commit": "a".repeat(40)}),
        ),
        ("/binding", json!({"change": "k".repeat(32)})),
        ("/summary", json!({"intent": ""})),
    ] {
        let mut r = record("x", json!({"merge_request": 1}));
        let key = pointer.trim_start_matches('/');
        r[key] = value;
        bad.push(r);
    }
    for b in bad {
        assert!(!v.is_valid(&b), "{b}");
    }
}

#[test]
fn conf_agent_05_an_agent_session_trailer_binds_a_record_to_its_change() {
    let (w, n) = World::start();
    let r = w.upload(&w.bot, &record("Add c", json!(null)));
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["binding"]["state"], "none");
    let id = r.json()["id"].as_str().unwrap().to_owned();
    // A second record whose uploader is stopped before the push.
    let r = w.upload(&w.bot, &record("Add d", json!(null)));
    let stopped = r.json()["id"].as_str().unwrap().to_owned();

    let wd = w.w.path();
    std::fs::write(wd.join("c.txt"), lines("c", false)).unwrap();
    w.git.ok(wd, &["add", "-A"]);
    w.git.ok(
        wd,
        &[
            "commit",
            "-q",
            "-m",
            &format!("Add c\n\nAgent-Session: {id}"),
        ],
    );
    w.push("topic");
    w.wait_revisions(n, 2);
    let c = w.changes(n)[2].clone();
    let r = w.api(&w.alice, "GET", &format!("/context-records/{id}"), None);
    assert_eq!(r.json()["binding"]["state"], "bound", "{}", r.body);
    assert_eq!(r.json()["binding"]["change"], c.as_str());

    let s = w.s.client().api(
        "POST",
        "/api/v1/bots/alice-ci/stop",
        Some(&w.alice_user_token()),
        None,
    );
    assert_eq!(s.status, 200, "{}", s.body);
    std::fs::write(wd.join("d.txt"), lines("d", false)).unwrap();
    w.git.ok(wd, &["add", "-A"]);
    w.git.ok(
        wd,
        &[
            "commit",
            "-q",
            "-m",
            &format!("Add d\n\nAgent-Session: {stopped}"),
        ],
    );
    w.push("topic");
    w.wait_revisions(n, 3);
    let r = w.api(
        &w.alice,
        "GET",
        &format!("/context-records/{stopped}"),
        None,
    );
    assert_eq!(r.json()["binding"]["state"], "none", "{}", r.body);
}

/// One session as the Entire CLI 0.13 writes it.
fn entire_session(n: i64) -> Value {
    let transcript = [
        json!({"v": 1, "type": "user", "ts": "2026-11-20T10:00:00Z", "content": [{"text": "Fix the parser"}]}),
        json!({"v": 1, "type": "assistant", "ts": "2026-11-20T10:00:05Z", "content": [
            {"type": "text", "text": "Running the tests."},
            {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cargo test"},
             "result": {"output": "test result: ok", "status": "success"}}
        ]}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    json!({
        "session": {
            "cli_version": "0.13.4", "checkpoint_id": "a1b2c3d4e5f6", "session_id": "s-1",
            "strategy": "manual-commit", "created_at": "2026-11-20T10:00:00Z",
            "agent": "Claude Code", "model": "some-model", "files_touched": ["a.txt"],
            "summary": {"intent": "Fix the parser", "outcome": "Fixed", "open_items": [], "friction": []},
            "redaction": {"replacements": 1, "layers": {"entropy": 1}}
        },
        "transcript": transcript,
        "prompt": "Fix the parser",
        "binding": {"merge_request": n}
    })
}

#[test]
fn conf_agent_05_entire_checkpoints_import_when_turned_on() {
    let (w, n) = World::start();
    let import = |body: &Value| {
        w.api(
            &w.bot,
            "POST",
            "/context-records/import/entire",
            Some(body.clone()),
        )
    };
    // Off by default: the import isn't there.
    assert_eq!(import(&entire_session(n)).status, 404);
    let patch = |token: &str, body: Value| {
        w.s.client().api(
            "PATCH",
            "/api/v1/repos/alice/demo",
            Some(token),
            Some(&body),
        )
    };
    assert_eq!(
        patch(&w.bob, json!({"context_entire_import": true})).status,
        403
    );
    let r = patch(&w.alice, json!({"context_entire_import": true}));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["context_entire_import"], true);

    // At the summary level, the transcript is left out.
    let r = import(&entire_session(n));
    assert_eq!(r.status, 201, "{}", r.body);
    slath_tests::contract::check(
        "POST",
        "/api/v1/repos/{ns}/{repo}/context-records/import/entire",
        &r,
    );
    let v = r.json();
    assert_eq!(v["agent"]["name"], "Claude Code");
    assert_eq!(v["agent"]["tool"], "entire 0.13.4");
    assert_eq!(v["summary"]["intent"], "Fix the parser");
    assert_eq!(v["redactions"][0]["rule"], "entire-entropy");
    assert_eq!(v["transcript_entries"], 0);
    assert_eq!(v["binding"]["state"], "bound");

    // With full, it comes along, and a secret in it is refused in place.
    assert_eq!(
        patch(&w.alice, json!({"context_detail": "full"})).status,
        200
    );
    let r = import(&entire_session(n));
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["transcript_entries"], 4);
    assert_eq!(r.json()["transcript"][2]["tool"], "Bash");
    let mut leaky = entire_session(n);
    let token = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
    leaky["transcript"] = json!(format!(
        "{}",
        json!({"v": 1, "type": "user", "content": [{"text": format!("use {token}")}]})
    ));
    let r = import(&leaky);
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("\"/transcript\""), "{}", r.body);
    assert!(!r.body.contains(&token), "{}", r.body);
    // Readers can't import, and the request has a fixed shape.
    assert_eq!(
        w.api(
            &w.carol,
            "POST",
            "/context-records/import/entire",
            Some(entire_session(n))
        )
        .status,
        403
    );
    let mut odd = entire_session(n);
    odd["extra"] = json!(1);
    assert_eq!(import(&odd).status, 422);
}

#[test]
fn conf_agent_16_context_refs_are_refused_with_a_hint() {
    let (w, _) = World::start();
    let wd = w.w.path();
    let refused = |refspec: &str| {
        let out = w
            .git
            .cmd(wd)
            .args(["push", &w.url, refspec])
            .output()
            .unwrap();
        (
            !out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    for r in [
        "HEAD:refs/context/abc",
        "HEAD:refs/entire/checkpoints/ab/a1b2c3d4e5ab",
    ] {
        let (no, err) = refused(r);
        assert!(no, "{r}");
        assert!(
            err.contains("agent context isn't kept in Git here"),
            "{err}"
        );
        assert!(err.contains("/context-records"), "{err}");
    }
    // Entire's old checkpoint branch is a branch like any other, until the
    // repository imports Entire checkpoints.
    assert!(!refused("HEAD:refs/heads/entire/checkpoints/v1").0);
    let r = w.s.client().api(
        "PATCH",
        "/api/v1/repos/alice/demo",
        Some(&w.alice),
        Some(&json!({"context_entire_import": true})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let (no, err) = refused("+HEAD~1:refs/heads/entire/checkpoints/v1");
    assert!(no);
    assert!(err.contains("import/entire"), "{err}");
}

#[test]
fn conf_agent_05_a_records_page_pages_its_transcript_for_those_who_may_see_it() {
    let (w, n) = World::start();
    let r = w.s.client().api(
        "PATCH",
        "/api/v1/repos/alice/demo",
        Some(&w.alice),
        Some(&json!({"context_detail": "full"})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let long: String = (0..60).map(|i| format!("line {i}\n")).collect();
    let mut entries: Vec<Value> = (1..=250)
        .map(|i| json!({"kind": "response", "text": format!("Entry text {i}")}))
        .collect();
    entries[0] = json!({"kind": "prompt", "text": "Do the thing"});
    entries[1] = json!({"kind": "tool_result", "text": long, "tool": "shell"});
    let mut rec = record("Paged", json!({"merge_request": n}));
    rec["transcript"] = json!(entries);
    let r = w.upload(&w.bot, &rec);
    assert_eq!(r.status, 201, "{}", r.body);
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let page = format!("/alice/demo/-/context/{id}");

    let mut bob = w.s.login("bob", PASSWORD);
    let first = bob.get(&page);
    assert_eq!(first.status, 200, "{}", first.body);
    assert!(
        first.body.contains("Entries 1–200 of 250."),
        "{}",
        first.body
    );
    assert!(first.body.contains("Do the thing"));
    assert!(first.body.contains("Entry text 200"));
    assert!(!first.body.contains("Entry text 201"));
    // A long entry shows its first lines; the rest folds away.
    assert!(first.body.contains("Show 40 more lines"), "{}", first.body);
    assert!(first.body.contains(&format!("{page}?page=2")));
    let second = bob.get(&format!("{page}?page=2"));
    assert!(
        second.body.contains("Entries 201–250 of 250."),
        "{}",
        second.body
    );
    assert!(second.body.contains("Entry text 250"));
    // The panel shows the prompt and links here, not the transcript.
    let mr = bob.get(&format!("/alice/demo/merge-requests/{n}"));
    assert!(
        mr.body.contains("Asked: <bdi>Do the thing</bdi>"),
        "{}",
        mr.body
    );
    assert!(mr.body.contains("Read the transcript (250 entries)"));
    assert!(!mr.body.contains("Entry text 2"));

    // A reader the record doesn't name, and anonymous visitors, get 404.
    let mut carol = w.s.login("carol", PASSWORD);
    assert_eq!(carol.get(&page).status, 404);
    assert!(
        !carol
            .get(&format!("/alice/demo/merge-requests/{n}"))
            .body
            .contains("side-context")
    );
    assert_eq!(w.s.client().get(&page).status, 404);
    assert_eq!(bob.get("/alice/demo/-/context/not-a-uuid").status, 404);
}

impl World {
    fn patch_repo(&self, token: &str, body: Value) -> slath_tests::http::Resp {
        self.s.client().api(
            "PATCH",
            "/api/v1/repos/alice/demo",
            Some(token),
            Some(&body),
        )
    }

    fn mcp_list(&self, token: &str, n: i64) -> String {
        let r = self.s.client().api_with(
            "POST",
            "/api/mcp",
            Some(token),
            Some(
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "list_context_records",
                "arguments": {"ns": "alice", "repo": "demo", "merge_request": n}}}),
            ),
            &[
                ("accept", "application/json, text/event-stream"),
                ("mcp-protocol-version", "2025-06-18"),
            ],
        );
        assert_eq!(r.status, 200, "{}", r.body);
        r.body
    }
}

/// With the default visibility, a reader the record doesn't name finds
/// nothing of it on any surface, and nothing of it is reachable over Git.
#[test]
fn conf_agent_16_other_readers_find_nothing_on_any_surface() {
    let (w, n) = World::start();
    let marker = "Zebracorn-7f3a plan";
    let mut rec = record(marker, json!({"merge_request": n}));
    rec["summary"]["outcome"] = json!(format!("{marker} done"));
    let r = w.upload(&w.bot, &rec);
    assert_eq!(r.status, 201, "{}", r.body);
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let sha = w.s.db.query_one(&format!(
        "SELECT payload_sha256 FROM context_records WHERE id = '{id}'"
    ));
    // Writers see it (the panel and the MCP list), so the checks below
    // look in the right places.
    let mut bob = w.s.login("bob", PASSWORD);
    assert!(
        bob.get(&format!("/alice/demo/merge-requests/{n}"))
            .body
            .contains(marker)
    );
    assert!(w.mcp_list(&w.bob, n).contains(&id));

    let mut carol = w.s.login("carol", PASSWORD);
    let ui = [
        format!("/alice/demo/merge-requests/{n}"),
        format!("/alice/demo/-/context/{id}"),
        format!("/search?q={}", marker.replace(' ', "+")),
        "/notifications".to_owned(),
        "/".to_owned(),
        "/alice/demo".to_owned(),
    ];
    for page in &ui {
        let r = carol.get(page);
        assert!(!r.body.contains(marker), "{page}: {}", r.body);
        if page.contains(&id) {
            // The error page shows the address asked for, nothing more.
            assert_eq!(r.status, 404, "{page}");
        } else {
            assert!(!r.body.contains(&id), "{page}");
        }
    }
    for path in [
        format!("/context-records/{id}"),
        "/context-records".to_owned(),
        format!("/context-records?merge_request={n}"),
        format!("/merge-requests/{n}"),
        format!("/merge-requests/{n}/timeline"),
    ] {
        let r = w.api(&w.carol, "GET", &path, None);
        assert!(!r.body.contains(marker), "{path}: {}", r.body);
        assert!(!r.body.contains(&id), "{path}: {}", r.body);
    }
    let listed = w.mcp_list(&w.carol, n);
    assert!(
        !listed.contains(&id) && !listed.contains(marker),
        "{listed}"
    );
    // Nothing went out by webhook or notification, and the audit log names
    // the record without its text.
    for table in ["webhook_deliveries", "notifications", "audit_events"] {
        let hits = w.s.db.query_one(&format!(
            "SELECT count(*)::text FROM {table} t WHERE t::text LIKE '%Zebracorn%'"
        ));
        assert_eq!(hits, "0", "{table}");
    }

    // Git: the record's ID, its payload's digest, and the Git object IDs
    // its ciphertext and a text of it would have are unknown to the
    // repository, over protocol v0 and v2, as a pushed ref, and as a
    // claimed have.
    let blob = w.s.data_dir().join("blobs").join(&sha[..2]).join(&sha);
    let wd = w.w.path();
    let hash = |path: &Path| {
        w.git
            .ok(wd, &["hash-object", path.to_str().unwrap()])
            .trim()
            .to_owned()
    };
    let text = w.w.path().join("text.tmp");
    std::fs::write(&text, format!("{marker} done")).unwrap();
    let oids = [
        hash(&blob),
        hash(&text),
        sha[..40].to_owned(),
        id.replace('-', "")[..32].to_owned() + "00000000",
    ];
    std::fs::remove_file(&text).unwrap();
    let objects = w.objects();
    for oid in &oids {
        assert!(!objects.contains(oid.as_str()), "{oid}");
        for proto in ["0", "2"] {
            let out = w
                .git
                .cmd(wd)
                .args([
                    "-c",
                    &format!("protocol.version={proto}"),
                    "fetch",
                    &w.url,
                    oid,
                ])
                .output()
                .unwrap();
            assert!(!out.status.success(), "v{proto} fetched {oid}");
        }
        let out = w
            .git
            .cmd(wd)
            .args(["push", &w.url, &format!("{oid}:refs/heads/leak")])
            .output()
            .unwrap();
        assert!(!out.status.success(), "pushed a ref to {oid}");
    }
    // A commit locally holding the ciphertext, offered as a have, gets
    // nothing from the server but its own objects.
    let leak = wd.join("leak.bin");
    std::fs::copy(&blob, &leak).unwrap();
    w.git.ok(wd, &["add", "leak.bin"]);
    w.git.ok(wd, &["commit", "-q", "-m", "local only"]);
    let clone = tempfile::tempdir().unwrap();
    w.git
        .ok(clone.path(), &["clone", "-q", "--mirror", &w.url, "m.git"]);
    let mirrored = w.git.ok(
        &clone.path().join("m.git"),
        &["cat-file", "--batch-all-objects", "--batch-check"],
    );
    assert!(!mirrored.contains(&oids[0]), "{mirrored}");
    w.git.ok(wd, &["fetch", "-q", &w.url, "main"]);
    assert_eq!(w.objects(), objects);
}

#[test]
fn conf_agent_16_readers_follow_the_code_and_off_refuses_ingest() {
    let (w, n) = World::start();
    let r = w.upload(&w.bot, &record("Plan", json!({"merge_request": n})));
    let id = r.json()["id"].as_str().unwrap().to_owned();
    assert_eq!(
        w.api(&w.alice, "GET", "", None).json()["context_visibility"],
        "writers"
    );
    // Only repository admins change it, and only to known values.
    assert_eq!(
        w.patch_repo(&w.bob, json!({"context_visibility": "readers"}))
            .status,
        403
    );
    let r = w.patch_repo(&w.alice, json!({"context_visibility": "everyone"}));
    assert_eq!(r.status, 422, "{}", r.body);
    let r = w.patch_repo(&w.alice, json!({"context_visibility": "readers"}));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["context_visibility"], "readers");
    assert_eq!(w.ids(&w.carol, ""), std::slice::from_ref(&id));
    let mut carol = w.s.login("carol", PASSWORD);
    assert_eq!(
        carol.get(&format!("/alice/demo/-/context/{id}")).status,
        200
    );
    // Never more than the code: someone who can't read it still gets 404.
    w.s.create_user("dave", PASSWORD, false);
    let dave = token(&w.s, "dave");
    assert_eq!(
        w.api(&dave, "GET", &format!("/context-records/{id}"), None)
            .status,
        404
    );
    assert_eq!(w.api(&dave, "GET", "/context-records", None).status, 404);
    assert_eq!(
        w.s.client()
            .get(&format!("/alice/demo/-/context/{id}"))
            .status,
        404
    );
    // Readers still can't upload.
    assert_eq!(w.upload(&w.carol, &record("x", json!(null))).status, 403);

    // Off: no new records through any path; existing ones stay as with
    // writers, so they can be read and deleted.
    let r = w.patch_repo(
        &w.alice,
        json!({"context_visibility": "off", "context_entire_import": true}),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let r = w.upload(&w.bot, &record("More", json!({"merge_request": n})));
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("context_off"), "{}", r.body);
    let r = w.api(
        &w.bot,
        "POST",
        "/context-records/import/entire",
        Some(entire_session(n)),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("context_off"), "{}", r.body);
    assert_eq!(w.ids(&w.bob, ""), std::slice::from_ref(&id));
    assert!(w.ids(&w.carol, "").is_empty());
    assert_eq!(
        w.s.db.query_one(
            "SELECT data->>'context_visibility' FROM audit_events
              WHERE action = 'repo.context_settings' ORDER BY created_at DESC LIMIT 1"
        ),
        "off"
    );
}

#[test]
fn conf_agent_16_the_asker_the_owner_and_admins_delete_records() {
    let (w, n) = World::start();
    let mut rec = record("For carol", json!({"merge_request": n}));
    rec["initiated_by"] = json!("carol");
    let mine = w.upload(&w.bot, &rec).json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other = w
        .upload(&w.bot, &record("Other", json!({"merge_request": n})))
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sha = w.s.db.query_one(&format!(
        "SELECT payload_sha256 FROM context_records WHERE id = '{mine}'"
    ));
    let del = |t: &str, id: &str| w.api(t, "DELETE", &format!("/context-records/{id}"), None);
    // bob sees both as a writer but neither asked nor owns the bot.
    assert_eq!(del(&w.bob, &other).status, 403);
    // carol may delete the one she asked for; the other is invisible to her.
    assert_eq!(del(&w.carol, &other).status, 404);
    let r = del(&w.carol, &mine);
    assert_eq!(r.status, 204, "{}", r.body);
    // A tombstone stays as it is.
    assert_eq!(del(&w.carol, &mine).status, 204);
    let t = w.api(&w.alice, "GET", &format!("/context-records/{mine}"), None);
    assert_eq!(t.status, 200, "{}", t.body);
    assert_eq!(t.json()["deleted"]["by"], "carol");
    // The sealed key and the claimed fields are gone at once; the blob is
    // released.
    assert_eq!(
        w.s.db.query_one(&format!(
            "SELECT (agent_name IS NULL AND payload_key_enc IS NULL AND payload_sha256 IS NULL)::text
               FROM context_records WHERE id = '{mine}'"
        )),
        "true"
    );
    assert_eq!(
        w.s.db.query_one(&format!(
            "SELECT refcount::text FROM blobs WHERE sha256 = '{sha}'"
        )),
        "0"
    );
    assert_eq!(
        w.s.db.query_one(&format!(
            "SELECT count(*)::text FROM audit_events WHERE action = 'context.delete' AND target_id = '{mine}'"
        )),
        "1"
    );
    // alice owns the bot and administers the repository.
    assert_eq!(del(&w.alice, &other).status, 204);
    assert!(w.live_ids(&w.alice).is_empty());
    assert_eq!(del(&w.alice, "not-a-uuid").status, 404);
}

#[test]
fn conf_agent_16_records_past_the_retention_are_deleted() {
    let (mut w, n) = World::start();
    let old = w
        .upload(&w.bot, &record("Old", json!({"merge_request": n})))
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let new = w
        .upload(&w.bot, &record("New", json!({"merge_request": n})))
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for bad in [json!(0), json!(3651), json!("x")] {
        let r = w.patch_repo(&w.alice, json!({"context_retention_days": bad}));
        assert_eq!(r.status, 422, "{bad}: {}", r.body);
    }
    let r = w.patch_repo(&w.alice, json!({"context_retention_days": 7}));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["context_retention_days"], 7);
    w.s.db.execute(&format!(
        "UPDATE context_records SET created_at = now() - interval '8 days' WHERE id = '{old}'"
    ));
    // The hourly maintenance runs at start.
    w.s.stop();
    w.s.start_again(&slath_tests::server::slath_bin()).unwrap();
    let start = std::time::Instant::now();
    while w.live_ids(&w.alice).len() != 1 {
        assert!(start.elapsed().as_secs() < 30, "{}", w.s.log());
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert_eq!(w.live_ids(&w.alice), vec![new]);
    let t = w.api(&w.alice, "GET", &format!("/context-records/{old}"), None);
    assert_eq!(t.json()["deleted"]["retention"], true, "{}", t.body);
    assert_eq!(t.json()["deleted"]["by"], Value::Null);
    let repo =
        w.s.db
            .query_one("SELECT id::text FROM repositories WHERE name = 'demo'");
    assert_eq!(
        w.s.db.query_one(&format!(
            "SELECT data->>'count' FROM audit_events WHERE action = 'context.expire' AND target_id = '{repo}'"
        )),
        "1"
    );
    // null keeps records again.
    let r = w.patch_repo(&w.alice, json!({"context_retention_days": null}));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["context_retention_days"], Value::Null);
    let r = w.patch_repo(&w.alice, json!({"context_detail": "summary"}));
    assert_eq!(r.json()["context_retention_days"], Value::Null);
}

#[test]
fn conf_agent_16_settings_and_deletion_in_the_ui() {
    use slath_tests::http::submit;
    let (w, n) = World::start();
    let general = "/alice/demo/settings/general";
    let action = "/alice/demo/settings/context";
    let mut alice = w.s.login("alice", PASSWORD);
    let page = alice.get(general);
    assert!(page.body.contains("Agent context"), "{}", page.body);
    assert!(
        page.body.contains(r#"value="writers" checked"#),
        "{}",
        page.body
    );
    fn form<'a>(v: &'a str, d: &'a str, days: &'a str, entire: bool) -> Vec<(&'a str, &'a str)> {
        let mut f = vec![("visibility", v), ("detail", d), ("retention_days", days)];
        if entire {
            f.push(("entire_import", "1"));
        }
        f
    }
    let r = submit(
        &mut alice,
        general,
        action,
        &form("readers", "full", "30", true),
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        r.header("location"),
        Some("/alice/demo/settings/general?status=context")
    );
    let repo = w.api(&w.alice, "GET", "", None).json();
    assert_eq!(repo["context_visibility"], "readers");
    assert_eq!(repo["context_detail"], "full");
    assert_eq!(repo["context_retention_days"], 30);
    assert_eq!(repo["context_entire_import"], true);
    assert!(
        alice
            .get(&format!("{general}?status=context"))
            .body
            .contains("Agent context settings saved.")
    );
    // A retention out of range is explained in place; empty keeps records.
    let r = submit(
        &mut alice,
        general,
        action,
        &form("readers", "full", "0", true),
    );
    assert_eq!(r.status, 422);
    assert!(
        r.body.contains("Keep records for 1 to 3650 days"),
        "{}",
        r.body
    );
    let r = submit(
        &mut alice,
        general,
        action,
        &form("writers", "summary", "", false),
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let repo = w.api(&w.alice, "GET", "", None).json();
    assert_eq!(repo["context_retention_days"], Value::Null);
    assert_eq!(repo["context_visibility"], "writers");
    assert_eq!(repo["context_entire_import"], false);
    // Writers who aren't admins don't get the page; it needs sudo.
    let mut bob = w.s.login("bob", PASSWORD);
    assert_eq!(bob.get(general).status, 404);
    w.s.db.execute("UPDATE sessions SET sudo_until = NULL");
    let r = submit(
        &mut alice,
        general,
        action,
        &form("readers", "summary", "", false),
    );
    assert_eq!(r.status, 303);
    assert!(r.header("location").unwrap().starts_with("/-/sudo"));
    assert_eq!(
        w.api(&w.alice, "GET", "", None).json()["context_visibility"],
        "writers"
    );

    // Deletion from the record's page: offered to the person who asked,
    // not to a writer who neither asked nor owns the bot.
    let mut rec = record("For carol", json!({"merge_request": n}));
    rec["initiated_by"] = json!("carol");
    let id = w.upload(&w.bot, &rec).json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let page = format!("/alice/demo/-/context/{id}");
    assert!(!bob.get(&page).body.contains("Delete record"));
    let mut carol = w.s.login("carol", PASSWORD);
    assert!(carol.get(&page).body.contains("Delete record"));
    let r = submit(&mut bob, &page, &format!("{page}/delete"), &[]);
    assert_eq!(r.status, 403, "{}", r.body);
    let r = submit(&mut carol, &page, &format!("{page}/delete"), &[]);
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        r.header("location"),
        Some(format!("/alice/demo/merge-requests/{n}").as_str())
    );
    let gone = carol.get(&page);
    assert_eq!(gone.status, 200, "{}", gone.body);
    assert!(gone.body.contains("Deleted by carol"), "{}", gone.body);
    assert!(!gone.body.contains("Delete record"));
}

impl World {
    fn mcp_call(&self, token: &str, tool: &str, args: Value) -> String {
        let mut arguments = json!({"ns": "alice", "repo": "demo"});
        arguments
            .as_object_mut()
            .unwrap()
            .extend(args.as_object().unwrap().clone());
        let r = self.s.client().api_with(
            "POST",
            "/api/mcp",
            Some(token),
            Some(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": {"name": tool, "arguments": arguments}})),
            &[
                ("accept", "application/json, text/event-stream"),
                ("mcp-protocol-version", "2025-06-18"),
            ],
        );
        assert_eq!(r.status, 200, "{}", r.body);
        r.body
    }

    /// Waits until record `id` is a tombstone.
    fn wait_tombstone(&self, id: &str) -> Value {
        let start = std::time::Instant::now();
        loop {
            let r = self.api(&self.alice, "GET", &format!("/context-records/{id}"), None);
            if !r.json()["deleted"].is_null() {
                return r.json();
            }
            assert!(start.elapsed().as_secs() < 30, "{}", self.s.log());
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

/// A deleted record leaves a tombstone that says it existed, what it was
/// about, and who deleted it, and holds none of its text anywhere.
#[test]
fn conf_agent_21_tombstones_keep_no_text_on_any_surface() {
    let (w, n) = World::start();
    let marker = "Quokkaflint-91c";
    let mut rec = record(&format!("{marker} intent"), json!({"merge_request": n}));
    rec["agent"] = json!({"name": format!("{marker} agent"), "model": format!("{marker} model"), "tool": format!("{marker} tool")});
    rec["summary"]["outcome"] = json!(format!("{marker} outcome"));
    rec["summary"]["open_items"] = json!([format!("{marker} item")]);
    rec["artifacts"] = json!([{"name": format!("{marker}.txt"), "sha256": "b".repeat(64)}]);
    rec["redactions"] = json!([{"rule": format!("{marker}-rule"), "count": 1}]);
    let r = w.upload(&w.bot, &rec);
    assert_eq!(r.status, 201, "{}", r.body);
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let digest = w.s.db.query_one(&format!(
        "SELECT payload_sha256 FROM context_records WHERE id = '{id}'"
    ));
    let mut bob = w.s.login("bob", PASSWORD);
    let mr_page = format!("/alice/demo/merge-requests/{n}");
    assert!(bob.get(&mr_page).body.contains(marker));

    let r = w.api(&w.alice, "DELETE", &format!("/context-records/{id}"), None);
    assert_eq!(r.status, 204, "{}", r.body);

    // The API, by ID and in lists: the tombstone, without anything claimed.
    let t = w.api(&w.bob, "GET", &format!("/context-records/{id}"), None);
    assert_eq!(t.status, 200, "{}", t.body);
    let v = t.json();
    assert_eq!(v["deleted"]["by"], "alice");
    assert_eq!(v["deleted"]["retention"], false);
    assert_eq!(v["agent"], Value::Null);
    assert_eq!(v["summary"], Value::Null);
    assert_eq!(v["transcript"], Value::Null);
    assert_eq!(v["binding"]["merge_request"], n);
    assert_eq!(v["uploader"], "alice-ci");
    assert_eq!(v["initiated_by"], "alice");
    assert_eq!(v["payload_sha256"], digest.as_str());
    for path in [
        format!("/context-records/{id}"),
        "/context-records".to_owned(),
        format!("/context-records?merge_request={n}"),
    ] {
        let r = w.api(&w.bob, "GET", &path, None);
        assert!(r.body.contains(&id), "{path}: {}", r.body);
        assert!(!r.body.contains(marker), "{path}: {}", r.body);
    }
    // MCP.
    let listed = w.mcp_call(&w.bob, "list_context_records", json!({"merge_request": n}));
    assert!(listed.contains(&id) && !listed.contains(marker), "{listed}");
    let got = w.mcp_call(&w.bob, "get_context_record", json!({"id": id}));
    assert!(got.contains("deleted") && !got.contains(marker), "{got}");
    // The UI: one line in the panel, and the record's page.
    let mr = bob.get(&mr_page).body;
    assert!(!mr.contains(marker), "{mr}");
    assert!(mr.contains("Deleted by alice"), "{mr}");
    let page = bob.get(&format!("/alice/demo/-/context/{id}"));
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(!page.body.contains(marker), "{}", page.body);
    assert!(page.body.contains("Deleted by alice"), "{}", page.body);
    assert!(page.body.contains(&digest[..12]), "{}", page.body);
    // The database keeps no text of it, and the payload's blob is released.
    for table in ["context_records", "audit_events"] {
        let hits = w.s.db.query_one(&format!(
            "SELECT count(*)::text FROM {table} t WHERE t::text LIKE '%Quokkaflint%'"
        ));
        assert_eq!(hits, "0", "{table}");
    }
    assert_eq!(
        w.s.db.query_one(&format!(
            "SELECT refcount::text FROM blobs WHERE sha256 = '{digest}'"
        )),
        "0"
    );
}

/// A tombstone is seen by exactly those who could see the record.
#[test]
fn conf_agent_21_tombstones_are_seen_as_their_records_were() {
    let (w, n) = World::start();
    let mut rec = record("For carol", json!({"merge_request": n}));
    rec["initiated_by"] = json!("carol");
    let hers = w.upload(&w.bot, &rec).json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other = w
        .upload(
            &w.bot,
            &record("Not for carol", json!({"merge_request": n})),
        )
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for id in [&hers, &other] {
        let r = w.api(&w.alice, "DELETE", &format!("/context-records/{id}"), None);
        assert_eq!(r.status, 204, "{}", r.body);
    }
    // carol, a reader, sees the tombstone of the record she asked for.
    assert_eq!(w.ids(&w.carol, ""), std::slice::from_ref(&hers));
    let get = |t: &str, id: &str| {
        w.api(t, "GET", &format!("/context-records/{id}"), None)
            .status
    };
    assert_eq!(get(&w.carol, &hers), 200);
    assert_eq!(get(&w.carol, &other), 404);
    let mut carol = w.s.login("carol", PASSWORD);
    assert_eq!(
        carol.get(&format!("/alice/demo/-/context/{hers}")).status,
        200
    );
    assert_eq!(
        carol.get(&format!("/alice/demo/-/context/{other}")).status,
        404
    );
    let mr = carol.get(&format!("/alice/demo/merge-requests/{n}")).body;
    assert!(mr.contains(&hers) && !mr.contains(&other), "{mr}");
    // Signed out on a private repository: nothing.
    assert_eq!(
        w.s.client()
            .get(&format!("/alice/demo/-/context/{hers}"))
            .status,
        404
    );
    // Nobody can delete a tombstone into something else.
    assert_eq!(
        w.api(
            &w.carol,
            "DELETE",
            &format!("/context-records/{hers}"),
            None
        )
        .status,
        204
    );
    assert_eq!(
        w.s.db.query_one(&format!(
            "SELECT (deleted_by = (SELECT u.id FROM users u JOIN namespaces n ON n.id = u.namespace_id WHERE n.name = 'alice'))::text FROM context_records WHERE id = '{hers}'"
        )),
        "true"
    );
}

/// Expiring context removes no evidence: reviews, statuses, merge
/// requests' timelines, and audit entries stay.
#[test]
fn conf_agent_21_expiring_context_removes_no_evidence() {
    let (mut w, n) = World::start();
    let id = w
        .upload(&w.bot, &record("Old", json!({"merge_request": n})))
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = w.api(
        &w.bob,
        "POST",
        &format!("/merge-requests/{n}/reviews"),
        Some(json!({"verdict": "approve"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let mut c = w.s.login("alice", PASSWORD);
    let statuses = slath_tests::http::create_token(&mut c, "ci", &["statuses:write"]);
    let head = w
        .api(&w.alice, "GET", &format!("/merge-requests/{n}"), None)
        .json()["head_oid"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = w.api(
        &statuses,
        "POST",
        &format!("/statuses/{head}"),
        Some(json!({"state": "success", "context": "ci/build"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let r = w.patch_repo(&w.alice, json!({"context_retention_days": 1}));
    assert_eq!(r.status, 200, "{}", r.body);
    // What exists now, counted again after the expiry (audit entries
    // written since, such as the expiry's own, don't count).
    let cut = w.s.db.query_one("SELECT now()::text");
    let evidence = |w: &World| {
        w.s.db.query_one(&format!(
            "SELECT (SELECT count(*) FROM mr_reviews)::text || ' ' ||
                    (SELECT count(*) FROM commit_statuses)::text || ' ' ||
                    (SELECT count(*) FROM timeline_events WHERE created_at <= '{cut}')::text || ' ' ||
                    (SELECT count(*) FROM audit_events WHERE created_at <= '{cut}')::text"
        ))
    };
    let before_settings = evidence(&w);
    assert!(before_settings.starts_with("1 1 "), "{before_settings}");
    w.s.db.execute(&format!(
        "UPDATE context_records SET created_at = now() - interval '2 days' WHERE id = '{id}'"
    ));
    w.s.stop();
    w.s.start_again(&slath_tests::server::slath_bin()).unwrap();
    let t = w.wait_tombstone(&id);
    assert_eq!(t["deleted"]["retention"], true);
    assert_eq!(evidence(&w), before_settings);
    // The record's own upload stays in the audit log.
    assert_eq!(
        w.s.db.query_one(&format!(
            "SELECT count(*)::text FROM audit_events WHERE action = 'context.create' AND data->>'record' = '{id}'"
        )),
        "1"
    );
}
