// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Jujutsu against a Slath remote, for every pinned jj version.
//!
//! jj binaries come from `$SLATH_TEST_JJ` (colon-separated paths) or from
//! `scripts/install-jj.sh`'s default location. Set `SLATH_SKIP_JJ_TESTS=1`
//! to skip. Besides the clone/push round trips, these tests record how jj
//! writes its change identity into Git commits, which Slath relies on.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use slath_git::commit::RawCommit;
use slath_tests::fixtures;
use slath_tests::server::{DEMO, GitClient, TestServer};
use slath_tests::sshd::{self, Sshd};

/// All jj binaries to test, with their versions.
fn jj_binaries() -> Vec<(String, PathBuf)> {
    if std::env::var_os("SLATH_SKIP_JJ_TESTS").is_some_and(|v| v == "1") {
        eprintln!("SLATH_SKIP_JJ_TESTS=1: skipping");
        return Vec::new();
    }
    let paths: Vec<PathBuf> = if let Some(list) = std::env::var_os("SLATH_TEST_JJ") {
        std::env::split_paths(&list).collect()
    } else {
        let dir = std::env::var_os("SLATH_TOOLS_DIR").map_or_else(
            || PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cache/slath/tools"),
            PathBuf::from,
        );
        let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|d| {
                d.flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with("jj-"))
                    .map(|e| e.path().join("bin/jj"))
                    .filter(|p| p.is_file())
                    .collect()
            })
            .unwrap_or_default();
        found.sort();
        found
    };
    assert!(
        !paths.is_empty(),
        "no jj binaries found; run scripts/install-jj.sh <version> for each version in \
         tests/compat/versions.toml, set SLATH_TEST_JJ, or set SLATH_SKIP_JJ_TESTS=1"
    );
    paths
        .into_iter()
        .map(|p| {
            let out = Command::new(&p).arg("--version").output().unwrap();
            (String::from_utf8_lossy(&out.stdout).trim().to_owned(), p)
        })
        .collect()
}

/// One jj "machine": its own HOME and config.
struct Jj {
    bin: PathBuf,
    version: String,
    home: tempfile::TempDir,
    ssh_command: Option<String>,
}

impl Jj {
    fn new(version: &str, bin: &Path) -> Self {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("jj.toml"),
            "[user]\nname = \"Jay Jay\"\nemail = \"jj@example.org\"\n[ui]\npaginate = \"never\"\n",
        )
        .unwrap();
        Self {
            bin: bin.to_owned(),
            version: version.to_owned(),
            home,
            ssh_command: None,
        }
    }

    fn cmd(&self, cwd: &Path) -> Command {
        let mut c = Command::new(&self.bin);
        c.current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.home.path())
            .env("JJ_CONFIG", self.home.path().join("jj.toml"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0");
        if let Some(ssh) = &self.ssh_command {
            c.env("GIT_SSH_COMMAND", ssh);
        }
        c
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd(cwd).args(args).output().unwrap()
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.run(cwd, args);
        assert!(
            out.status.success(),
            "{} {args:?} failed:\n{}",
            self.version,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// `jj log` of one revision with a template.
    fn show(&self, repo: &Path, rev: &str, template: &str) -> String {
        self.ok(repo, &["log", "--no-graph", "-r", rev, "-T", template])
            .trim()
            .to_owned()
    }
}

/// The Git object store of a jj repository: `.git` when colocated (the
/// default for `jj git init` in recent versions), else the internal store.
fn git_store(repo: &Path) -> PathBuf {
    let colocated = repo.join(".git");
    if colocated.is_dir() {
        colocated
    } else {
        repo.join(".jj/repo/store/git")
    }
}

/// The raw commit object `rev` resolves to in `git_dir`.
fn raw_commit(git: &GitClient, git_dir: &Path, rev: &str) -> RawCommit {
    let out = git
        .cmd(git_dir)
        .args(["cat-file", "commit", rev])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cat-file {rev}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    RawCommit::parse(out.stdout).unwrap()
}

/// Clone with jj, commit, push; then a second jj machine and a plain Git
/// clone both see the change, with the same change ID.
fn round_trip(server: &TestServer, url: &str, ssh: Option<String>) {
    let git = GitClient::new();
    for (version, bin) in jj_binaries() {
        let work = tempfile::tempdir().unwrap();
        let mut alice = Jj::new(&version, &bin);
        alice.ssh_command.clone_from(&ssh);
        alice.ok(work.path(), &["git", "clone", url, "a"]);
        let a = work.path().join("a");
        let msg = format!("Change from {version}");
        std::fs::write(a.join(format!("{}.txt", version.replace(' ', "_"))), &msg).unwrap();
        alice.ok(&a, &["describe", "-m", &msg]);
        alice.ok(&a, &["bookmark", "set", "main", "-r", "@"]);
        alice.ok(&a, &["git", "push", "--bookmark", "main"]);
        let change_id = alice.show(&a, "main", "change_id");
        let commit_id = alice.show(&a, "main", "commit_id");

        // The server has the commit, with jj's change-id header.
        let server_repo = server.repo_path(&DEMO);
        let c = raw_commit(&git, &server_repo, "refs/heads/main");
        assert_eq!(
            c.change_id(),
            Some(change_id.as_str()),
            "{version}: change-id header"
        );
        git.ok(&server_repo, &["fsck", "--strict", "--no-dangling"]);
        // Only the bookmark was pushed; no refs/jj/*.
        let refs = git.ok(&server_repo, &["for-each-ref", "--format=%(refname)"]);
        assert!(
            refs.lines().all(|r| r.starts_with("refs/heads/")),
            "{version}: {refs}"
        );

        // Second machine, jj: same commit and change ID.
        let mut bob = Jj::new(&version, &bin);
        bob.ssh_command.clone_from(&ssh);
        bob.ok(work.path(), &["git", "clone", url, "b"]);
        let b = work.path().join("b");
        assert_eq!(bob.show(&b, "main", "commit_id"), commit_id, "{version}");
        assert_eq!(bob.show(&b, "main", "change_id"), change_id, "{version}");
        assert_eq!(bob.show(&b, "main", "description"), msg, "{version}");

        // Second machine, plain Git.
        let mut clone = git.cmd(work.path());
        if let Some(ssh) = &ssh {
            clone.env("GIT_SSH_COMMAND", ssh);
        }
        let out = clone.args(["clone", "--quiet", url, "g"]).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let g = work.path().join("g");
        assert_eq!(
            git.ok(&g, &["rev-parse", "HEAD"]).trim(),
            commit_id,
            "{version}"
        );
        assert_eq!(
            raw_commit(&git, &g, "HEAD").change_id(),
            Some(change_id.as_str())
        );
    }
}

fn seeded_server(extra: &str) -> TestServer {
    let server = TestServer::start_full(&[DEMO], extra);
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("basic", &src, &[]);
    git.ok(&src, &["push", "--quiet", &server.url(&DEMO), "main"]);
    server
}

#[test]
fn conf_jj_01_clone_and_push_over_https() {
    if jj_binaries().is_empty() {
        return;
    }
    let server = seeded_server("");
    round_trip(&server, &server.url(&DEMO), None);
}

#[test]
fn conf_jj_01_clone_and_push_over_ssh() {
    if jj_binaries().is_empty() || sshd::skipped() {
        return;
    }
    let keys = tempfile::tempdir().unwrap();
    let (key, public) = sshd::keygen(keys.path(), "jj");
    let extra = format!(
        "[[spike.ssh_keys]]\nid = \"0192f1a2-3b4c-7d5e-8f60-0000000000bb\"\npublic_key = \"{public}\"\nrepos = [\"alice/demo\"]\n"
    );
    let server = seeded_server(&extra);
    let sshd = Sshd::start(&server);
    round_trip(
        &server,
        &sshd.url("alice", "demo"),
        Some(sshd.git_ssh_command(&key)),
    );
}

/// How jj and Git treat the `change-id` header: present and `[k-z]{32}` by
/// default, absent with `git.write-change-id-header=false`, kept by
/// `git commit --amend`, dropped by `git rebase`.
#[test]
fn jj_change_id_header_behavior() {
    let git = GitClient::new();
    for (version, bin) in jj_binaries() {
        let work = tempfile::tempdir().unwrap();
        let jj = Jj::new(&version, &bin);
        jj.ok(work.path(), &["git", "init", "r"]);
        let r = work.path().join("r");
        let store = git_store(&r);
        std::fs::write(r.join("a"), "a").unwrap();
        jj.ok(&r, &["describe", "-m", "with header"]);
        let with = jj.show(&r, "@", "commit_id");
        let c = raw_commit(&git, &store, &with);
        let id = c
            .change_id()
            .unwrap_or_else(|| panic!("{version}: no valid change-id header"));
        assert_eq!(id, jj.show(&r, "@", "change_id"), "{version}");

        jj.ok(
            &r,
            &[
                "new",
                "-m",
                "without header",
                "--config",
                "git.write-change-id-header=false",
            ],
        );
        let without = jj.show(&r, "@", "commit_id");
        let c = raw_commit(&git, &store, &without);
        assert!(
            c.header(b"change-id").is_none(),
            "{version}: header written despite config"
        );

        // Plain Git rewriting a jj commit.
        let g = work.path().join("g");
        git.ok(work.path(), &["init", "--quiet", "g"]);
        git.ok(
            &g,
            &[
                "fetch",
                "--quiet",
                store.to_str().unwrap(),
                &format!("{with}:refs/heads/jj"),
            ],
        );
        git.ok(&g, &["checkout", "--quiet", "jj"]);
        git.ok(
            &g,
            &["commit", "--quiet", "--amend", "-m", "amended by git"],
        );
        assert_eq!(
            raw_commit(&git, &g, "HEAD").change_id(),
            Some(id),
            "{version}: amend"
        );
        git.ok(&g, &["commit", "--quiet", "--allow-empty", "-m", "child"]);
        git.ok(&g, &["rebase", "--quiet", "--force-rebase", "--root"]);
        assert_eq!(
            raw_commit(&git, &g, "HEAD~1").change_id(),
            None,
            "{version}: rebase"
        );
    }
}

/// jj conflict commits carry a `jj:trees` header and `.jjconflict-*` tree
/// entries, and `jj git push` refuses them.
#[test]
fn jj_conflicted_commits_are_marked_and_not_pushed() {
    let git = GitClient::new();
    let server = TestServer::start();
    for (version, bin) in jj_binaries() {
        let work = tempfile::tempdir().unwrap();
        let jj = Jj::new(&version, &bin);
        jj.ok(work.path(), &["git", "init", "r"]);
        let r = work.path().join("r");
        jj.ok(&r, &["describe", "-m", "x"]);
        std::fs::write(r.join("f"), "x\n").unwrap();
        let x = jj.show(&r, "@", "change_id");
        jj.ok(&r, &["new", "root()", "-m", "y"]);
        std::fs::write(r.join("f"), "y\n").unwrap();
        let y = jj.show(&r, "@", "change_id");
        jj.ok(&r, &["new", &x, &y, "-m", "conflicted merge"]);
        let commit = jj.show(&r, "@", "commit_id");
        let store = git_store(&r);
        assert!(
            raw_commit(&git, &store, &commit).has_jj_trees_header(),
            "{version}"
        );
        let tree = git.ok(&store, &["ls-tree", "--name-only", &commit]);
        assert!(
            tree.lines().any(|l| l.starts_with(".jjconflict-")),
            "{version}: {tree}"
        );

        jj.ok(&r, &["git", "remote", "add", "origin", &server.url(&DEMO)]);
        jj.ok(&r, &["bookmark", "create", "conflicted", "-r", "@"]);
        let out = jj.run(&r, &["git", "push", "--bookmark", "conflicted"]);
        assert!(
            !out.status.success(),
            "{version}: conflicted commit was pushed"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("conflict"),
            "{version}"
        );
    }
}
