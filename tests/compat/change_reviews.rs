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

#[test]
fn conf_review_08_the_ledger_records_each_change_approval() {
    let (w, n) = World::start(json!({"pattern": "main", "required_approvals": 1}));
    let ids = w.changes(n);
    for id in &ids {
        w.review(&w.bob, n, json!({"verdict": "approve", "change": id}));
    }
    std::fs::write(w.w.path().join("b.txt"), lines("b", Some(3))).unwrap();
    w.git
        .ok(w.w.path(), &["commit", "-qa", "--amend", "--no-edit"]);
    w.push("topic");
    w.wait_revisions(n, 2);
    let ledger = w
        .api(
            &w.alice,
            "GET",
            &format!("/merge-requests/{n}/ledger"),
            None,
        )
        .json();
    let approvals: Vec<(String, String, String)> = ledger
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"].as_str().unwrap().starts_with("approval_") && e["revision"] == 2)
        .map(|e| {
            (
                e["kind"].as_str().unwrap().to_owned(),
                e["change_id"].as_str().unwrap_or("").to_owned(),
                e["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        approvals,
        vec![
            (
                "approval_carried".into(),
                ids[0].clone(),
                "same-patch".into()
            ),
            (
                "approval_stale".into(),
                ids[1].clone(),
                "patch-changed".into()
            ),
        ]
    );
    let mut alice = w.s.login("alice", PASSWORD);
    let page = alice.get(&format!("/alice/demo/merge-requests/{n}")).body;
    assert!(
        page.contains(&format!(
            "made earlier approvals of change {} stale: revision 2 changed its patch",
            &ids[1][..8]
        )),
        "no timeline note"
    );
}

/// The stack column of a change page: each `<li class="stack-item">`'s
/// text with tags removed and spaces folded, bottom first.
fn stack_column(html: &str) -> Vec<String> {
    let tags = regex::Regex::new(r"<[^>]*>").unwrap();
    let Some(start) = html.find("aria-labelledby=\"stack-heading\"") else {
        return Vec::new();
    };
    let end = html[start..]
        .find("</ol>")
        .map_or(html.len(), |e| start + e);
    html[start..end]
        .split("<li class=\"stack-item\"")
        .skip(1)
        .map(|item| {
            tags.replace_all(&format!("<{item}"), " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

#[test]
fn conf_jj_04_change_pages_show_the_stack_bottom_first() {
    let (w, n) = World::start(json!({"pattern": "main", "required_approvals": 1}));
    let mut ui = w.s.login("alice", PASSWORD);
    let page = |ui: &mut slath_tests::http::Client, id: &str| {
        let r = ui.get(&format!("/alice/demo/merge-requests/{n}/changes/{id}"));
        assert_eq!(r.status, 200, "{id}: {}", r.body);
        r.body
    };
    // Two more changes make a stack of four; bob approves the second.
    w.commit("c.txt", &lines("c", None), "Add c");
    w.commit("d.txt", &lines("d", None), "Add d");
    w.push("topic");
    w.wait_revisions(n, 2);
    let ids = w.changes(n);
    assert_eq!(ids.len(), 4);
    w.review(&w.bob, n, json!({"verdict": "approve", "change": ids[1]}));

    let second = page(&mut ui, &ids[1]);
    let rows = stack_column(&second);
    assert_eq!(rows.len(), 4, "{second}");
    for (i, (row, subject)) in rows
        .iter()
        .zip(["Add a", "Add b", "Add c", "Add d"])
        .enumerate()
    {
        assert!(row.starts_with(&format!("{}. ", i + 1)), "{row}");
        assert!(row.contains(subject), "{row}");
        assert!(row.contains(&ids[i][..8]), "{row}");
    }
    // The shown change is marked and not a link; the others are links.
    assert!(rows[1].contains("(shown)"), "{}", rows[1]);
    let base = format!("/alice/demo/merge-requests/{n}/changes/");
    assert!(second.contains(&format!("<a href=\"{base}{}\">Add a</a>", ids[0])));
    assert!(!second.contains(&format!("<a href=\"{base}{}\">Add b</a>", ids[1])));
    assert!(rows[1].contains("Approved by bob"), "{}", rows[1]);
    assert!(!rows[0].contains("Approved"), "{}", rows[0]);
    // The two changes added by revision 2 are new in it.
    assert!(rows[2].contains("New in revision 2") && rows[3].contains("New in revision 2"));
    assert!(!rows[0].contains("New in"), "{}", rows[0]);
    // Buttons to the changes below and above, with their keys.
    assert!(second.contains(&format!(
        "href=\"{base}{}\" data-key=\"k\" aria-keyshortcuts=\"k\"",
        ids[0]
    )));
    assert!(second.contains(&format!(
        "href=\"{base}{}\" data-key=\"j\" aria-keyshortcuts=\"j\"",
        ids[2]
    )));
    // Grouped by kind, with the position; a version per revision.
    assert!(second.contains(">Change 2 of 4<"), "{second}");
    assert!(second.contains(">Version 2 of 2<"), "{second}");
    // A change new in revision 2 has one version: no version steps.
    assert!(!page(&mut ui, &ids[2]).contains("step-version"));
    // The bottom change has no previous change, the top one no next.
    assert!(!page(&mut ui, &ids[0]).contains("data-key=\"k\""));
    assert!(!page(&mut ui, &ids[3]).contains("data-key=\"j\""));

    // Reordering c and d and inserting a change below them: the stack
    // follows the new order, and the version steps appear for a change
    // with two versions.
    let p = w.w.path();
    w.git.ok(p, &["reset", "-q", "--hard", "HEAD~2"]);
    w.commit("e.txt", &lines("e", None), "Add e");
    w.commit("d.txt", &lines("d", None), "Add d");
    w.commit("c.txt", &lines("c", None), "Add c");
    w.push("topic");
    w.wait_revisions(n, 3);
    let now = w.changes(n);
    assert_eq!(now.len(), 5);
    assert_eq!((&now[3], &now[4]), (&ids[3], &ids[2]));
    let top = page(&mut ui, &ids[2]);
    let rows = stack_column(&top);
    let subjects: Vec<&str> = rows
        .iter()
        .map(|r| {
            ["Add a", "Add b", "Add e", "Add d", "Add c"]
                .into_iter()
                .find(|s| r.contains(s))
                .unwrap()
        })
        .collect();
    assert_eq!(subjects, ["Add a", "Add b", "Add e", "Add d", "Add c"]);
    assert!(rows[2].contains("New in revision 3"), "{}", rows[2]);
    assert!(top.contains("data-key=\"[\""), "{top}");
    assert!(
        top.contains(">Change 5 of 5<") && top.contains(">Version 2 of 2<"),
        "{top}"
    );
    assert!(!top.contains("data-key=\"]\""), "{top}");
}

#[test]
fn conf_jj_04_the_stack_column_needs_more_than_one_change() {
    let (w, n) = World::start(json!({"pattern": "main", "required_approvals": 1}));
    let p = w.w.path();
    w.git.ok(p, &["reset", "-q", "--hard", "HEAD~1"]);
    w.push("topic");
    w.wait_revisions(n, 2);
    let ids = w.changes(n);
    assert_eq!(ids.len(), 1);
    // The removed change still has a page: the stack lists it apart.
    let mut ui = w.s.login("alice", PASSWORD);
    let r = ui.get(&format!(
        "/alice/demo/merge-requests/{n}/changes/{}",
        ids[0]
    ));
    assert_eq!(r.status, 200);
    assert!(r.body.contains("Removed changes"), "{}", r.body);

    // A merge request of one change from the start shows no stack.
    w.git.ok(p, &["switch", "-q", "-c", "single", "main"]);
    w.commit("s.txt", "s\n", "Add s");
    w.push("single");
    let r = w.api(
        &w.alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "single", "title": "Add s"})),
    );
    let m = r.json()["number"].as_i64().unwrap();
    w.wait_revisions(m, 1);
    let id = w.changes(m)[0].clone();
    let r = ui.get(&format!("/alice/demo/merge-requests/{m}/changes/{id}"));
    assert_eq!(r.status, 200);
    assert!(!r.body.contains("stack-heading"), "{}", r.body);
    assert!(!r.body.contains("data-key="), "{}", r.body);
}

#[test]
fn conf_jj_04_changes_and_files_tabs_and_their_old_addresses() {
    let (w, n) = World::start(json!({"pattern": "main", "required_approvals": 1}));
    let ids = w.changes(n);
    let mut ui = w.s.login("alice", PASSWORD);
    let mr = format!("/alice/demo/merge-requests/{n}");
    // The Changes tab is the stack; each title leads to its change.
    let stack = ui.get(&format!("{mr}/changes"));
    assert_eq!(stack.status, 200);
    assert!(stack.body.contains(">2 changes<"), "{}", stack.body);
    assert!(stack.body.contains(&format!(
        "<a class=\"item-title\" href=\"{mr}/changes/{}\">Add a</a>",
        ids[0]
    )));
    for (href, label) in [("changes", "Changes"), ("files", "Files")] {
        assert!(
            stack.body.contains(&format!("<a href=\"{mr}/{href}\""))
                && stack
                    .body
                    .contains(&format!(">{label} <span class=\"count\">")),
            "{label}: {}",
            stack.body
        );
    }
    // The Files tab is the diff.
    let files = ui.get(&format!("{mr}/files"));
    assert_eq!(files.status, 200);
    assert!(files.body.contains("a.txt") && files.body.contains("diff-file"));
    // Old addresses: the Commits tab is now Changes, and the diff's
    // queries go on to Files, with their query.
    for (old, new) in [
        ("/commits", "/changes"),
        ("/commits?status=rebound", "/changes?status=rebound"),
        ("/changes?since=1", "/files?since=1"),
        ("/changes?file=0&w=1", "/files?file=0&w=1"),
    ] {
        let r = ui.get(&format!("{mr}{old}"));
        assert_eq!(r.status, 308, "{old}");
        assert_eq!(
            r.header("location"),
            Some(format!("{mr}{new}").as_str()),
            "{old}"
        );
    }
    // Invisible merge requests stay not found, old address or new.
    let mut stranger = w.s.client();
    for path in ["/commits", "/changes?since=1", "/changes", "/files"] {
        let r = stranger.get(&format!("/alice/demo/merge-requests/{}{path}", n + 100));
        assert_eq!(r.status, 404, "{path}");
    }
}

#[test]
fn conf_review_08_a_prefix_has_the_requirements_of_its_changes() {
    let (w, n) = World::start(json!({"pattern": "main", "required_approvals": 1}));
    let ids = w.changes(n);
    let upto = |id: &str| {
        w.api(
            &w.alice,
            "GET",
            &format!("/merge-requests/{n}/mergeability?upto_change={id}"),
            None,
        )
    };
    // Only "Add a" is approved, and bob asks for changes on "Add b": the
    // prefix of "Add a" can land, the whole can't.
    w.review(&w.bob, n, json!({"verdict": "approve", "change": ids[0]}));
    w.review(
        &w.bob,
        n,
        json!({"verdict": "request_changes", "body": "not yet", "change": ids[1]}),
    );
    let m = upto(&ids[0][..8]).json();
    assert_eq!(m["mergeable"], true, "{m}");
    assert_eq!(w.requirement(n, "approvals").0, "unmet");
    assert_eq!(w.requirement(n, "changes_requested").0, "unmet");
    // Up to the top change is the whole merge request.
    let m = upto(&ids[1]).json();
    assert_eq!(m["mergeable"], false, "{m}");
    // Unknown changes and too short prefixes are refused.
    for bad in ["zzzzzzzzzzzzzzzz", &ids[0][..4]] {
        let r = upto(bad);
        assert_eq!(r.status, 422, "{bad}: {}", r.body);
        assert!(r.body.contains("upto_change"), "{}", r.body);
    }
}

#[test]
fn conf_review_08_a_prefix_lands_with_the_approvals_of_its_changes() {
    let (w, n) = World::start(json!({"pattern": "main", "required_approvals": 1}));
    let ids = w.changes(n);
    let upto = |id: &str| {
        w.api(
            &w.alice,
            "GET",
            &format!("/merge-requests/{n}/mergeability?upto_change={id}"),
            None,
        )
        .json()
    };
    // Only "Add a" is approved, and bob asks for changes on "Add b": the
    // prefix of "Add a" can land, the whole can't.
    w.review(&w.bob, n, json!({"verdict": "approve", "change": ids[0]}));
    w.review(
        &w.bob,
        n,
        json!({"verdict": "request_changes", "body": "not yet", "change": ids[1]}),
    );
    assert_eq!(upto(&ids[0])["mergeable"], true, "{}", upto(&ids[0]));
    assert_eq!(w.requirement(n, "approvals").0, "unmet");
    let head = |w: &World| {
        w.api(&w.alice, "GET", &format!("/merge-requests/{n}"), None)
            .json()["head_oid"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let merge = |w: &World, body: Value| {
        w.api(
            &w.bob,
            "POST",
            &format!("/merge-requests/{n}/merge"),
            Some(body),
        )
    };
    let r = merge(
        &w,
        json!({"strategy": "rebase", "expected_head_oid": head(&w)}),
    );
    assert_eq!(r.status, 409, "{}", r.body);
    let r = merge(
        &w,
        json!({"strategy": "rebase", "expected_head_oid": head(&w), "upto_change": ids[0]}),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let main = w
        .git
        .ok(&w.s.repo_path(&DEMO), &["ls-tree", "--name-only", "main"]);
    assert!(main.contains("a.txt") && !main.contains("b.txt"), "{main}");
    // The rest is a revision of its own and still needs its approval.
    w.wait_revisions(n, 2);
    assert_eq!(w.changes(n), vec![ids[1].clone()]);
    let (state, reason) = w.requirement(n, "changes_requested");
    assert_eq!(state, "unmet", "{reason}");
}

#[test]
fn conf_review_08_threads_on_later_changes_dont_block_a_prefix() {
    let (w, n) = World::start(json!({"pattern": "main", "require_resolved_threads": true}));
    let stack = w
        .api(&w.alice, "GET", &format!("/merge-requests/{n}/stack"), None)
        .json();
    let entry = |i: usize| stack["entries"][i].clone();
    let (a, b) = (entry(0), entry(1));
    let a_id = a["change_id"].as_str().unwrap().to_owned();
    let thread = |body: Value| {
        let r = w.api(
            &w.bob,
            "POST",
            &format!("/merge-requests/{n}/threads"),
            Some(body),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        r.json()["id"].as_str().unwrap().to_owned()
    };
    let threads = |id: &str| {
        let m = w
            .api(
                &w.alice,
                "GET",
                &format!("/merge-requests/{n}/mergeability?upto_change={id}"),
                None,
            )
            .json();
        let r = m["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == "threads")
            .unwrap()
            .clone();
        (
            r["state"].as_str().unwrap().to_owned(),
            r["reason"].as_str().unwrap().to_owned(),
        )
    };
    // A thread on "Add b"'s own diff waits for "Add b".
    thread(json!({"body": "b?", "path": "b.txt", "line_start": 2, "commit": b["commit_oid"]}));
    assert_eq!(
        threads(&a_id),
        (
            "met".into(),
            "No unresolved threads block landing these; 1 is on a later change.".into()
        )
    );
    assert_eq!(w.requirement(n, "threads").0, "unmet");
    // One on "Add a"'s diff, and a general one, block it.
    let on_a =
        thread(json!({"body": "a?", "path": "a.txt", "line_start": 2, "commit": a["commit_oid"]}));
    let general = thread(json!({"body": "overall?"}));
    assert_eq!(
        threads(&a_id),
        (
            "unmet".into(),
            "2 threads are unresolved and block landing these; 1 more is on a later change.".into()
        )
    );
    for id in [on_a, general] {
        let r = w.api(
            &w.alice,
            "PATCH",
            &format!("/merge-requests/{n}/threads/{id}"),
            Some(json!({"resolved": true})),
        );
        assert!(r.status < 300, "{}", r.body);
    }
    assert_eq!(threads(&a_id).0, "met");
}
