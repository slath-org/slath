// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hunk checkpoints (REVIEW-17): hunks marked viewed stay viewed in a new
//! revision only while their changed lines are unchanged, whichever tool
//! made the revisions (Git, jj, or both).

use std::path::Path;

use regex::Regex;
use serde_json::json;
use slath_tests::http::{Client, submit};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::fork_merge_requests::{api, eventually, token};
use crate::jj::{Jj, jj_binaries};

/// Which tool makes the first and the second revision.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Git,
    Jj,
}

/// f.txt: 40 lines, with the lines in `edits` replaced by their text.
fn file(edits: &[(usize, &str)]) -> String {
    (1..=40)
        .map(|i| match edits.iter().find(|(n, _)| *n == i) {
            Some((_, t)) => format!("{t}\n"),
            None => format!("line {i}\n"),
        })
        .collect()
}

/// The three hunks of the first revision, and the second (the middle one
/// changed).
const FIRST: [(usize, &str); 3] = [(5, "five"), (20, "twenty"), (35, "thirty-five")];
const SECOND: [(usize, &str); 3] = [(5, "five"), (20, "TWENTY"), (35, "thirty-five")];

struct World {
    s: TestServer,
    alice: String,
    d: tempfile::TempDir,
    git: GitClient,
    url: String,
    /// The jj clone, once jj made a revision.
    jj: Option<(Jj, std::path::PathBuf)>,
}

impl World {
    fn start() -> Self {
        let s = TestServer::start();
        s.create_user("bob", PASSWORD, false);
        let alice = token(&s, "alice");
        let r = api(
            &s,
            &alice,
            "PUT",
            "/alice/demo/collaborators/bob",
            Some(json!({"role": "write"})),
        );
        assert!(r.status < 300, "{}", r.body);
        let git = GitClient::new();
        let d = tempfile::tempdir().unwrap();
        let url = s.url(&DEMO);
        let w = d.path().join("git");
        std::fs::create_dir_all(&w).unwrap();
        git.ok(&w, &["init", "-q", "-b", "main"]);
        write(&w, "f.txt", &file(&[]));
        write(&w, "other.txt", "o\n");
        git.ok(&w, &["add", "."]);
        git.ok(&w, &["commit", "-q", "-m", "init"]);
        git.ok(&w, &["push", "-q", &url, "main"]);
        Self {
            s,
            alice,
            d,
            git,
            url,
            jj: None,
        }
    }

    fn git_dir(&self) -> std::path::PathBuf {
        self.d.path().join("git")
    }

    /// Moves main on without touching f.txt.
    fn move_main(&self) {
        let w = self.git_dir();
        self.git.ok(&w, &["switch", "-q", "main"]);
        self.git.ok(&w, &["pull", "-q", &self.url, "main"]);
        write(&w, "other.txt", "o2\n");
        self.git.ok(&w, &["commit", "-qam", "Other"]);
        self.git.ok(&w, &["push", "-q", &self.url, "main"]);
    }

    /// Pushes `topic` with f.txt as `edits`, onto the latest main.
    fn push(&mut self, tool: Tool, edits: &[(usize, &str)], first: bool) {
        match tool {
            Tool::Git => {
                let w = self.git_dir();
                if first {
                    self.git.ok(&w, &["switch", "-q", "-c", "topic"]);
                } else {
                    self.git.ok(&w, &["fetch", "-q", &self.url, "topic"]);
                    self.git
                        .ok(&w, &["switch", "-q", "-C", "topic", "FETCH_HEAD"]);
                    self.git.ok(&w, &["fetch", "-q", &self.url, "main"]);
                    self.git.ok(&w, &["rebase", "-q", "FETCH_HEAD"]);
                }
                write(&w, "f.txt", &file(edits));
                self.git.ok(&w, &["add", "."]);
                if first {
                    self.git.ok(&w, &["commit", "-q", "-m", "Edit f"]);
                } else {
                    self.git.ok(&w, &["commit", "-q", "--amend", "--no-edit"]);
                }
                self.git
                    .ok(&w, &["push", "-q", "--force", &self.url, "topic"]);
            }
            Tool::Jj => {
                let (version, bin) = jj_binaries().into_iter().last().unwrap();
                if self.jj.is_none() {
                    let jj = Jj::new(&version, &bin);
                    let j = self.d.path().join("jj");
                    jj.ok(
                        self.d.path(),
                        &["git", "clone", &self.url, j.to_str().unwrap()],
                    );
                    if first {
                        jj.ok(&j, &["new", "main@origin", "-m", "Edit f"]);
                        jj.ok(&j, &["bookmark", "create", "topic", "-r", "@"]);
                    } else {
                        // A Git-made first revision: take it over.
                        jj.ok(&j, &["bookmark", "track", "topic@origin"]);
                        jj.ok(&j, &["edit", "topic"]);
                    }
                    self.jj = Some((jj, j));
                }
                let (jj, j) = self.jj.as_ref().unwrap();
                if !first {
                    jj.ok(j, &["git", "fetch"]);
                    jj.ok(j, &["rebase", "-d", "main@origin"]);
                }
                write(j, "f.txt", &file(edits));
                jj.ok(j, &["git", "push", "--bookmark", "topic"]);
            }
        }
    }

    fn wait(&self, n: i64, k: i64) {
        eventually(&self.s, "no revision", || {
            api(
                &self.s,
                &self.alice,
                "GET",
                &format!("/alice/demo/merge-requests/{n}"),
                None,
            )
            .json()["revisions"]
                == k
        });
    }
}

fn write(dir: &Path, f: &str, text: &str) {
    std::fs::write(dir.join(f), text).unwrap();
}

const FILES: &str = "/alice/demo/merge-requests/1/files";

/// The hunks on the diff page: their keys and whether they're viewed.
fn hunks(bob: &mut Client) -> Vec<(String, bool)> {
    let page = bob.get(FILES).body;
    let re = Regex::new(r#"(?s)name="hunk" value="([0-9a-f]{64})">.*?aria-pressed="(true|false)""#)
        .unwrap();
    re.captures_iter(&page)
        .map(|c| (c[1].to_owned(), &c[2] == "true"))
        .collect()
}

fn mark(bob: &mut Client, fields: &[(&str, &str)]) -> slath_tests::http::Resp {
    let mut all = vec![("path", "f.txt"), ("back", FILES)];
    all.extend_from_slice(fields);
    submit(bob, FILES, "/alice/demo/merge-requests/1/viewed", &all)
}

fn scenario(first: Tool, second: Tool) {
    if (first == Tool::Jj || second == Tool::Jj) && jj_binaries().is_empty() {
        eprintln!("no jj: skipping");
        return;
    }
    let mut w = World::start();
    w.push(first, &FIRST, true);
    let r = api(
        &w.s,
        &w.alice,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Edit f"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    w.wait(1, 1);
    let mut bob = w.s.login("bob", PASSWORD);
    let first_keys: Vec<String> = hunks(&mut bob).into_iter().map(|(k, _)| k).collect();
    assert_eq!(first_keys.len(), 3);
    // Marking the file marks its hunks.
    let mut fields = vec![("viewed", "true")];
    fields.extend(first_keys.iter().map(|k| ("hunks", k.as_str())));
    assert_eq!(mark(&mut bob, &fields).status, 303);
    assert!(
        bob.get(FILES).body.contains("You marked this file viewed"),
        "the file folds"
    );

    // A rebase onto a moved main that changes the middle hunk: the other
    // two stay viewed, the file doesn't.
    w.move_main();
    w.push(second, &SECOND, false);
    w.wait(1, 2);
    let now = hunks(&mut bob);
    let states: Vec<bool> = now.iter().map(|(_, v)| *v).collect();
    assert_eq!(states, vec![true, false, true]);
    assert_eq!(now[0].0, first_keys[0]);
    assert_eq!(now[2].0, first_keys[2]);
    assert_ne!(now[1].0, first_keys[1]);

    // Viewing the changed hunk makes the file viewed again.
    let r = mark(&mut bob, &[("viewed", "true"), ("hunk", &now[1].0)]);
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(bob.get(FILES).body.contains("You marked this file viewed"));
    // Unmarking one hunk unfolds it, with that hunk not viewed.
    let r = mark(&mut bob, &[("viewed", "false"), ("hunk", &now[0].0)]);
    assert_eq!(r.status, 303, "{}", r.body);
    let states: Vec<bool> = hunks(&mut bob).iter().map(|(_, v)| *v).collect();
    assert_eq!(states, vec![false, true, true]);
    // An approval records the hunks viewed, whichever tool made them.
    let bob_token = token(&w.s, "bob");
    let r = api(
        &w.s,
        &bob_token,
        "POST",
        "/alice/demo/merge-requests/1/reviews",
        Some(json!({"verdict": "approve"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let m = api(
        &w.s,
        &w.alice,
        "GET",
        "/alice/demo/merge-requests/1/mergeability",
        None,
    )
    .json();
    assert_eq!(
        m["coverage"]["approvals"],
        json!([{"reviewer": "bob", "revision": 2, "files_viewed": 0, "files_total": 1,
                "hunks_viewed": 2, "hunks_total": 3}])
    );
    // Viewed state is the viewer's own.
    let mut alice = w.s.login("alice", PASSWORD);
    let page = alice.get(FILES).body;
    assert!(
        !page.contains(r#"aria-pressed="true""#),
        "alice viewed nothing"
    );
}

#[test]
fn conf_review_17_viewed_hunks_carry_over_only_unchanged_git() {
    scenario(Tool::Git, Tool::Git);
}

#[test]
fn conf_review_17_viewed_hunks_carry_over_only_unchanged_jj() {
    scenario(Tool::Jj, Tool::Jj);
}

#[test]
fn conf_review_17_viewed_hunks_carry_over_only_unchanged_mixed() {
    scenario(Tool::Git, Tool::Jj);
}

#[test]
fn conf_review_17_hunk_marks_are_checked() {
    let mut w = World::start();
    w.push(Tool::Git, &FIRST, true);
    let r = api(
        &w.s,
        &w.alice,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Edit f"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    w.wait(1, 1);
    let mut bob = w.s.login("bob", PASSWORD);
    let key = hunks(&mut bob).remove(0).0;
    // A file the merge request doesn't change, and a malformed key.
    let r = submit(
        &mut bob,
        FILES,
        "/alice/demo/merge-requests/1/viewed",
        &[
            ("path", "other.txt"),
            ("viewed", "true"),
            ("hunk", &key),
            ("back", FILES),
        ],
    );
    assert_eq!(r.status, 422, "{}", r.body);
    let r = mark(&mut bob, &[("viewed", "true"), ("hunk", "abc")]);
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(hunks(&mut bob).iter().all(|(_, v)| !v));
    // Marking returns to the hunk.
    let r = mark(
        &mut bob,
        &[("viewed", "true"), ("hunk", &key), ("anchor", "#diff-0-h0")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(
        r.header("location")
            .is_some_and(|l| l.ends_with("/files#diff-0-h0")),
        "{:?}",
        r.header("location")
    );
    // Anything else as an anchor is dropped.
    let r = mark(
        &mut bob,
        &[
            ("viewed", "true"),
            ("hunk", &key),
            ("anchor", "//evil.example"),
        ],
    );
    assert!(r.header("location").is_some_and(|l| l.ends_with("/files")));
}

#[test]
fn conf_review_17_hunk_coverage_is_unknown_past_the_limits() {
    let s = TestServer::start_full(&[DEMO], "[limits]\ndiff_max_lines = 5\n");
    s.create_user("bob", PASSWORD, false);
    let alice = token(&s, "alice");
    let r = api(
        &s,
        &alice,
        "PUT",
        "/alice/demo/collaborators/bob",
        Some(json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let url = s.url(&DEMO);
    git.ok(w, &["init", "-q", "-b", "main"]);
    write(w, "f.txt", &file(&[]));
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "-c", "topic"]);
    write(w, "f.txt", &file(&FIRST));
    git.ok(w, &["commit", "-qam", "Edit f"]);
    git.ok(w, &["push", "-q", &url, "topic"]);
    let r = api(
        &s,
        &alice,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Edit f"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    eventually(&s, "no revision", || {
        api(&s, &alice, "GET", "/alice/demo/merge-requests/1", None).json()["revisions"] == 1
    });
    let r = api(
        &s,
        &token(&s, "bob"),
        "POST",
        "/alice/demo/merge-requests/1/reviews",
        Some(json!({"verdict": "approve"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let m = api(
        &s,
        &alice,
        "GET",
        "/alice/demo/merge-requests/1/mergeability",
        None,
    )
    .json();
    // The diff was cut at 5 lines: the hunks are unknown, not estimated.
    assert_eq!(
        m["coverage"]["approvals"],
        json!([{"reviewer": "bob", "revision": 1, "files_viewed": 0, "files_total": 1,
                "hunks_viewed": null, "hunks_total": null}])
    );
    let page = s
        .login("alice", PASSWORD)
        .get("/alice/demo/merge-requests/1")
        .body;
    assert!(page.contains("(r1, 0 of 1 file viewed)"), "{page}");
}
