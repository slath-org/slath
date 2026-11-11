// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The user guides' commands work (JJ-05, JJ-11, D-44): every named block
//! (```` ```sh git <step> ````, ```` ```sh jj <step> ````) in
//! `docs/user/merge-requests.md` and `docs/user/forks.md` runs here against
//! a test server, in the guide's order, with Git and with jj, and with no
//! client configuration beyond the remote URL (with a token) and a name.

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
    for doc in ["merge-requests.md", "forks.md"] {
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
