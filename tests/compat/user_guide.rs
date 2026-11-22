// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The user guides' commands work (JJ-05, JJ-11, D-44): every named block
//! (```` ```sh git <step> ````, ```` ```sh jj <step> ````) in
//! `docs/user/merge-requests.md`, `docs/user/stacks.md`, and
//! `docs/user/forks.md` runs here against a test server, in the guide's
//! order, with Git, with jj, and with both on one stack, and with no client
//! configuration beyond the remote URL (with a token) and a name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer, git_bin};

use crate::fork_merge_requests::{api, eventually, fork, token};
use crate::jj::jj_binaries;

/// The named blocks of the guides: (tool, step) → commands.
fn blocks() -> HashMap<(String, String), String> {
    let mut out = HashMap::new();
    for doc in ["merge-requests.md", "stacks.md", "forks.md"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/user")
            .join(doc);
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines = text.lines();
        while let Some(l) = lines.next() {
            let Some(info) = l.strip_prefix("```sh ") else {
                continue;
            };
            let mut parts = info.split_whitespace();
            let (Some(tool), Some(step)) = (parts.next(), parts.next()) else {
                continue;
            };
            let body: Vec<&str> = lines.by_ref().take_while(|l| *l != "```").collect();
            let key = (tool.to_owned(), step.to_owned());
            assert!(!out.contains_key(&key), "{doc}: {tool} {step} twice");
            out.insert(key, body.join("\n"));
        }
    }
    out
}

/// Runs blocks of one tool like a user would, in a shell.
struct Shell {
    blocks: HashMap<(String, String), String>,
    tool: &'static str,
    bin: tempfile::TempDir,
    home: tempfile::TempDir,
    base: String,
}

impl Shell {
    fn new(tool: &'static str, s: &TestServer, jj: Option<&Path>) -> Self {
        let bin = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(git_bin(), bin.path().join("git")).unwrap();
        if let Some(jj) = jj {
            std::os::unix::fs::symlink(jj, bin.path().join("jj")).unwrap();
        }
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("jj.toml"),
            "[user]\nname = \"Jay Jay\"\nemail = \"jj@example.org\"\n[ui]\npaginate = \"never\"\n",
        )
        .unwrap();
        Self {
            blocks: blocks(),
            tool,
            bin,
            home,
            base: s.base().trim_start_matches("http://").to_owned(),
        }
    }

    /// Runs step `step` in `dir` as the owner of `token`, with merge
    /// request `n` for the guide's example number 12. Returns its output.
    fn run(&self, step: &str, dir: &Path, token: &str, n: i64) -> String {
        let block = self
            .blocks
            .get(&(self.tool.to_owned(), step.to_owned()))
            .unwrap_or_else(|| panic!("the guides have no `sh {} {step}` block", self.tool));
        let script = block
            .replace(
                "https://slath.example/",
                &format!("http://git:{token}@{}/", self.base),
            )
            .replace("merge-requests/12/", &format!("merge-requests/{n}/"))
            .replace("mr/12", &format!("mr/{n}"));
        let path = format!(
            "{}:{}",
            self.bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new("bash")
            .args(["-e", "-c", &script])
            .current_dir(dir)
            .env_clear()
            .env("PATH", path)
            .env("HOME", self.home.path())
            .env("JJ_CONFIG", self.home.path().join("jj.toml"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", "Jay Jay")
            .env("GIT_AUTHOR_EMAIL", "jj@example.org")
            .env("GIT_COMMITTER_NAME", "Jay Jay")
            .env("GIT_COMMITTER_EMAIL", "jj@example.org")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "{} {step} failed:\n{script}\n{text}",
            self.tool
        );
        text
    }

    /// The commit `rev` names in the clone at `dir`.
    fn rev(&self, dir: &Path, rev: &str) -> String {
        let (cmd, args): (&str, Vec<String>) = if self.tool == "git" {
            ("git", vec!["rev-parse".into(), rev.into()])
        } else {
            (
                "jj",
                vec![
                    "log".into(),
                    "--no-graph".into(),
                    "-r".into(),
                    rev.into(),
                    "-T".into(),
                    "commit_id".into(),
                ],
            )
        };
        let out = Command::new(self.bin.path().join(cmd))
            .args(&args)
            .current_dir(dir)
            .env("HOME", self.home.path())
            .env("JJ_CONFIG", self.home.path().join("jj.toml"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }
}

fn mr(s: &TestServer, t: &str, n: i64) -> serde_json::Value {
    api(
        s,
        t,
        "GET",
        &format!("/alice/demo/merge-requests/{n}"),
        None,
    )
    .json()
}

/// The tools to run the guides with: Git, and the newest jj there is.
fn tools() -> Vec<(&'static str, Option<PathBuf>)> {
    let mut t = vec![("git", None)];
    if let Some((_, bin)) = jj_binaries().into_iter().last() {
        t.push(("jj", Some(bin)));
    }
    t
}

fn seed(s: &TestServer) {
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "demo\n").unwrap();
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "init"]);
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "main"]);
}

/// Moves `main` on the server with another commit (adding `file`).
fn advance_main(s: &TestServer, file: &str) {
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    git.ok(d.path(), &["clone", "-q", &s.url(&DEMO), "w"]);
    std::fs::write(w.join(file), "elsewhere\n").unwrap();
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "Elsewhere"]);
    git.ok(&w, &["push", "-q", "origin", "main"]);
}

#[test]
fn conf_jj_11_the_merge_request_guide_runs_with_git_and_jj() {
    for (tool, jj) in tools() {
        let s = TestServer::start();
        seed(&s);
        let alice = token(&s, "alice");
        let sh = Shell::new(tool, &s, jj.as_deref());
        let root = tempfile::tempdir().unwrap();
        let demo = root.path().join("demo");
        sh.run("clone", root.path(), &alice, 0);

        // Push a branch: the output links to the compare page.
        std::fs::write(demo.join("notes.md"), "notes\n").unwrap();
        let out = sh.run("push-branch", &demo, &alice, 0);
        assert!(
            out.contains("Create a merge request for my-topic"),
            "{tool}: {out}"
        );
        let r = api(
            &s,
            &alice,
            "POST",
            "/alice/demo/merge-requests",
            Some(json!({"source_branch": "my-topic", "title": "Add notes"})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        eventually(&s, "no first revision", || {
            mr(&s, &alice, n)["revisions"] == 1
        });

        // Update: a new revision of the same change.
        std::fs::write(demo.join("notes.md"), "notes, reviewed\n").unwrap();
        sh.run("update", &demo, &alice, n);
        eventually(&s, "no second revision", || {
            mr(&s, &alice, n)["revisions"] == 2
        });
        let changes = |s: &TestServer| {
            api(
                s,
                &alice,
                "GET",
                &format!("/alice/demo/merge-requests/{n}/stack"),
                None,
            )
            .json()["entries"]
                .as_array()
                .unwrap()
                .len()
        };
        assert_eq!(changes(&s), 1, "{tool}: still one change");

        // Rebase onto a moved target.
        advance_main(&s, "one.md");
        sh.run("rebase", &demo, &alice, n);
        eventually(&s, "no rebased revision", || {
            mr(&s, &alice, n)["revisions"] == 3
        });
        let mb = api(
            &s,
            &alice,
            "GET",
            &format!("/alice/demo/merge-requests/{n}/mergeability"),
            None,
        )
        .json();
        assert_eq!(mb["up_to_date"], true, "{tool}: {mb}");

        // A change on the server, then catching up.
        advance_main(&s, "two.md");
        let r = api(
            &s,
            &alice,
            "POST",
            &format!("/alice/demo/merge-requests/{n}/update-branch"),
            Some(json!({"method": "merge"})),
        );
        assert_eq!(r.status, 200, "{}", r.body);
        let server_head = r.json()["head_oid"].as_str().unwrap().to_owned();
        sh.run("catch-up", &demo, &alice, n);
        let local = if tool == "git" { "HEAD" } else { "my-topic" };
        assert_eq!(sh.rev(&demo, local), server_head, "{tool}: caught up");

        // Checking out the merge request.
        sh.run("checkout", &demo, &alice, n);
        let checked = if tool == "git" { "HEAD" } else { "@-" };
        assert_eq!(sh.rev(&demo, checked), server_head, "{tool}: checked out");

        // Merged, then on with the target.
        let r = api(
            &s,
            &alice,
            "POST",
            &format!("/alice/demo/merge-requests/{n}/merge"),
            Some(json!({"strategy": "merge-commit", "expected_head_oid": server_head})),
        );
        assert_eq!(r.status, 200, "{}", r.body);
        sh.run("after-merge", &demo, &alice, n);
        let main = GitClient::new().ok(&s.repo_path(&DEMO), &["rev-parse", "main"]);
        let at = if tool == "git" { "HEAD" } else { "@-" };
        assert_eq!(
            sh.rev(&demo, at),
            main.trim(),
            "{tool}: on the merged target"
        );
    }
}

#[test]
fn conf_jj_13_the_guides_pushes_without_a_branch_run_with_git_and_jj() {
    for (tool, jj) in tools() {
        let s = TestServer::start();
        seed(&s);
        let alice = token(&s, "alice");
        let sh = Shell::new(tool, &s, jj.as_deref());
        let root = tempfile::tempdir().unwrap();
        let demo = root.path().join("demo");
        sh.run("clone", root.path(), &alice, 0);
        std::fs::write(demo.join("notes.md"), "notes\n").unwrap();
        let out = sh.run("push-change", &demo, &alice, 0);
        assert!(out.contains("slath: Opened !1 for "), "{tool}: {out}");
        eventually(&s, "no first revision", || {
            mr(&s, &alice, 1)["revisions"] == 1
        });
        assert_eq!(mr(&s, &alice, 1)["title"], "Add notes", "{tool}");
        std::fs::write(demo.join("notes.md"), "notes, reviewed\n").unwrap();
        sh.run("update-change", &demo, &alice, 1);
        eventually(&s, "no second revision", || {
            mr(&s, &alice, 1)["revisions"] == 2
        });
        let r = api(
            &s,
            &alice,
            "GET",
            "/alice/demo/merge-requests?state=all",
            None,
        );
        assert_eq!(r.json()["items"].as_array().unwrap().len(), 1, "{tool}");
    }
}

#[test]
fn conf_jj_11_the_fork_guide_runs_with_git_and_jj() {
    for (tool, jj) in tools() {
        let s = TestServer::start();
        s.create_user("bob", PASSWORD, false);
        seed(&s);
        let alice = token(&s, "alice");
        let bob = token(&s, "bob");
        let r = api(
            &s,
            &alice,
            "PATCH",
            "/alice/demo",
            Some(json!({"visibility": "public"})),
        );
        assert!(r.status < 300, "{}", r.body);
        fork(&s, &bob, json!({"namespace": "bob"}), "/bob/demo/fork");
        let sh = Shell::new(tool, &s, jj.as_deref());
        let root = tempfile::tempdir().unwrap();

        // Bob pushes a branch to his fork and opens a merge request.
        let bob_demo = root.path().join("bob-demo");
        sh.run("fork-clone", root.path(), &bob, 0);
        std::fs::write(bob_demo.join("notes.md"), "notes\n").unwrap();
        sh.run("fork-push", &bob_demo, &bob, 0);
        let r = api(
            &s,
            &bob,
            "POST",
            "/alice/demo/merge-requests",
            Some(
                json!({"source_repo": "bob/demo", "source_branch": "my-topic", "title": "Add notes"}),
            ),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        eventually(&s, "no first revision", || {
            mr(&s, &bob, n)["revisions"] == 1
        });

        // The original moves on; Bob rebases his branch onto it, which is
        // the next revision on the new base, with the same change.
        let change = |s: &TestServer| {
            api(
                s,
                &bob,
                "GET",
                &format!("/alice/demo/merge-requests/{n}/stack"),
                None,
            )
            .json()["entries"][0]["change_id"]
                .clone()
        };
        let before = change(&s);
        advance_main(&s, "elsewhere.txt");
        sh.run("fork-rebase", &bob_demo, &bob, 0);
        eventually(&s, "no revision from the rebase", || {
            mr(&s, &bob, n)["revisions"] == 2
        });
        let main = GitClient::new().ok(&s.repo_path(&DEMO), &["rev-parse", "main"]);
        let revs = api(
            &s,
            &bob,
            "GET",
            &format!("/alice/demo/merge-requests/{n}/revisions"),
            None,
        )
        .json();
        assert_eq!(revs[1]["base_oid"], main.trim(), "{tool}: {revs}");
        assert_eq!(change(&s), before, "{tool}");

        // Alice checks it out as mr/<n> in her clone, edits it, and the
        // fork's branch follows.
        let r = api(
            &s,
            &alice,
            "POST",
            &format!("/alice/demo/merge-requests/{n}/checkout"),
            None,
        );
        assert_eq!(r.status, 200, "{}", r.body);
        let demo = root.path().join("demo");
        sh.run("clone", root.path(), &alice, 0);
        sh.run("mr-checkout", &demo, &alice, n);
        let head = mr(&s, &alice, n)["head_oid"].as_str().unwrap().to_owned();
        let checked = if tool == "git" { "HEAD" } else { "@-" };
        assert_eq!(
            sh.rev(&demo, checked),
            head,
            "{tool}: mr branch checked out"
        );
        std::fs::write(demo.join("notes.md"), "notes, fixed\n").unwrap();
        let out = sh.run("mr-edit", &demo, &alice, n);
        assert!(
            out.contains(&format!("mr/{n}: updated bob/demo:my-topic")),
            "{tool}: {out}"
        );
        let fork_head = GitClient::new().ok(
            &s.repo_path(&slath_tests::server::TestRepo {
                namespace: "bob",
                name: "demo",
            }),
            &["rev-parse", "my-topic"],
        );
        eventually(&s, "no revision from the edit", || {
            mr(&s, &alice, n)["head_oid"] == fork_head.trim()
        });
        assert_ne!(fork_head.trim(), head, "{tool}: the fork moved");
    }
}

/// The stack of merge request `n`: (change ID, outcome, change state) per
/// change, bottom first.
fn stack(s: &TestServer, t: &str, n: i64) -> Vec<(String, String, String)> {
    let st = api(
        s,
        t,
        "GET",
        &format!("/alice/demo/merge-requests/{n}/stack"),
        None,
    )
    .json();
    st["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["change_id"].as_str().unwrap_or_default().to_owned(),
                e["outcome"].as_str().unwrap_or_default().to_owned(),
                e["change_state"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// A file of a few lines, as a step of real work has them.
fn parser(fix: &str) -> String {
    format!(
        "fn parse(input) {{\n  let tokens = split(input)\n  check(tokens)\n  // {fix}\n  tree(tokens)\n}}\n"
    )
}

/// How the bottom change of merge request `n` was matched.
fn binding(s: &TestServer, t: &str, n: i64) -> String {
    let st = api(
        s,
        t,
        "GET",
        &format!("/alice/demo/merge-requests/{n}/stack"),
        None,
    )
    .json();
    st["entries"][0]["binding"].as_str().unwrap().to_owned()
}

fn ids(stack: &[(String, String, String)]) -> Vec<String> {
    stack.iter().map(|e| e.0.clone()).collect()
}

/// Opens the merge request for `my-stack` and waits for its first revision.
fn open_stack(s: &TestServer, t: &str) -> i64 {
    let r = api(
        s,
        t,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_branch": "my-stack", "title": "Parser"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    eventually(s, "no first revision", || mr(s, t, n)["revisions"] == 1);
    n
}

fn head(s: &TestServer, t: &str, n: i64) -> String {
    mr(s, t, n)["head_oid"].as_str().unwrap().to_owned()
}

/// jj shows no change twice after following the server's rewrites.
fn no_divergence(sh: &Shell, dir: &Path) {
    if sh.tool == "jj" {
        let out = Command::new(sh.bin.path().join("jj"))
            .args(["log", "--no-graph", "-r", "divergent()", "-T", "change_id"])
            .current_dir(dir)
            .env("HOME", sh.home.path())
            .env("JJ_CONFIG", sh.home.path().join("jj.toml"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "",
            "divergent changes"
        );
    }
}

#[test]
fn conf_jj_11_the_stack_guide_runs_with_git_and_jj() {
    for (tool, jj) in tools() {
        let s = TestServer::start();
        seed(&s);
        let alice = token(&s, "alice");
        let sh = Shell::new(tool, &s, jj.as_deref());
        let root = tempfile::tempdir().unwrap();
        let demo = root.path().join("demo");
        sh.run("clone", root.path(), &alice, 0);

        // Two commits, two changes.
        std::fs::write(demo.join("parser.txt"), parser("first")).unwrap();
        std::fs::write(demo.join("main.txt"), "main\n").unwrap();
        sh.run("stack-push", &demo, &alice, 0);
        let n = open_stack(&s, &alice);
        let first = ids(&stack(&s, &alice, n));
        assert_eq!(first.len(), 2, "{tool}: {first:?}");

        // Fixing the bottom: a new version of it, the top only rebased.
        std::fs::write(demo.join("parser.txt"), parser("fixed")).unwrap();
        sh.run("stack-fix", &demo, &alice, n);
        eventually(&s, "no second revision", || {
            mr(&s, &alice, n)["revisions"] == 2
        });
        let st = stack(&s, &alice, n);
        assert_eq!(ids(&st), first, "{tool}: same changes");
        assert_eq!(st[0].1, "updated", "{tool}: {st:?}");
        assert_eq!(st[1].1, "unchanged", "{tool}: {st:?}");
        // jj's ID confirms it; a Git commit without one is matched by its
        // similar content.
        let bound = if tool == "git" {
            "inferred"
        } else {
            "confirmed"
        };
        assert_eq!(binding(&s, &alice, n), bound, "{tool}");

        // A third change on top.
        std::fs::write(demo.join("docs.txt"), "docs\n").unwrap();
        sh.run("stack-grow", &demo, &alice, n);
        eventually(&s, "no third revision", || {
            mr(&s, &alice, n)["revisions"] == 3
        });
        let st = stack(&s, &alice, n);
        assert_eq!(st.len(), 3, "{tool}: {st:?}");
        assert_eq!(ids(&st)[..2], first[..], "{tool}");
        assert_eq!(st[2].1, "new", "{tool}: {st:?}");
        let all = ids(&st);

        // The bottom lands in Slath; the rest is rebased onto it.
        let r = api(
            &s,
            &alice,
            "POST",
            &format!("/alice/demo/merge-requests/{n}/merge"),
            Some(
                json!({"strategy": "rebase", "expected_head_oid": head(&s, &alice, n), "upto_change": &all[0][..8]}),
            ),
        );
        assert_eq!(r.status, 200, "{}", r.body);
        eventually(&s, "rest not rebased", || stack(&s, &alice, n).len() == 2);
        sh.run("stack-catch-up", &demo, &alice, n);
        assert_eq!(
            sh.rev(&demo, "my-stack"),
            head(&s, &alice, n),
            "{tool}: caught up"
        );
        no_divergence(&sh, &demo);

        // The next change lands by a push of the target.
        sh.run("stack-land-next", &demo, &alice, n);
        eventually(&s, "second change not landed", || {
            stack(&s, &alice, n)
                .iter()
                .any(|e| e.0 == all[1] && e.2 == "merged")
        });
        assert_eq!(mr(&s, &alice, n)["state"], "open", "{tool}");

        // The rest by another push: merged.
        sh.run("stack-land-all", &demo, &alice, n);
        eventually(&s, "not merged", || mr(&s, &alice, n)["state"] == "merged");
        let main = GitClient::new().ok(&s.repo_path(&DEMO), &["rev-parse", "main"]);
        assert_eq!(main.trim(), sh.rev(&demo, "my-stack"), "{tool}");

        // Done: the branch is gone, and work goes on from the target.
        sh.run("stack-done", &demo, &alice, n);
        let branches = GitClient::new().ok(&s.repo_path(&DEMO), &["branch", "--list", "my-stack"]);
        assert_eq!(branches.trim(), "", "{tool}: branch deleted");
        let at = if tool == "git" { "HEAD" } else { "@-" };
        assert_eq!(sh.rev(&demo, at), main.trim(), "{tool}: on the target");
    }
}

#[test]
fn conf_jj_11_the_stack_guide_mixes_git_and_jj() {
    let Some((_, jj)) = jj_binaries().into_iter().last() else {
        return;
    };
    let s = TestServer::start();
    seed(&s);
    let alice = token(&s, "alice");
    let git = Shell::new("git", &s, None);
    let jj = Shell::new("jj", &s, Some(&jj));
    let (gr, jr) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (gd, jd) = (gr.path().join("demo"), jr.path().join("demo"));

    // Git pushes the stack.
    git.run("clone", gr.path(), &alice, 0);
    std::fs::write(gd.join("parser.txt"), parser("first")).unwrap();
    std::fs::write(gd.join("main.txt"), "main\n").unwrap();
    git.run("stack-push", &gd, &alice, 0);
    let n = open_stack(&s, &alice);
    let first = ids(&stack(&s, &alice, n));
    assert_eq!(first.len(), 2, "{first:?}");

    // jj takes it over and fixes the bottom.
    jj.run("clone", jr.path(), &alice, 0);
    jj.run("stack-take-over", &jd, &alice, n);
    std::fs::write(jd.join("parser.txt"), parser("fixed by jj")).unwrap();
    jj.run("stack-fix", &jd, &alice, n);
    eventually(&s, "no second revision", || {
        mr(&s, &alice, n)["revisions"] == 2
    });
    let st = stack(&s, &alice, n);
    assert_eq!(ids(&st), first, "jj kept the changes: {st:?}");
    assert_eq!((&*st[0].1, &*st[1].1), ("updated", "unchanged"), "{st:?}");

    // Git catches up and fixes the bottom again.
    git.run("stack-catch-up", &gd, &alice, n);
    assert_eq!(git.rev(&gd, "HEAD"), head(&s, &alice, n), "git caught up");
    std::fs::write(gd.join("parser.txt"), parser("fixed by git")).unwrap();
    git.run("stack-fix", &gd, &alice, n);
    eventually(&s, "no third revision", || {
        mr(&s, &alice, n)["revisions"] == 3
    });
    let st = stack(&s, &alice, n);
    assert_eq!(ids(&st), first, "git kept the changes: {st:?}");
    // Git kept the ID jj wrote into the commit.
    assert_eq!(binding(&s, &alice, n), "confirmed");
    assert_eq!((&*st[0].1, &*st[1].1), ("updated", "unchanged"), "{st:?}");

    // jj catches up and adds a change; Git lands the stack.
    jj.run("stack-catch-up", &jd, &alice, n);
    assert_eq!(jj.rev(&jd, "my-stack"), head(&s, &alice, n), "jj caught up");
    no_divergence(&jj, &jd);
    std::fs::write(jd.join("docs.txt"), "docs\n").unwrap();
    jj.run("stack-grow", &jd, &alice, n);
    eventually(&s, "no fourth revision", || {
        mr(&s, &alice, n)["revisions"] == 4
    });
    let st = stack(&s, &alice, n);
    assert_eq!(ids(&st)[..2], first[..], "{st:?}");
    assert_eq!(st.len(), 3, "{st:?}");
    git.run("stack-catch-up", &gd, &alice, n);
    git.run("stack-land-all", &gd, &alice, n);
    eventually(&s, "not merged", || mr(&s, &alice, n)["state"] == "merged");
}
