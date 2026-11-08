// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Server-side merges (REVIEW-05, GIT-05): headers and authors survive a
//! rebase byte for byte, signatures are dropped, Git-made commits gain
//! their change ID (and jj sees the UI's IDs), every strategy, conflicts,
//! stale heads, and concurrent target updates.

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use serde_json::{Value, json};
use slath_git::commit::RawCommit;
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::jj::{Jj, jj_binaries};

struct Ctx {
    s: TestServer,
    token: String,
    url: String,
    git: GitClient,
}

impl Ctx {
    fn start() -> Self {
        let s = TestServer::start();
        let mut alice = s.login("alice", PASSWORD);
        let token = slath_tests::http::create_token(&mut alice, "m", &["repo:write", "mrs:write"]);
        Self {
            url: s.url(&DEMO),
            s,
            token,
            git: GitClient::new(),
        }
    }

    fn api(&self, method: &str, path: &str, body: Option<Value>) -> slath_tests::http::Resp {
        self.s.client().api(
            method,
            &format!("/api/v1/repos/alice/demo{path}"),
            Some(&self.token),
            body.as_ref(),
        )
    }

    fn open_mr(&self, branch: &str) -> (i64, String) {
        let r = self.api(
            "POST",
            "/merge-requests",
            Some(json!({"source_branch": branch, "title": format!("Merge {branch}")})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        (n, self.head(n, 1))
    }

    /// The head once the merge request has `k` revisions.
    fn head(&self, n: i64, k: i64) -> String {
        let start = std::time::Instant::now();
        loop {
            let m = self
                .api("GET", &format!("/merge-requests/{n}"), None)
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

    fn merge(&self, n: i64, head: &str, strategy: &str) -> slath_tests::http::Resp {
        self.api(
            "POST",
            &format!("/merge-requests/{n}/merge"),
            Some(json!({"strategy": strategy, "expected_head_oid": head, "delete_source": false})),
        )
    }

    fn push(&self, w: &Path, refspec: &str) {
        self.git
            .ok(w, &["push", "-q", "--force", &self.url, refspec]);
    }

    fn server(&self, args: &[&str]) -> String {
        self.git.ok(&self.s.repo_path(&DEMO), args)
    }

    fn raw(&self, rev: &str) -> RawCommit {
        let out = self
            .git
            .cmd(&self.s.repo_path(&DEMO))
            .args(["cat-file", "commit", rev])
            .output()
            .unwrap();
        RawCommit::parse(out.stdout).unwrap()
    }
}

fn seed(c: &Ctx, root: &Path) -> std::path::PathBuf {
    let w = root.join("w");
    std::fs::create_dir_all(&w).unwrap();
    c.git.ok(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "demo\n").unwrap();
    c.git.ok(&w, &["add", "."]);
    c.git.ok(&w, &["commit", "-q", "-m", "init"]);
    c.push(&w, "main");
    w
}

fn commit(git: &GitClient, w: &Path, file: &str, text: &str, msg: &str) -> String {
    std::fs::write(w.join(file), text).unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", msg]);
    git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// A commit with extra headers (a signature, a change ID, an unknown one).
fn craft(git: &GitClient, w: &Path, parent: &str, extra: &str, file: &str) -> String {
    std::fs::write(w.join(file), "crafted\n").unwrap();
    git.ok(w, &["add", "-A"]);
    let tree = git.ok(w, &["write-tree"]).trim().to_owned();
    let raw = format!(
        "tree {tree}\nparent {parent}\nauthor Ann Author <ann@example.org> 1700000000 +0200\n\
         committer Ann Author <ann@example.org> 1700000000 +0200\n{extra}\nCrafted\n"
    );
    let mut child = git
        .cmd(w)
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
    git.ok(w, &["reset", "-q", "--hard", &oid]);
    oid
}

/// Moves `main` on the server past the merge request's base.
fn advance_main(c: &Ctx, w: &Path, file: &str) {
    c.git.ok(w, &["switch", "-q", "main"]);
    c.git.ok(w, &["pull", "-q", "--rebase", &c.url, "main"]);
    commit(&c.git, w, file, "more\n", &format!("Main gets {file}"));
    c.push(w, "main");
}

#[test]
fn conf_review_05_rebase_keeps_headers_and_authors_and_drops_signatures() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());
    let main = c.git.ok(&w, &["rev-parse", "main"]).trim().to_owned();
    c.git.ok(&w, &["switch", "-q", "-c", "signed"]);
    let id = "kkkkkkkkkkkkkkkkllllllllllllllll";
    let crafted = craft(
        &c.git,
        &w,
        &main,
        &format!(
            "gpgsig -----BEGIN PGP SIGNATURE-----\n fake\n -----END PGP SIGNATURE-----\nchange-id {id}\nx-tool keep me\n"
        ),
        "a.txt",
    );
    c.push(&w, "HEAD:refs/heads/signed");
    let (n, head) = c.open_mr("signed");
    advance_main(&c, &w, "m.txt");
    // A stale head is refused.
    let r = c.merge(n, &"0".repeat(40), "rebase");
    assert_eq!(r.status, 409, "{}", r.body);
    let r = c.merge(n, &head, "rebase");
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["state"], "merged");
    let landed = c.raw("refs/heads/main");
    let original = c.raw(&crafted);
    assert_eq!(landed.change_id(), Some(id));
    assert_eq!(landed.header(b"author"), original.header(b"author"));
    assert_eq!(landed.header(b"x-tool"), Some(&b"keep me"[..]));
    assert!(landed.header(b"gpgsig").is_none());
    assert_eq!(landed.message, original.message);
    c.server(&["fsck", "--strict", "--no-dangling"]);
    // Merged twice: no.
    assert_eq!(c.merge(n, &head, "rebase").status, 409);
}

#[test]
fn conf_review_05_git_made_commits_gain_their_change_ids_and_jj_sees_them() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());
    c.git.ok(&w, &["switch", "-q", "-c", "stack"]);
    commit(&c.git, &w, "a.txt", "a\n", "Add a");
    commit(&c.git, &w, "b.txt", "b\n", "Add b");
    c.push(&w, "stack");
    let (n, head) = c.open_mr("stack");
    let ids: Vec<String> = c
        .api("GET", &format!("/merge-requests/{n}/stack"), None)
        .json()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["change_id"].as_str().unwrap().to_owned())
        .collect();
    advance_main(&c, &w, "m.txt");
    let r = c.merge(n, &head, "rebase");
    assert_eq!(r.status, 200, "{}", r.body);
    // Both rewritten commits carry the display IDs.
    assert_eq!(
        c.raw("refs/heads/main~1").change_id(),
        Some(ids[0].as_str())
    );
    assert_eq!(c.raw("refs/heads/main").change_id(), Some(ids[1].as_str()));
    // jj shows the same IDs after fetching.
    if let Some((version, bin)) = jj_binaries().into_iter().last() {
        let jj = Jj::new(&version, &bin);
        let j = d.path().join("jj");
        jj.ok(d.path(), &["git", "clone", &c.url, j.to_str().unwrap()]);
        assert_eq!(
            jj.show(&j, "main@origin-", "change_id"),
            ids[0],
            "{version}"
        );
        assert_eq!(jj.show(&j, "main@origin", "change_id"), ids[1], "{version}");
    }
    c.server(&["fsck", "--strict", "--no-dangling"]);
}

#[test]
fn conf_review_05_every_strategy() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());

    // Fast-forward: the head itself; refused once main moved.
    c.git.ok(&w, &["switch", "-q", "-c", "ff", "main"]);
    let ff_head = commit(&c.git, &w, "ff.txt", "ff\n", "Fast");
    c.push(&w, "ff");
    let (n, head) = c.open_mr("ff");
    assert_eq!(c.merge(n, &head, "fast-forward").status, 200);
    assert_eq!(c.server(&["rev-parse", "main"]).trim(), ff_head);

    c.git
        .ok(&w, &["fetch", "-q", &c.url, "+main:refs/remotes/s/main"]);
    c.git.ok(&w, &["switch", "-q", "-c", "behind", "s/main~1"]);
    commit(&c.git, &w, "behind.txt", "b\n", "Behind");
    c.push(&w, "behind");
    let (n, head) = c.open_mr("behind");
    let r = c.merge(n, &head, "fast-forward");
    assert_eq!(r.status, 422, "{}", r.body);

    // Merge commit: two parents, the default message.
    let r = c.merge(n, &head, "merge-commit");
    assert_eq!(r.status, 200, "{}", r.body);
    let mc = c.raw("refs/heads/main");
    assert_eq!(mc.parents().count(), 2);
    assert!(String::from_utf8_lossy(&mc.message).starts_with(&format!("Merge !{n}: Merge behind")));

    // Squash: one commit, the first commit's author, the change's ID.
    c.git
        .ok(&w, &["fetch", "-q", &c.url, "+main:refs/remotes/s/main"]);
    c.git.ok(&w, &["switch", "-q", "-C", "sq", "s/main~1"]);
    commit(&c.git, &w, "s.txt", "one\n", "Squash one");
    c.push(&w, "sq");
    let (n, head) = c.open_mr("sq");
    let id = c
        .api("GET", &format!("/merge-requests/{n}/stack"), None)
        .json()["entries"][0]["change_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = c.api(
        "POST",
        &format!("/merge-requests/{n}/merge"),
        Some(json!({"strategy": "squash", "expected_head_oid": head, "commit_message": "Squashed\n\nBody.", "delete_source": true})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let sq = c.raw("refs/heads/main");
    assert_eq!(sq.parents().count(), 1);
    assert_eq!(sq.change_id(), Some(id.as_str()));
    assert_eq!(String::from_utf8_lossy(&sq.message), "Squashed\n\nBody.\n");
    // The source branch is gone.
    let refs = c.server(&["for-each-ref", "--format=%(refname)", "refs/heads/"]);
    assert!(!refs.contains("refs/heads/sq"), "{refs}");
    c.server(&["fsck", "--strict", "--no-dangling"]);
}

#[test]
fn conf_review_05_conflicts_and_requirements_block_merging() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());
    c.git.ok(&w, &["switch", "-q", "-c", "clash", "main"]);
    commit(&c.git, &w, "README", "theirs\n", "Change README");
    c.push(&w, "clash");
    let (n, head) = c.open_mr("clash");
    c.git.ok(&w, &["switch", "-q", "main"]);
    commit(&c.git, &w, "README", "ours\n", "Change README on main");
    c.push(&w, "main");
    let m = c
        .api("GET", &format!("/merge-requests/{n}/mergeability"), None)
        .json();
    let conflicts = m["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "conflicts")
        .unwrap()
        .clone();
    assert_eq!(conflicts["state"], "unmet", "{m}");
    assert!(
        conflicts["reason"].as_str().unwrap().contains("README"),
        "{m}"
    );
    let r = c.merge(n, &head, "rebase");
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("README"), "{}", r.body);

    // A draft isn't merged.
    c.git.ok(&w, &["switch", "-q", "-c", "draft", "main"]);
    commit(&c.git, &w, "d.txt", "d\n", "Draft");
    c.push(&w, "draft");
    let (n, head) = c.open_mr("draft");
    c.api(
        "PATCH",
        &format!("/merge-requests/{n}"),
        Some(json!({"draft": true})),
    );
    let r = c.merge(n, &head, "rebase");
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("draft"), "{}", r.body);
}

#[test]
fn conf_git_05_server_merges_and_concurrent_pushes_lose_nothing() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());
    let mut mrs = Vec::new();
    for i in 0..3 {
        c.git
            .ok(&w, &["switch", "-q", "-c", &format!("p{i}"), "main"]);
        commit(
            &c.git,
            &w,
            &format!("p{i}.txt"),
            "p\n",
            &format!("Part {i}"),
        );
        c.push(&w, &format!("p{i}"));
        mrs.push(c.open_mr(&format!("p{i}")));
    }
    // Someone keeps pushing to main while the merges run.
    let pusher = {
        let url = c.url.clone();
        let dir = d.path().join("pusher");
        std::thread::spawn(move || {
            let git = GitClient::new();
            git.ok(
                dir.parent().unwrap(),
                &["clone", "-q", &url, dir.to_str().unwrap()],
            );
            let mut pushed = Vec::new();
            for i in 0..5 {
                loop {
                    git.ok(&dir, &["pull", "-q", "--rebase", &url, "main"]);
                    std::fs::write(dir.join(format!("push{i}.txt")), "x\n").unwrap();
                    git.ok(&dir, &["add", "-A"]);
                    if git
                        .cmd(&dir)
                        .args(["commit", "-q", "-m", &format!("Push {i}")])
                        .status()
                        .unwrap()
                        .success()
                    {
                        // committed
                    }
                    let out = git
                        .cmd(&dir)
                        .args(["push", "-q", &url, "HEAD:main"])
                        .output()
                        .unwrap();
                    if out.status.success() {
                        pushed.push(git.ok(&dir, &["rev-parse", "HEAD"]).trim().to_owned());
                        break;
                    }
                }
            }
            pushed
        })
    };
    let mut merged = Vec::new();
    for (n, head) in &mrs {
        let r = c.merge(*n, head, "rebase");
        assert!(r.status == 200 || r.status == 409, "{}", r.body);
        if r.status == 200 {
            merged.push(*n);
        }
    }
    let pushed = pusher.join().unwrap();
    // Every push survived: its commit is an ancestor of main.
    for p in &pushed {
        c.server(&["merge-base", "--is-ancestor", p, "refs/heads/main"]);
    }
    // Every merged part is on main.
    let files = c.server(&["ls-tree", "--name-only", "refs/heads/main"]);
    for (i, (n, _)) in mrs.iter().enumerate() {
        if merged.contains(n) {
            assert!(files.contains(&format!("p{i}.txt")), "!{n}: {files}");
        }
    }
    assert!(!merged.is_empty(), "nothing merged");
    c.server(&["fsck", "--strict", "--no-dangling"]);
}

#[test]
fn squash_messages_are_scanned_for_secrets() {
    let c = Ctx::start();
    c.s.db
        .execute("UPDATE repositories SET push_protection = 'block'");
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());
    c.git.ok(&w, &["switch", "-q", "-c", "sq"]);
    commit(&c.git, &w, "s.txt", "s\n", "Squash me");
    c.push(&w, "sq");
    let (n, head) = c.open_mr("sq");
    let (_, secret) = slath_core::secret_scan::samples()
        .into_iter()
        .find(|(r, _)| *r == "github-token")
        .unwrap();
    let r = c.api(
        "POST",
        &format!("/merge-requests/{n}/merge"),
        Some(json!({"strategy": "squash", "expected_head_oid": head, "commit_message": format!("Squash\n\ntoken {secret}")})),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.body.contains("looks like a secret") && !r.body.contains(&secret),
        "{}",
        r.body
    );
    assert_eq!(c.merge(n, &head, "squash").status, 200);
}
