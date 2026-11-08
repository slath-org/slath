// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Change identity in merge requests with real Git and jj clients: rewrites
//! keep their change (JJ-02), content pairing (JJ-03), mixed Git and jj
//! teams without setup (JJ-09), and hostile or conflicting IDs (JJ-10).

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::jj::{Jj, jj_binaries};

/// A server with alice/demo, `main` pushed, and an API token.
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
        let token =
            slath_tests::http::create_token(&mut alice, "ids", &["repo:write", "mrs:write"]);
        let url = s.url(&DEMO);
        Self {
            s,
            token,
            url,
            git: GitClient::new(),
        }
    }

    fn api(&self, method: &str, path: &str, body: Option<&Value>) -> slath_tests::http::Resp {
        self.s.client().api(
            method,
            &format!("/api/v1/repos/alice/demo{path}"),
            Some(&self.token),
            body,
        )
    }

    fn open_mr(&self, branch: &str) -> i64 {
        let r = self.api(
            "POST",
            "/merge-requests",
            Some(&json!({"source_branch": branch, "title": branch})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        r.json()["number"].as_i64().unwrap()
    }

    /// The stack once the merge request has `revisions` revisions.
    fn stack(&self, n: i64, revisions: i64) -> Value {
        let start = std::time::Instant::now();
        loop {
            let m = self
                .api("GET", &format!("/merge-requests/{n}"), None)
                .json();
            if m["revisions"] == revisions {
                break;
            }
            assert!(
                start.elapsed().as_secs() < 30,
                "!{n}: no revision {revisions}: {m}\n{}",
                self.s.log()
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let r = self.api("GET", &format!("/merge-requests/{n}/stack"), None);
        assert_eq!(r.status, 200, "{}", r.body);
        r.json()
    }

    /// A Git working copy of the server's repository with `main`.
    fn clone(&self, dir: &Path) {
        let out = self
            .git
            .cmd(dir.parent().unwrap())
            .args(["clone", "--quiet", &self.url, dir.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn push(&self, dir: &Path, refspec: &str) {
        self.git
            .ok(dir, &["push", "--quiet", "--force", &self.url, refspec]);
    }
}

/// A working copy with an initial `main` on the server.
fn seed(c: &Ctx, root: &Path) -> std::path::PathBuf {
    let w = root.join("seed");
    std::fs::create_dir_all(&w).unwrap();
    c.git.ok(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "demo\n").unwrap();
    c.git.ok(&w, &["add", "."]);
    c.git.ok(&w, &["commit", "-q", "-m", "init"]);
    c.push(&w, "main");
    w
}

/// Twenty numbered lines, with line `edit` replaced if given (big enough
/// for content pairing).
fn lines(tag: &str, edit: Option<usize>) -> String {
    (1..=20)
        .map(|i| {
            if Some(i) == edit {
                format!("{tag} line {i}, edited\n")
            } else {
                format!("{tag} line {i}\n")
            }
        })
        .collect()
}

fn commit_file(git: &GitClient, w: &Path, file: &str, text: &str, msg: &str) -> String {
    std::fs::write(w.join(file), text).unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", msg]);
    git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// Writes a commit object with exactly these headers after the parent,
/// returning its ID (Git can't make such commits itself).
fn craft(
    git: &GitClient,
    w: &Path,
    parent: &str,
    extra_headers: &str,
    file: &str,
    msg: &str,
) -> String {
    std::fs::write(w.join(file), format!("{msg}\n")).unwrap();
    git.ok(w, &["add", "-A"]);
    let tree = git.ok(w, &["write-tree"]).trim().to_owned();
    let raw = format!(
        "tree {tree}\nparent {parent}\nauthor A <a@example.org> 1700000000 +0000\n\
         committer A <a@example.org> 1700000000 +0000\n{extra_headers}\n{msg}\n"
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
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let oid = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    git.ok(w, &["reset", "-q", "--hard", &oid]);
    oid
}

fn entries(stack: &Value) -> &Vec<Value> {
    stack["entries"].as_array().unwrap()
}

fn ids(stack: &Value) -> Vec<String> {
    entries(stack)
        .iter()
        .map(|e| e["change_id"].as_str().unwrap_or("").to_owned())
        .collect()
}

fn bindings(stack: &Value) -> Vec<(String, String)> {
    entries(stack)
        .iter()
        .map(|e| {
            (
                e["binding"].as_str().unwrap().to_owned(),
                e["reason"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect()
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_owned(), b.to_owned())
}

#[test]
fn conf_jj_02_git_rewrites_keep_their_change() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());

    // A Change-Id trailer survives an amend that changes the content.
    let trailer = "Change-Id: I0123456789abcdef0123456789abcdef01234567";
    c.git.ok(&w, &["switch", "-q", "-c", "gerrit", "main"]);
    commit_file(
        &c.git,
        &w,
        "g.txt",
        &lines("g", None),
        &format!("Add g\n\n{trailer}"),
    );
    c.push(&w, "gerrit");
    let n = c.open_mr("gerrit");
    let first = c.stack(n, 1);
    assert_eq!(bindings(&first), vec![pair("new", "")]);
    std::fs::write(w.join("g.txt"), lines("g", Some(3))).unwrap();
    c.git.ok(
        &w,
        &[
            "commit",
            "-qa",
            "--amend",
            "-m",
            &format!("Add g, fixed\n\n{trailer}"),
        ],
    );
    c.push(&w, "gerrit");
    let second = c.stack(n, 2);
    assert_eq!(bindings(&second), vec![pair("confirmed", "alias")]);
    assert_eq!(ids(&second), ids(&first));
    assert_eq!(entries(&second)[0]["earlier_versions"], 1);

    // A plain rebase of a commit without any ID: the identical patch.
    c.git.ok(&w, &["switch", "-q", "-c", "plain", "main"]);
    commit_file(&c.git, &w, "p.txt", &lines("p", None), "Add p");
    c.push(&w, "plain");
    let n = c.open_mr("plain");
    let first = c.stack(n, 1);
    c.git.ok(&w, &["switch", "-q", "main"]);
    commit_file(&c.git, &w, "other.txt", "other\n", "Other work");
    c.push(&w, "main");
    c.git.ok(&w, &["switch", "-q", "plain"]);
    c.git.ok(&w, &["rebase", "-q", "main"]);
    c.push(&w, "plain");
    let second = c.stack(n, 2);
    assert_eq!(bindings(&second), vec![pair("confirmed", "patch")]);
    assert_eq!(ids(&second), ids(&first));
}

#[test]
fn conf_jj_03_content_pairing_split_and_join() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());
    c.git.ok(&w, &["switch", "-q", "-c", "pairing", "main"]);
    commit_file(&c.git, &w, "a.txt", &lines("a", None), "Add a");
    c.push(&w, "pairing");
    let n = c.open_mr("pairing");
    let first = c.stack(n, 1);

    // Rewritten with a new message and one line changed, without any ID:
    // paired by content, marked inferred. The message plays no part.
    c.git.ok(&w, &["reset", "-q", "--hard", "main"]);
    commit_file(
        &c.git,
        &w,
        "a.txt",
        &lines("a", Some(7)),
        "Something else entirely",
    );
    c.push(&w, "pairing");
    let second = c.stack(n, 2);
    assert_eq!(bindings(&second), vec![pair("inferred", "content")]);
    assert_eq!(ids(&second), ids(&first));

    // Split makes it a new change; join brings it back.
    let r = c.api(
        "POST",
        &format!("/merge-requests/{n}/stack/0/bind"),
        Some(&json!({"change": null})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let split = r.json();
    assert_eq!(bindings(&split), vec![pair("new", "split")]);
    assert_ne!(ids(&split), ids(&first));
    assert_eq!(split["removed"], json!(ids(&first)));
    let r = c.api(
        "POST",
        &format!("/merge-requests/{n}/stack/0/bind"),
        Some(&json!({"change": ids(&first)[0]})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let joined = r.json();
    assert_eq!(bindings(&joined), vec![pair("confirmed", "join")]);
    assert_eq!(ids(&joined), ids(&first));
    assert_eq!(joined["removed"], json!([]));

    // An unrelated rewrite is a new change, and the old one is removed.
    c.git.ok(&w, &["reset", "-q", "--hard", "main"]);
    commit_file(&c.git, &w, "b.txt", &lines("b", None), "Add a");
    c.push(&w, "pairing");
    let third = c.stack(n, 3);
    assert_eq!(bindings(&third), vec![pair("new", "")]);
    assert_eq!(third["removed"], json!(ids(&first)));

    // Someone who can't push the source can't rebind.
    c.s.create_user("bob", PASSWORD, false);
    let mut bob = c.s.login("bob", PASSWORD);
    let bob_token = slath_tests::http::create_token(&mut bob, "b", &["mrs:write"]);
    let r = c.s.client().api(
        "POST",
        &format!("/api/v1/repos/alice/demo/merge-requests/{n}/stack/0/bind"),
        Some(&bob_token),
        Some(&json!({"change": null})),
    );
    assert!(matches!(r.status, 403 | 404), "{}", r.body);
}

#[test]
fn conf_jj_09_mixed_git_and_jj_without_setup() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());
    for (version, bin) in jj_binaries() {
        let v = version.replace(['.', ' '], "_");
        let jj = Jj::new(&version, &bin);
        let jdir = d.path().join(format!("jj-{v}"));
        let g = d.path().join(format!("git-{v}"));
        c.clone(&g);

        // A Git commit, edited in jj: the predicted ID becomes the header,
        // the change continues, and its ID is the one jj shows.
        let branch = format!("git-made-{v}");
        c.git
            .ok(&g, &["switch", "-q", "-c", &branch, "origin/main"]);
        let x = commit_file(&c.git, &g, "x.txt", &lines("x", None), "Add x");
        c.push(&g, &branch);
        let n = c.open_mr(&branch);
        let first = c.stack(n, 1);
        jj.ok(d.path(), &["git", "clone", &c.url, jdir.to_str().unwrap()]);
        let shown = jj.show(&jdir, &format!("{branch}@origin"), "change_id");
        assert_eq!(
            ids(&first),
            vec![shown.clone()],
            "{version}: display ID of a Git commit"
        );
        assert_eq!(
            slath_core::change_identity::predict(&x).unwrap(),
            shown,
            "{version}: prediction"
        );
        jj.ok(&jdir, &["bookmark", "track", &format!("{branch}@origin")]);
        jj.ok(&jdir, &["new", &branch]);
        std::fs::write(jdir.join("x.txt"), lines("x", Some(2))).unwrap();
        jj.ok(&jdir, &["squash"]);
        jj.ok(&jdir, &["git", "push", "--bookmark", &branch]);
        let second = c.stack(n, 2);
        assert_eq!(
            bindings(&second),
            vec![pair("confirmed", "alias")],
            "{version}"
        );
        assert_eq!(ids(&second), vec![shown.clone()], "{version}");

        // Back to Git: an amend keeps jj's header, the change continues.
        c.git.ok(&g, &["fetch", "-q", "origin"]);
        c.git
            .ok(&g, &["reset", "-q", "--hard", &format!("origin/{branch}")]);
        std::fs::write(g.join("x.txt"), lines("x", Some(9))).unwrap();
        c.git.ok(&g, &["commit", "-qa", "--amend", "--no-edit"]);
        c.push(&g, &branch);
        assert_eq!(
            ids(&c.stack(n, 3)),
            vec![shown],
            "{version}: alternating pushes"
        );

        // A jj stack, rebased with Git (headers dropped): identical patches.
        let stackb = format!("jj-made-{v}");
        jj.ok(&jdir, &["new", "main@origin", "-m", "Add y"]);
        std::fs::write(jdir.join("y.txt"), lines("y", None)).unwrap();
        jj.ok(&jdir, &["new", "-m", "Add z"]);
        std::fs::write(jdir.join("z.txt"), lines("z", None)).unwrap();
        jj.ok(&jdir, &["bookmark", "create", &stackb, "-r", "@"]);
        jj.ok(&jdir, &["git", "push", "--bookmark", &stackb]);
        let n = c.open_mr(&stackb);
        let first = c.stack(n, 1);
        let want = vec![
            jj.show(&jdir, &format!("{stackb}-"), "change_id"),
            jj.show(&jdir, &stackb, "change_id"),
        ];
        assert_eq!(ids(&first), want, "{version}: display IDs of a jj stack");
        c.git.ok(&w, &["switch", "-q", "main"]);
        c.git.ok(&w, &["pull", "-q", "--rebase", &c.url, "main"]);
        commit_file(
            &c.git,
            &w,
            &format!("main-{v}.txt"),
            "more\n",
            "Advance main",
        );
        c.push(&w, "main");
        c.git.ok(&g, &["fetch", "-q", "origin"]);
        c.git.ok(
            &g,
            &["switch", "-q", "-C", &stackb, &format!("origin/{stackb}")],
        );
        c.git
            .ok(&g, &["rebase", "-q", "--force-rebase", "origin/main"]);
        c.push(&g, &stackb);
        let second = c.stack(n, 2);
        assert_eq!(
            bindings(&second),
            vec![pair("confirmed", "patch"), pair("confirmed", "patch")],
            "{version}"
        );
        assert_eq!(ids(&second), want, "{version}");

        // A jj commit whose content Git changed without its header: paired
        // by content, inferred.
        c.git.ok(&g, &["reset", "-q", "--soft", "HEAD~1"]);
        c.git.ok(&g, &["stash", "-q"]);
        c.git.ok(&g, &["reset", "-q", "--hard", "HEAD~1"]);
        commit_file(
            &c.git,
            &g,
            "y.txt",
            &lines("y", Some(4)),
            "Add y (reworked)",
        );
        c.git.ok(&g, &["stash", "pop", "-q"]);
        c.git.ok(&g, &["commit", "-q", "-m", "Add z"]);
        c.push(&g, &stackb);
        let third = c.stack(n, 3);
        assert_eq!(
            bindings(&third),
            vec![pair("inferred", "content"), pair("confirmed", "patch")],
            "{version}"
        );
        assert_eq!(ids(&third), want, "{version}");
    }
}

#[test]
fn conf_jj_10_hostile_and_conflicting_ids() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());
    let main = c.git.ok(&w, &["rev-parse", "main"]).trim().to_owned();
    let h = "kkkkkkkkkkkkkkkkllllllllllllllll";

    // One MR's ID copied into another MR: a change of its own there, a
    // hint, and nothing changes in the first MR.
    craft(
        &c.git,
        &w,
        &main,
        &format!("change-id {h}\n"),
        "one.txt",
        "One",
    );
    c.push(&w, "HEAD:refs/heads/one");
    let one = c.open_mr("one");
    let s1 = c.stack(one, 1);
    assert_eq!(ids(&s1), vec![h.to_owned()]);
    craft(
        &c.git,
        &w,
        &main,
        &format!("change-id {h}\n"),
        "two.txt",
        "Two",
    );
    c.push(&w, "HEAD:refs/heads/two");
    let two = c.open_mr("two");
    let s2 = c.stack(two, 1);
    assert_eq!(bindings(&s2), vec![pair("new", "")]);
    assert_eq!(entries(&s2)[0]["also_in"], json!([one]));
    let again = c
        .api("GET", &format!("/merge-requests/{one}/stack"), None)
        .json();
    assert_eq!((ids(&again), bindings(&again)), (ids(&s1), bindings(&s1)));
    assert_eq!(entries(&again)[0]["also_in"], json!([two]));

    // A malformed or doubled header counts as none: the predicted ID.
    let bad = craft(
        &c.git,
        &w,
        &main,
        &format!("change-id {h}\nchange-id {h}\n"),
        "dup.txt",
        "Dup",
    );
    c.push(&w, "HEAD:refs/heads/doubled");
    let n = c.open_mr("doubled");
    assert_eq!(
        ids(&c.stack(n, 1)),
        vec![slath_core::change_identity::predict(&bad).unwrap()]
    );

    // Two commits of one revision with the same ID: unresolved, merge
    // blocked.
    let a = craft(
        &c.git,
        &w,
        &main,
        "change-id zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n",
        "a.txt",
        "A",
    );
    craft(
        &c.git,
        &w,
        &a,
        "change-id zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n",
        "b.txt",
        "B",
    );
    c.push(&w, "HEAD:refs/heads/same");
    let n = c.open_mr("same");
    let s = c.stack(n, 1);
    assert_eq!(
        bindings(&s),
        vec![
            pair("unresolved", "duplicate-id"),
            pair("unresolved", "duplicate-id")
        ]
    );
    assert_eq!(s["merge_blocked"], true);

    // A header and a trailer naming two different changes: unresolved, the
    // trailer's change first; picking one resolves it.
    let ha = "nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn";
    let tb = "I1111111111111111111111111111111111111111";
    let pa = craft(
        &c.git,
        &w,
        &main,
        &format!("change-id {ha}\n"),
        "pa.txt",
        "Part a",
    );
    craft(
        &c.git,
        &w,
        &pa,
        "",
        "pb.txt",
        &format!("Part b\n\nChange-Id: {tb}"),
    );
    c.push(&w, "HEAD:refs/heads/mixed");
    let n = c.open_mr("mixed");
    let s = c.stack(n, 1);
    let (id_a, id_b) = (ids(&s)[0].clone(), ids(&s)[1].clone());
    craft(
        &c.git,
        &w,
        &main,
        &format!("change-id {ha}\n"),
        "pab.txt",
        &format!("Both\n\nChange-Id: {tb}"),
    );
    c.push(&w, "HEAD:refs/heads/mixed");
    let s = c.stack(n, 2);
    assert_eq!(bindings(&s), vec![pair("unresolved", "aliases-conflict")]);
    assert_eq!(entries(&s)[0]["candidates"], json!([id_b, id_a]));
    assert_eq!(s["merge_blocked"], true);
    // Only candidates can be picked.
    let r = c.api(
        "POST",
        &format!("/merge-requests/{n}/stack/0/bind"),
        Some(&json!({"change": h})),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    let r = c.api(
        "POST",
        &format!("/merge-requests/{n}/stack/0/bind"),
        Some(&json!({"change": id_a})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let s = r.json();
    assert_eq!(bindings(&s), vec![pair("confirmed", "resolved")]);
    assert_eq!(ids(&s), vec![id_a.clone()]);
    assert_eq!(s["merge_blocked"], false);
    // The choice rebound the trailer: the next push continues `a`.
    craft(
        &c.git,
        &w,
        &main,
        "",
        "pab2.txt",
        &format!("Both again\n\nChange-Id: {tb}"),
    );
    c.push(&w, "HEAD:refs/heads/mixed");
    let s = c.stack(n, 3);
    assert_eq!(bindings(&s), vec![pair("confirmed", "alias")]);
    assert_eq!(ids(&s), vec![id_a]);
}

#[test]
fn revisions_reviewed_as_a_whole_have_no_changes() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());
    c.git.ok(&w, &["switch", "-q", "-c", "side", "main"]);
    commit_file(&c.git, &w, "s.txt", "s\n", "Side");
    c.git.ok(&w, &["switch", "-q", "-c", "merged", "main"]);
    commit_file(&c.git, &w, "m.txt", "m\n", "Mine");
    c.git.ok(&w, &["merge", "-q", "--no-edit", "side"]);
    c.push(&w, "merged");
    // Unrelated histories, and more commits than are reviewed one by one.
    c.git.ok(&w, &["switch", "-q", "--orphan", "unrelated"]);
    commit_file(&c.git, &w, "u.txt", "u\n", "Unrelated");
    c.push(&w, "unrelated");
    c.git.ok(&w, &["switch", "-q", "-c", "many", "main"]);
    for i in 0..251 {
        c.git.ok(
            &w,
            &["commit", "-q", "--allow-empty", "-m", &format!("Step {i}")],
        );
    }
    c.push(&w, "many");
    for (branch, reason) in [
        ("merged", "merge-commits"),
        ("unrelated", "unrelated-histories"),
        ("many", "too-many-commits"),
    ] {
        let n = c.open_mr(branch);
        let s = c.stack(n, 1);
        assert_eq!(s["entries"], json!([]), "{branch}");
        assert_eq!(s["revision"], 1, "{branch}");
        let got = c.s.db.query_one(&format!(
            "SELECT v.mode || ':' || v.mode_reason FROM mr_revisions v
               JOIN merge_requests m ON m.id = v.mr_id WHERE m.number = {n}"
        ));
        assert_eq!(got, format!("whole-only:{reason}"), "{branch}");
    }
}

/// The `remote:` lines of a push, without the prefix.
fn push_output(c: &Ctx, dir: &Path, refspec: &str) -> Vec<String> {
    let out = c
        .git
        .cmd(dir)
        .args(["push", "--force", &c.url, refspec])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{err}");
    feedback(&err)
}

fn feedback(err: &str) -> Vec<String> {
    err.lines()
        .filter_map(|l| l.strip_prefix("remote: slath: "))
        .filter(|l| l.contains("merge request") || l.starts_with("  "))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

#[test]
fn conf_jj_11_push_feedback_lists_each_change() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());
    c.git.ok(&w, &["switch", "-q", "-c", "fb", "main"]);
    commit_file(&c.git, &w, "a.txt", &lines("a", None), "Add a");
    commit_file(&c.git, &w, "b.txt", &lines("b", None), "Add b");
    commit_file(&c.git, &w, "c.txt", &lines("c", None), "Add c");
    c.push(&w, "fb");
    let n = c.open_mr("fb");
    let first = c.stack(n, 1);
    let id = |i: usize| ids(&first)[i][..8].to_owned();
    let url = format!("{}/alice/demo/merge-requests/{n}", c.s.base());

    // Git: amend the middle commit's content (a keeps its patch), drop c,
    // add d.
    c.git.ok(&w, &["reset", "-q", "--hard", "HEAD~2"]);
    commit_file(&c.git, &w, "b.txt", &lines("b", Some(1)), "Add b");
    commit_file(&c.git, &w, "d.txt", &lines("d", None), "Add d");
    let got = push_output(&c, &w, "fb");
    let s = c.stack(n, 2);
    let new_d = ids(&s)[2][..8].to_owned();
    assert_eq!(
        got,
        vec![
            format!("merge request !{n} for fb, revision 2: {url}"),
            format!("{} unchanged Add a", id(0)),
            format!("{} inferred (version 2) Add b", id(1)),
            format!("{new_d} new Add d"),
            format!("{} removed", id(2)),
        ]
    );

    // jj: the same report for the same kind of push.
    if let Some((version, bin)) = jj_binaries().into_iter().last() {
        let jj = Jj::new(&version, &bin);
        let j = d.path().join("jj");
        jj.ok(d.path(), &["git", "clone", &c.url, j.to_str().unwrap()]);
        jj.ok(&j, &["bookmark", "track", "fb@origin"]);
        jj.ok(&j, &["edit", "fb"]);
        std::fs::write(j.join("d.txt"), lines("d", Some(5))).unwrap();
        let out = jj
            .cmd(&j)
            .args(["git", "push", "--bookmark", "fb"])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "{err}");
        assert_eq!(
            feedback(&err),
            vec![
                format!("merge request !{n} for fb, revision 3: {url}"),
                format!("{} unchanged Add a", id(0)),
                format!("{} unchanged Add b", id(1)),
                format!("{new_d} updated (version 2) Add d"),
            ],
            "{version}"
        );
    }

    // A push that doesn't move the head: only the link.
    c.git.ok(&w, &["fetch", "-q", &c.url, "fb"]);
    c.git.ok(&w, &["reset", "-q", "--hard", "FETCH_HEAD"]);
    c.git.ok(&w, &["push", "-q", &c.url, "fb:refs/heads/other"]);
    let got = push_output(&c, &w, "fb:refs/heads/fb");
    assert!(
        got.is_empty() || got == vec![format!("merge request !{n} for fb: {url}")],
        "{got:?}"
    );
}

#[test]
fn push_feedback_past_its_budget_prints_only_the_link() {
    let s = TestServer::start_full(&[DEMO], "[limits]\npush_feedback_timeout = \"0ms\"\n");
    let mut alice = s.login("alice", PASSWORD);
    let token = slath_tests::http::create_token(&mut alice, "ids", &["repo:write", "mrs:write"]);
    let c = Ctx {
        url: s.url(&DEMO),
        s,
        token,
        git: GitClient::new(),
    };
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());
    c.git.ok(&w, &["switch", "-q", "-c", "slow", "main"]);
    commit_file(&c.git, &w, "a.txt", "a\n", "Add a");
    c.push(&w, "slow");
    let n = c.open_mr("slow");
    c.stack(n, 1);
    commit_file(&c.git, &w, "b.txt", "b\n", "Add b");
    let got = push_output(&c, &w, "slow");
    let url = format!("{}/alice/demo/merge-requests/{n}", c.s.base());
    assert_eq!(got, vec![format!("merge request !{n} for slow: {url}")]);
    // The job still records the revision.
    assert_eq!(entries(&c.stack(n, 2)).len(), 2);
}

#[test]
fn conf_review_03_interdiffs_are_labeled_exact_or_heuristic() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());
    c.git.ok(&w, &["switch", "-q", "-c", "inter", "main"]);
    commit_file(&c.git, &w, "a.txt", &lines("a", None), "Add a");
    c.push(&w, "inter");
    let n = c.open_mr("inter");
    let id = ids(&c.stack(n, 1))[0].clone();
    // Version 2: amended, same parent.
    std::fs::write(w.join("a.txt"), lines("a", Some(3))).unwrap();
    c.git.ok(&w, &["commit", "-qa", "--amend", "--no-edit"]);
    c.push(&w, "inter");
    c.stack(n, 2);
    // Version 3: rebased onto a newer main and edited again.
    c.git.ok(&w, &["switch", "-q", "main"]);
    commit_file(&c.git, &w, "m.txt", "m\n", "Main moves");
    c.push(&w, "main");
    c.git.ok(&w, &["switch", "-q", "inter"]);
    c.git.ok(&w, &["rebase", "-q", "main"]);
    std::fs::write(w.join("a.txt"), lines("a", Some(9))).unwrap();
    c.git.ok(&w, &["commit", "-qa", "--amend", "--no-edit"]);
    c.push(&w, "inter");
    assert_eq!(ids(&c.stack(n, 3)), vec![id.clone()]);

    let mut ui = c.s.login("alice", PASSWORD);
    let page = |ui: &mut slath_tests::http::Client, path: &str| {
        let r = ui.get(&format!("/alice/demo/merge-requests/{n}{path}"));
        assert_eq!(r.status, 200, "{path}: {}", r.body);
        r.body
    };
    let base = format!("/changes/{id}");
    let exact = page(&mut ui, &format!("{base}?from=1&rev=2"));
    assert!(exact.contains("<strong>Exact:</strong>"), "{exact}");
    assert!(exact.contains("a line 3, edited"));
    let patches = page(&mut ui, &format!("{base}?from=2&rev=3"));
    assert!(
        patches.contains("<strong>Diff of patches:</strong>"),
        "{patches}"
    );
    assert!(patches.contains("class=\"range-diff\""));
    let tree = page(&mut ui, &format!("{base}?from=2&rev=3&tree=1"));
    assert!(
        tree.contains("<strong>Tree diff, including the rebase:</strong>"),
        "{tree}"
    );
    // The tree diff includes the rebase; the patches don't.
    assert!(tree.contains("m.txt") && !patches.contains("m.txt"));
    // The short form of the ID and a commit ID lead to the change too.
    page(&mut ui, &format!("/changes/{}", &id[..8]));
    let revs = page(&mut ui, "/interdiff?from=1&to=3");
    assert!(revs.contains("<strong>Heuristic:</strong>"), "{revs}");
    assert_eq!(
        ui.get(&format!("/alice/demo/merge-requests/{n}/changes/zzzzzzzz"))
            .status,
        404
    );
}
