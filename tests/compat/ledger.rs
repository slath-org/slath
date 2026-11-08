// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The review continuity ledger (REVIEW-12) with Git and jj rewrites: each
//! binding with its state and reason, approvals carried or stale with
//! theirs, statuses stale after the head moves, target moves, unresolved
//! bindings that block merge, and signatures removed by a rebase merge
//! (ambiguous pairings are covered by the identity unit tests). The API and the change pages
//! agree with it.

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::jj::{Jj, jj_binaries};

struct Ctx {
    s: TestServer,
    alice: String,
    bob: String,
    url: String,
    git: GitClient,
}

fn token(s: &TestServer, user: &str) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, user, &["repo:write", "mrs:write", "statuses:write"])
}

impl Ctx {
    fn start() -> Self {
        let s = TestServer::start();
        s.create_user("bob", PASSWORD, false);
        let c = Self {
            alice: token(&s, "alice"),
            bob: token(&s, "bob"),
            url: s.url(&DEMO),
            s,
            git: GitClient::new(),
        };
        let r = c.api(
            &c.alice,
            "PUT",
            "/collaborators/bob",
            Some(json!({"role": "write"})),
        );
        assert!(r.status < 300, "{}", r.body);
        let r = c.api(
            &c.alice,
            "POST",
            "/protections",
            Some(json!({"pattern": "main", "required_approvals": 1, "block_force_push": false})),
        );
        assert_eq!(r.status, 200, "{}", r.body);
        c
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

    fn wait(&self, n: i64, k: i64) -> String {
        let start = std::time::Instant::now();
        loop {
            let m = self
                .api(&self.alice, "GET", &format!("/merge-requests/{n}"), None)
                .json();
            if m["revisions"] == k {
                return m["head_oid"].as_str().unwrap().to_owned();
            }
            assert!(
                start.elapsed().as_secs() < 30,
                "no revision {k}\n{}",
                self.s.log()
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    fn ledger(&self, n: i64) -> Vec<Value> {
        let r = self.api(
            &self.alice,
            "GET",
            &format!("/merge-requests/{n}/ledger"),
            None,
        );
        assert_eq!(r.status, 200, "{}", r.body);
        r.json().as_array().unwrap().clone()
    }

    fn push(&self, w: &Path, refspec: &str) {
        self.git
            .ok(w, &["push", "-q", "--force", &self.url, refspec]);
    }
}

/// `kind/state/reason` of the entries of one revision.
fn shape(entries: &[Value], rev: i64) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e["revision"] == rev)
        .map(|e| {
            format!(
                "{}/{}/{}",
                e["kind"].as_str().unwrap(),
                e["state"].as_str().unwrap_or("-"),
                e["reason"].as_str().unwrap_or("-")
            )
        })
        .collect()
}

fn lines(tag: &str, edit: Option<usize>) -> String {
    (0..20)
        .map(|i| {
            if Some(i) == edit {
                format!("{tag} {i} edited\n")
            } else {
                format!("{tag} {i}\n")
            }
        })
        .collect()
}

fn commit(git: &GitClient, w: &Path, file: &str, text: &str, msg: &str) {
    std::fs::write(w.join(file), text).unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", msg]);
}

#[test]
fn conf_review_12_the_ledger_records_bindings_approvals_and_statuses() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    c.git.ok(&w, &["init", "-q", "-b", "main"]);
    commit(&c.git, &w, "README", "x\n", "init");
    c.push(&w, "main");
    c.git.ok(&w, &["switch", "-q", "-c", "topic"]);
    commit(&c.git, &w, "a.txt", &lines("a", None), "Add a");
    commit(&c.git, &w, "b.txt", &lines("b", None), "Add b");
    c.push(&w, "topic");
    let r = c.api(
        &c.alice,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Topic"})),
    );
    let n = r.json()["number"].as_i64().unwrap();
    let head = c.wait(n, 1);
    assert_eq!(
        shape(&c.ledger(n), 1),
        vec!["binding/new/-", "binding/new/-"]
    );

    // Approved, with a status on the head.
    assert_eq!(
        c.api(
            &c.bob,
            "POST",
            &format!("/merge-requests/{n}/reviews"),
            Some(json!({"verdict": "approve"}))
        )
        .status,
        201
    );
    assert_eq!(
        c.api(
            &c.bob,
            "POST",
            &format!("/statuses/{head}"),
            Some(json!({"state": "success", "context": "ci/build"}))
        )
        .status,
        201
    );

    // Git: a plain rebase onto a moved main keeps the patches.
    c.git.ok(&w, &["switch", "-q", "main"]);
    commit(&c.git, &w, "m.txt", "m\n", "Main moves");
    c.push(&w, "main");
    c.git.ok(&w, &["switch", "-q", "topic"]);
    c.git.ok(&w, &["rebase", "-q", "main"]);
    c.push(&w, "topic");
    c.wait(n, 2);
    let l = c.ledger(n);
    assert!(l.iter().any(|e| e["kind"] == "target_moved"), "{l:?}");
    assert_eq!(
        shape(&l, 2),
        vec![
            "binding/confirmed/patch",
            "binding/confirmed/patch",
            "approval_carried/-/same-patches",
            "status_stale/success/head-moved",
        ]
    );

    // Git: the second commit's content changes without an ID: inferred,
    // and the approval goes stale.
    c.git.ok(&w, &["reset", "-q", "--hard", "HEAD~1"]);
    commit(&c.git, &w, "b.txt", &lines("b", Some(3)), "Add b, reworked");
    c.push(&w, "topic");
    c.wait(n, 3);
    assert_eq!(
        shape(&c.ledger(n), 3),
        vec![
            "binding/confirmed/alias",
            "binding/inferred/content",
            "approval_stale/-/inferred"
        ]
    );
    let stale = c
        .ledger(n)
        .into_iter()
        .find(|e| e["kind"] == "approval_stale")
        .unwrap();
    assert_eq!(stale["detail"]["reviewer"], "bob");

    // jj edits the first commit: jj writes the predicted ID, confirmed.
    if let Some((version, bin)) = jj_binaries().into_iter().last() {
        let jj = Jj::new(&version, &bin);
        let j = d.path().join("jj");
        jj.ok(d.path(), &["git", "clone", &c.url, j.to_str().unwrap()]);
        jj.ok(&j, &["bookmark", "track", "topic@origin"]);
        jj.ok(&j, &["edit", "topic-"]);
        std::fs::write(j.join("a.txt"), lines("a", Some(1))).unwrap();
        jj.ok(&j, &["git", "push", "--bookmark", "topic"]);
        c.wait(n, 4);
        let l = c.ledger(n);
        let r4: Vec<&Value> = l
            .iter()
            .filter(|e| e["revision"] == 4 && e["kind"] == "binding")
            .collect();
        assert_eq!(r4[0]["state"], "confirmed", "{version}");
        assert!(
            r4[0]["detail"]["by"].as_str().unwrap().starts_with("jj:"),
            "{version}: {:?}",
            r4[0]
        );
    }

    // The change page tells the same story.
    let id = c
        .ledger(n)
        .into_iter()
        .find(|e| e["kind"] == "binding" && e["state"] == "inferred")
        .unwrap()["change_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let page =
        c.s.login("alice", PASSWORD)
            .get(&format!("/alice/demo/merge-requests/{n}/changes/{id}"))
            .body;
    assert!(
        page.contains("matched by similar content (inferred"),
        "{page}"
    );
    assert!(page.contains("matched by an identical patch"));
}

#[test]
fn conf_review_12_unresolved_bindings_block_and_removed_signatures_are_noted() {
    let c = Ctx::start();
    let r = c.api(
        &c.alice,
        "POST",
        "/protections",
        Some(json!({"pattern": "main", "required_approvals": 0, "block_force_push": false})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    c.git.ok(&w, &["init", "-q", "-b", "main"]);
    commit(&c.git, &w, "README", "x\n", "init");
    c.push(&w, "main");
    let main = c.git.ok(&w, &["rev-parse", "HEAD"]).trim().to_owned();

    // Two commits with the same header: unresolved, merge blocked.
    let craft = |parent: &str, extra: &str, file: &str| -> String {
        std::fs::write(w.join(file), lines(file, None)).unwrap();
        c.git.ok(&w, &["add", "-A"]);
        let tree = c.git.ok(&w, &["write-tree"]).trim().to_owned();
        let raw = format!(
            "tree {tree}\nparent {parent}\nauthor A <a@example.org> 1700000000 +0000\n\
             committer A <a@example.org> 1700000000 +0000\n{extra}\n{file}\n"
        );
        let mut child = c
            .git
            .cmd(&w)
            .args(["hash-object", "-t", "commit", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(raw.as_bytes())
            .unwrap();
        let oid = String::from_utf8(child.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned();
        c.git.ok(&w, &["reset", "-q", "--hard", &oid]);
        oid
    };
    c.git.ok(&w, &["switch", "-q", "-c", "dup"]);
    let a = craft(&main, "change-id zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n", "d1");
    craft(&a, "change-id zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n", "d2");
    c.push(&w, "dup");
    let n = c
        .api(
            &c.alice,
            "POST",
            "/merge-requests",
            Some(json!({"source_branch": "dup", "title": "Dup"})),
        )
        .json()["number"]
        .as_i64()
        .unwrap();
    c.wait(n, 1);
    assert_eq!(
        shape(&c.ledger(n), 1),
        vec![
            "binding/unresolved/duplicate-id",
            "binding/unresolved/duplicate-id"
        ]
    );
    let m = c
        .api(
            &c.alice,
            "GET",
            &format!("/merge-requests/{n}/mergeability"),
            None,
        )
        .json();
    assert_eq!(m["mergeable"], false, "{m}");

    // A signed commit, rebase-merged: the ledger notes the dropped signature.
    c.git.ok(&w, &["switch", "-q", "-C", "signed", &main]);
    craft(
        &main,
        "gpgsig -----BEGIN PGP SIGNATURE-----\n fake\n -----END PGP SIGNATURE-----\n",
        "s1",
    );
    c.push(&w, "signed");
    let n = c
        .api(
            &c.alice,
            "POST",
            "/merge-requests",
            Some(json!({"source_branch": "signed", "title": "Signed"})),
        )
        .json()["number"]
        .as_i64()
        .unwrap();
    let head = c.wait(n, 1);
    // Move main so the rebase rewrites the commit.
    c.git.ok(&w, &["switch", "-q", "-C", "main", &main]);
    commit(&c.git, &w, "m.txt", "m\n", "Main moves");
    c.push(&w, "main");
    let r = c.api(
        &c.alice,
        "POST",
        &format!("/merge-requests/{n}/merge"),
        Some(json!({"strategy": "rebase", "expected_head_oid": head})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let l = c.ledger(n);
    let sig = l
        .iter()
        .find(|e| e["kind"] == "signature_removed")
        .unwrap_or_else(|| panic!("{l:?}"));
    assert_eq!(sig["detail"]["commit"], head.as_str());
    assert_eq!(sig["actor"], "alice");
}
