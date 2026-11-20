// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reviews of one change: a verdict on a change's version in the current
//! revision, which counts for that change only and goes stale only when
//! that change's patch changes.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

struct World {
    s: TestServer,
    alice: String,
    bob: String,
    git: GitClient,
    url: String,
    w: tempfile::TempDir,
}

/// Twenty numbered lines of `tag`, with line `edit` changed: an edit
/// small enough to keep the change.
fn lines(tag: &str, edit: Option<usize>) -> String {
    (0..20)
        .map(|i| {
            if Some(i) == edit {
                format!("{tag} {i}, edited\n")
            } else {
                format!("{tag} {i}\n")
            }
        })
        .collect()
}

fn token(s: &TestServer, user: &str) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, user, &["repo:write", "mrs:write"])
}

impl World {
    /// alice's repository, bob a writer, main protected with one required
    /// approval, and an MR of a stack "Add a", "Add b".
    fn start(protection: Value) -> (Self, i64) {
        let s = TestServer::start();
        s.create_user("bob", PASSWORD, false);
        let world = Self {
            alice: token(&s, "alice"),
            bob: token(&s, "bob"),
            url: s.url(&DEMO),
            s,
            git: GitClient::new(),
            w: tempfile::tempdir().unwrap(),
        };
        let r = world.api(
            &world.alice,
            "PUT",
            "/collaborators/bob",
            Some(json!({"role": "write"})),
        );
        assert!(r.status < 300, "{}", r.body);
        let r = world.api(&world.alice, "POST", "/protections", Some(protection));
        assert_eq!(r.status, 200, "{}", r.body);
        let w = world.w.path();
        world.git.ok(w, &["init", "-q", "-b", "main"]);
        world.commit("README", "demo\n", "init");
        world.push("main");
        world.git.ok(w, &["switch", "-q", "-c", "topic"]);
        world.commit("a.txt", &lines("a", None), "Add a");
        world.commit("b.txt", &lines("b", None), "Add b");
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

    fn review(&self, token: &str, n: i64, body: Value) -> slath_tests::http::Resp {
        self.api(
            token,
            "POST",
            &format!("/merge-requests/{n}/reviews"),
            Some(body),
        )
    }

    fn requirement(&self, n: i64, key: &str) -> (String, String) {
        let m = self
            .api(
                &self.alice,
                "GET",
                &format!("/merge-requests/{n}/mergeability"),
                None,
            )
            .json();
        let r = m["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .unwrap_or_else(|| panic!("no {key} in {m}"))
            .clone();
        (
            r["state"].as_str().unwrap().to_owned(),
            r["reason"].as_str().unwrap().to_owned(),
        )
    }
}

#[test]
fn conf_review_08_a_review_can_target_one_change() {
    let (w, n) = World::start(json!({"pattern": "main", "required_approvals": 1}));
    let ids = w.changes(n);
    assert_eq!(ids.len(), 2);

    // By full ID or by its first 8 characters.
    let r = w.review(&w.bob, n, json!({"verdict": "approve", "change": ids[0]}));
    assert_eq!(r.status, 201, "{}", r.body);
    let v = r.json();
    assert_eq!(v["change_id"], ids[0].as_str());
    assert_eq!(v["revision"], 1);
    assert_eq!(v["counts"], true);
    // It doesn't approve the other change.
    assert_eq!(w.requirement(n, "approvals").0, "unmet");
    let r = w.review(
        &w.bob,
        n,
        json!({"verdict": "comment", "body": "ok", "change": &ids[1][..8]}),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["change_id"], ids[1].as_str());

    // A review of the whole revision names no change.
    let r = w.review(&w.bob, n, json!({"verdict": "comment", "body": "overall"}));
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["change_id"], Value::Null);

    // Unknown changes, too short prefixes, and the author's own verdicts
    // are refused.
    for bad in ["zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz", &ids[0][..4]] {
        let r = w.review(&w.bob, n, json!({"verdict": "approve", "change": bad}));
        assert_eq!(r.status, 422, "{bad}: {}", r.body);
        assert!(r.body.contains("\"change\""), "{}", r.body);
    }
    let r = w.review(&w.alice, n, json!({"verdict": "approve", "change": ids[0]}));
    assert_eq!(r.status, 422, "{}", r.body);
}

#[test]
fn conf_review_08_no_changes_in_a_revision_reviewed_as_a_whole() {
    let (w, n) = World::start(json!({"pattern": "main", "required_approvals": 1}));
    let id = w.changes(n)[0].clone();
    // A merge commit makes the next revision whole-only.
    let p = w.w.path();
    w.git.ok(p, &["switch", "-q", "-c", "side", "main"]);
    w.commit("s.txt", "s\n", "Side");
    w.git.ok(p, &["switch", "-q", "topic"]);
    w.git
        .ok(p, &["merge", "-q", "--no-ff", "-m", "Merge side", "side"]);
    w.push("topic");
    w.wait_revisions(n, 2);
    let r = w.review(&w.bob, n, json!({"verdict": "approve", "change": id}));
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("as a whole"), "{}", r.body);
}

#[test]
fn conf_review_08_change_approvals_count_for_their_change_only() {
    let (w, n) = World::start(json!({"pattern": "main", "required_approvals": 1}));
    let ids = w.changes(n);
    let (a, b) = (&ids[0][..8], &ids[1][..8]);

    // One change approved: the other one is missing.
    w.review(&w.bob, n, json!({"verdict": "approve", "change": ids[0]}));
    let (state, reason) = w.requirement(n, "approvals");
    assert_eq!(state, "unmet");
    assert!(
        reason.contains(&format!("missing: {b} (0 of 1)")),
        "{reason}"
    );
    w.review(&w.bob, n, json!({"verdict": "approve", "change": ids[1]}));
    assert_eq!(
        w.requirement(n, "approvals"),
        ("met".into(), "Approved by bob.".into())
    );

    // "Add b" changes: its approval goes stale, the one of "Add a" stays.
    std::fs::write(w.w.path().join("b.txt"), lines("b", Some(3))).unwrap();
    w.git
        .ok(w.w.path(), &["commit", "-qa", "--amend", "--no-edit"]);
    w.push("topic");
    w.wait_revisions(n, 2);
    let (state, reason) = w.requirement(n, "approvals");
    assert_eq!(state, "unmet", "{reason}");
    assert!(
        reason.contains(&format!("missing: {b} (0 of 1)")),
        "{reason}"
    );
    assert!(reason.contains("1 approval is stale"), "{reason}");
    let reviews = w
        .api(
            &w.alice,
            "GET",
            &format!("/merge-requests/{n}/reviews"),
            None,
        )
        .json();
    let counts: Vec<(String, bool)> = reviews
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["change_id"].as_str().unwrap()[..8].to_owned(),
                r["counts"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(counts, vec![(a.to_owned(), true), (b.to_owned(), false)]);

    // An approval of the whole covers both; a request for changes on one
    // change blocks merging.
    w.review(&w.bob, n, json!({"verdict": "approve"}));
    assert_eq!(w.requirement(n, "approvals").0, "met");
    w.review(
        &w.bob,
        n,
        json!({"verdict": "request_changes", "body": "not a", "change": ids[0]}),
    );
    let (state, reason) = w.requirement(n, "changes_requested");
    assert_eq!(state, "unmet");
    assert_eq!(reason, format!("bob (on {a}) requested changes."));
    // ...and "Add a" is no longer approved by bob.
    assert_eq!(w.requirement(n, "approvals").0, "unmet");
}

#[test]
fn conf_review_08_sensitive_paths_need_a_maintainer_on_every_change() {
    let (w, n) = World::start(json!({"pattern": "main", "required_approvals": 0}));
    // A third change touches an agent instruction file.
    w.commit("AGENTS.md", "be careful\n", "Instruct agents");
    w.push("topic");
    w.wait_revisions(n, 2);
    let r = w.api(
        &w.alice,
        "PUT",
        "/collaborators/bob",
        Some(json!({"role": "maintain"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let ids = w.changes(n);
    assert_eq!(ids.len(), 3);
    for id in &ids[..2] {
        w.review(&w.bob, n, json!({"verdict": "approve", "change": id}));
    }
    assert_eq!(w.requirement(n, "sensitive_paths").0, "unmet");
    w.review(&w.bob, n, json!({"verdict": "approve", "change": ids[2]}));
    let (state, reason) = w.requirement(n, "sensitive_paths");
    assert_eq!(state, "met", "{reason}");
    assert!(
        reason.ends_with("approved by a maintainer: bob."),
        "{reason}"
    );
}
