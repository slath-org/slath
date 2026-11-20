// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every merge request flow for a Git-made, a jj-made, and a mixed stack
//! (JJ-11, D-44): open from the push link, update, review, apply a
//! suggestion as a commit and folded, update the branch, and merge with
//! each strategy or by pushing the target (with jj for the jj-made stack,
//! with Git otherwise). Each step's push output, API responses, and pages
//! must be the same for the three stacks once names, numbers, and IDs are
//! replaced. No step installs a hook or sets client configuration.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::fork_merge_requests::{api, eventually, token};
use crate::jj::{Jj, jj_binaries};
use crate::tool_parity::normalized;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Made {
    Git,
    Jj,
    Mixed,
}

impl Made {
    const ALL: [Self; 3] = [Self::Git, Self::Jj, Self::Mixed];

    const fn name(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Jj => "jj",
            Self::Mixed => "mixed",
        }
    }
}

struct World {
    s: TestServer,
    alice: String,
    bob: String,
    git: GitClient,
    jj: Jj,
    url: String,
    /// A Git working copy (also moves `main`).
    w: PathBuf,
    /// A jj working copy.
    j: PathBuf,
    _d: tempfile::TempDir,
}

/// Twenty numbered lines of `tag`, with line `edit` changed.
fn lines(tag: &str, edit: Option<usize>) -> String {
    (1..=20)
        .map(|i| {
            if Some(i) == edit {
                format!("{tag} {i}, edited\n")
            } else {
                format!("{tag} {i}\n")
            }
        })
        .collect()
}

/// The `slath:` lines of a push's output, without the prefix.
fn feedback(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("remote: slath: "))
        .map(|l| l.trim_end().to_owned())
        .collect()
}

impl World {
    fn start(version: &str, bin: &Path) -> Self {
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
        let bob = token(&s, "bob");
        let d = tempfile::tempdir().unwrap();
        let w = d.path().join("w");
        let url = s.url(&DEMO);
        let git = GitClient::new();
        std::fs::create_dir_all(&w).unwrap();
        git.ok(&w, &["init", "-q", "-b", "main"]);
        std::fs::write(w.join("README"), "demo\n").unwrap();
        git.ok(&w, &["add", "-A"]);
        git.ok(&w, &["commit", "-q", "-m", "init"]);
        git.ok(&w, &["push", "-q", &url, "main"]);
        let jj = Jj::new(version, bin);
        let j = d.path().join("j");
        jj.ok(d.path(), &["git", "clone", &url, j.to_str().unwrap()]);
        Self {
            s,
            alice,
            bob,
            git,
            jj,
            url,
            w,
            j,
            _d: d,
        }
    }

    fn git_commit(&self, file: &str, text: &str, msg: &str) {
        std::fs::write(self.w.join(file), text).unwrap();
        self.git.ok(&self.w, &["add", "-A"]);
        self.git.ok(&self.w, &["commit", "-q", "-m", msg]);
    }

    /// Pushes `branch` from the Git working copy; the push's feedback.
    fn git_push(&self, branch: &str) -> Vec<String> {
        let out = self
            .git
            .cmd(&self.w)
            .args(["push", "--force", &self.url, branch])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "{err}");
        feedback(&err)
    }

    fn jj_push(&self, branch: &str) -> Vec<String> {
        let out = self
            .jj
            .cmd(&self.j)
            .args(["git", "push", "--bookmark", branch])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "{err}");
        feedback(&err)
    }

    /// Moves the server's `main` with one more Git commit.
    fn advance_main(&self, file: &str) {
        self.git.ok(&self.w, &["switch", "-q", "main"]);
        self.git
            .ok(&self.w, &["pull", "-q", "--rebase", &self.url, "main"]);
        self.git_commit(file, "main\n", &format!("Main: {file}"));
        self.git.ok(&self.w, &["push", "-q", &self.url, "main"]);
    }

    /// A stack of two changes on `branch` ("Add a", "Add b"), made the way
    /// `made` says: the mixed one has a Git commit under a jj change.
    fn stack(&self, made: Made, branch: &str) -> Vec<String> {
        let (a, b) = (lines("a", None), lines("b", None));
        match made {
            Made::Git => {
                self.git.ok(&self.w, &["switch", "-q", "main"]);
                self.git
                    .ok(&self.w, &["pull", "-q", "--rebase", &self.url, "main"]);
                self.git.ok(&self.w, &["switch", "-q", "-c", branch]);
                self.git_commit(&format!("{branch}-a.txt"), &a, "Add a");
                self.git_commit(&format!("{branch}-b.txt"), &b, "Add b");
                self.git_push(branch)
            }
            Made::Jj => {
                self.jj.ok(&self.j, &["git", "fetch"]);
                self.jj.ok(&self.j, &["new", "main@origin", "-m", "Add a"]);
                std::fs::write(self.j.join(format!("{branch}-a.txt")), &a).unwrap();
                self.jj.ok(&self.j, &["new", "-m", "Add b"]);
                std::fs::write(self.j.join(format!("{branch}-b.txt")), &b).unwrap();
                self.jj
                    .ok(&self.j, &["bookmark", "create", branch, "-r", "@"]);
                self.jj_push(branch)
            }
            Made::Mixed => {
                self.git.ok(&self.w, &["switch", "-q", "main"]);
                self.git
                    .ok(&self.w, &["pull", "-q", "--rebase", &self.url, "main"]);
                self.git.ok(&self.w, &["switch", "-q", "-c", branch]);
                self.git_commit(&format!("{branch}-a.txt"), &a, "Add a");
                self.git_push(branch);
                self.jj.ok(&self.j, &["git", "fetch"]);
                let remote = format!("{branch}@origin");
                self.jj.ok(&self.j, &["bookmark", "track", &remote]);
                self.jj.ok(&self.j, &["new", branch, "-m", "Add b"]);
                std::fs::write(self.j.join(format!("{branch}-b.txt")), &b).unwrap();
                self.jj.ok(&self.j, &["bookmark", "set", branch, "-r", "@"]);
                self.jj_push(branch)
            }
        }
    }

    /// One more change ("Add c") on top, with the tool that made the top.
    fn add_change(&self, made: Made, branch: &str) -> Vec<String> {
        let c = lines("c", None);
        if made == Made::Git {
            self.git.ok(&self.w, &["switch", "-q", branch]);
            self.git_commit(&format!("{branch}-c.txt"), &c, "Add c");
            self.git_push(branch)
        } else {
            self.jj.ok(&self.j, &["new", branch, "-m", "Add c"]);
            std::fs::write(self.j.join(format!("{branch}-c.txt")), &c).unwrap();
            self.jj.ok(&self.j, &["bookmark", "set", branch, "-r", "@"]);
            self.jj_push(branch)
        }
    }

    fn get(&self, n: i64, tail: &str) -> Value {
        let r = api(
            &self.s,
            &self.alice,
            "GET",
            &format!("/alice/demo/merge-requests/{n}{tail}"),
            None,
        );
        assert_eq!(r.status, 200, "{tail}: {}", r.body);
        r.json()
    }

    fn post(&self, t: &str, n: i64, tail: &str, body: Value) -> Value {
        let r = api(
            &self.s,
            t,
            "POST",
            &format!("/alice/demo/merge-requests/{n}{tail}"),
            Some(body),
        );
        assert!(r.status < 300, "{tail}: {}", r.body);
        r.json()
    }

    fn wait_revisions(&self, n: i64, revisions: i64) {
        eventually(&self.s, &format!("!{n}: no revision {revisions}"), || {
            self.get(n, "")["revisions"] == revisions
        });
    }

    /// Moves the server's `main` to `branch` by a push: with jj for a
    /// jj-made stack, with Git otherwise.
    fn land_by_push(&self, made: Made, branch: &str) {
        if made == Made::Jj {
            self.jj.ok(&self.j, &["git", "fetch"]);
            self.jj.ok(
                &self.j,
                &["bookmark", "set", "main", "-r", &format!("{branch}@origin")],
            );
            self.jj_push("main");
        } else {
            let remote = format!("refs/remotes/s/{branch}");
            self.git.ok(
                &self.w,
                &["fetch", "-q", &self.url, &format!("+{branch}:{remote}")],
            );
            self.git.ok(
                &self.w,
                &[
                    "push",
                    "-q",
                    &self.url,
                    &format!("{remote}:refs/heads/main"),
                ],
            );
        }
    }

    fn head(&self, n: i64) -> String {
        self.get(n, "")["head_oid"].as_str().unwrap().to_owned()
    }

    fn page(&self, n: i64) -> String {
        let mut web = self.s.login("alice", PASSWORD);
        let r = web.get(&format!("/alice/demo/merge-requests/{n}"));
        assert_eq!(r.status, 200);
        r.body
    }

    /// A line comment by bob with a suggestion; the comment's ID.
    fn suggest(&self, n: i64, path: &str, line: i32, text: &str) -> String {
        let v = self.post(
            &self.bob,
            n,
            "/threads",
            json!({"body": format!("```suggestion\n{text}\n```"), "path": path,
                   "side": "new", "line_start": line, "line_end": line}),
        );
        v["comments"][0]["id"].as_str().unwrap().to_owned()
    }
}

/// What a flow shows at each step, for one stack.
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn add(&mut self, step: &str, value: &str, branch: &str, n: i64) {
        let value = value.replace(&format!("\"number\":{n}"), "\"number\":N");
        self.0
            .push((step.to_owned(), normalized(&value, branch, n)));
    }
}

/// Without times, IDs, and who-did-what fields that legitimately differ.
fn strip(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        for k in ["created_at", "updated_at", "merged_at", "closed_at", "id"] {
            o.remove(k);
        }
        for (_, x) in o.iter_mut() {
            *x = strip(x.take());
        }
    } else if let Some(a) = v.as_array_mut() {
        for x in a.iter_mut() {
            *x = strip(x.take());
        }
    }
    v
}

fn run(world: &World, made: Made, strategy: &str) -> Record {
    let branch = format!("{}-{strategy}", made.name());
    let mut rec = Record::default();
    // Open from the push link.
    let fb = world.stack(made, &branch);
    let link = fb
        .iter()
        .find_map(|l| l.trim().strip_prefix(&world.s.base()))
        .unwrap_or_else(|| panic!("{branch}: no link in {fb:?}"))
        .to_owned();
    let mut web = world.s.login("alice", PASSWORD);
    let compare = web.get(&link);
    assert_eq!(compare.status, 200, "{link}");
    let r = api(
        &world.s,
        &world.alice,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_branch": branch, "title": "Add a and b"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    rec.add("push link", &fb.join("\n"), &branch, n);
    world.wait_revisions(n, 1);
    rec.add(
        "stack",
        &strip(world.get(n, "/stack")).to_string(),
        &branch,
        n,
    );

    // Update: a third change.
    let fb = world.add_change(made, &branch);
    rec.add("update feedback", &fb.join("\n"), &branch, n);
    world.wait_revisions(n, 2);
    rec.add(
        "updated stack",
        &strip(world.get(n, "/stack")).to_string(),
        &branch,
        n,
    );

    // Review.
    world.post(&world.bob, n, "/reviews", json!({"verdict": "approve"}));
    rec.add(
        "mergeability",
        &strip(world.get(n, "/mergeability")).to_string(),
        &branch,
        n,
    );

    // A suggestion as a commit, then one folded into its change.
    let c = world.suggest(n, &format!("{branch}-c.txt"), 2, "C 2");
    let head = world.head(n);
    world.post(
        &world.alice,
        n,
        "/suggestions/apply",
        json!({"comments": [c], "expected_head_oid": head}),
    );
    world.wait_revisions(n, 3);
    rec.add(
        "commit suggestion",
        &strip(world.get(n, "/stack")).to_string(),
        &branch,
        n,
    );
    let f = world.suggest(n, &format!("{branch}-a.txt"), 3, "A 3");
    let head = world.head(n);
    world.post(
        &world.alice,
        n,
        "/suggestions/apply",
        json!({"comments": [f], "mode": "fold", "expected_head_oid": head}),
    );
    world.wait_revisions(n, 4);
    rec.add(
        "folded suggestion",
        &strip(world.get(n, "/stack")).to_string(),
        &branch,
        n,
    );

    // Update branch: merged for a merge commit, else rebased.
    world.advance_main(&format!("{branch}.txt"));
    let method = if strategy == "merge-commit" {
        "merge"
    } else {
        "rebase"
    };
    let head = world.head(n);
    world.post(
        &world.alice,
        n,
        "/update-branch",
        json!({"method": method, "expected_head_oid": head}),
    );
    world.wait_revisions(n, 5);
    rec.add(
        "updated branch",
        &strip(world.get(n, "/stack")).to_string(),
        &branch,
        n,
    );
    world.post(&world.bob, n, "/reviews", json!({"verdict": "approve"}));
    rec.add(
        "mergeability before merge",
        &strip(world.get(n, "/mergeability")).to_string(),
        &branch,
        n,
    );

    // Merge, in Slath or by pushing the target.
    if strategy == "push" {
        world.land_by_push(made, &branch);
        eventually(&world.s, &format!("!{n}: not merged by the push"), || {
            world.get(n, "")["state"] == "merged"
        });
    } else {
        let head = world.head(n);
        let merged = world.post(
            &world.alice,
            n,
            "/merge",
            json!({"strategy": strategy, "expected_head_oid": head}),
        );
        rec.add("merge", &strip(merged).to_string(), &branch, n);
    }
    rec.add("merged", &strip(world.get(n, "")).to_string(), &branch, n);
    rec.add("page", &world.page(n), &branch, n);
    rec
}

fn first_difference(a: &str, b: &str) -> String {
    let at = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    let ctx = |p: &str| {
        p.chars()
            .skip(at.saturating_sub(200))
            .take(400)
            .collect::<String>()
    };
    format!("\n  {}\n  {}", ctx(a), ctx(b))
}

#[test]
fn conf_jj_11_every_flow_for_git_jj_and_mixed_stacks() {
    let Some((version, bin)) = jj_binaries().into_iter().last() else {
        eprintln!("no jj: skipping");
        return;
    };
    let world = World::start(&version, &bin);
    for strategy in ["fast-forward", "rebase", "merge-commit", "squash", "push"] {
        let records: Vec<(Made, Record)> = Made::ALL
            .iter()
            .map(|&made| (made, run(&world, made, strategy)))
            .collect();
        let (base_made, base) = &records[0];
        for (made, rec) in &records[1..] {
            for ((step, a), (_, b)) in base.0.iter().zip(&rec.0) {
                assert!(
                    a == b,
                    "{strategy}, {step}: {base_made:?} and {made:?} differ:{}",
                    first_difference(a, b)
                );
            }
        }
    }
}
