// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Repository maintenance: gc after pushes, size accounting, soft quota.

use std::path::Path;
use std::time::{Duration, Instant};

use slath_tests::server::{DEMO, GitClient, TestServer};

/// Commits and pushes `n` small changes, one push each.
fn push_many(git: &GitClient, src: &Path, url: &str, n: usize) {
    for i in 0..n {
        std::fs::write(src.join("counter.txt"), format!("{i}\n")).unwrap();
        git.ok(src, &["commit", "-q", "-am", &format!("change {i}")]);
        git.ok(src, &["push", "-q", url, "HEAD:refs/heads/main"]);
    }
}

fn wait_for(s: &TestServer, sql: &str, want: &str) {
    let start = Instant::now();
    loop {
        if s.db.query_one(sql) == want {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for {sql} = {want}\n{}",
            s.log()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn setup(s: &TestServer) -> (GitClient, tempfile::TempDir) {
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git.ok(&src, &["init", "-q"]);
    std::fs::write(src.join("counter.txt"), "start\n").unwrap();
    git.ok(&src, &["add", "."]);
    git.ok(&src, &["commit", "-q", "-m", "start"]);
    git.ok(&src, &["push", "-q", &s.url(&DEMO), "HEAD:refs/heads/main"]);
    (git, work)
}

#[test]
fn maintenance_runs_after_fifty_pushes_and_keeps_internal_refs() {
    let s = TestServer::start();
    let (git, work) = setup(&s);
    let src = work.path().join("src");
    let repo = s.repo_path(&DEMO);

    // A commit reachable only from refs/slath/*: pushed to a branch, the
    // branch deleted, then pinned by an internal ref.
    git.ok(&src, &["checkout", "-q", "-b", "side"]);
    std::fs::write(src.join("side.txt"), "only internal\n").unwrap();
    git.ok(&src, &["add", "."]);
    git.ok(&src, &["commit", "-q", "-m", "side"]);
    let side = git.ok(&src, &["rev-parse", "HEAD"]).trim().to_owned();
    git.ok(&src, &["push", "-q", &s.url(&DEMO), "HEAD:refs/heads/side"]);
    git.ok(&src, &["push", "-q", &s.url(&DEMO), ":refs/heads/side"]);
    git.ok(&repo, &["update-ref", "refs/slath/keep", &side]);
    git.ok(&src, &["checkout", "-q", "main"]);

    // A control object nothing references.
    let orphan = git
        .cmd(&repo)
        .args(["hash-object", "-w", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(b"unreferenced\n")?;
            c.wait_with_output()
        })
        .unwrap();
    let orphan = String::from_utf8(orphan.stdout).unwrap().trim().to_owned();

    assert_eq!(
        s.db.query_one("SELECT pushes_since_maintenance::text FROM repositories"),
        "3"
    );
    push_many(&git, &src, &s.url(&DEMO), 46);
    // Age every object past the prune grace period, so only reachability
    // decides what stays.
    let status = std::process::Command::new("find")
        .arg(repo.join("objects"))
        .args([
            "-type",
            "f",
            "-exec",
            "touch",
            "-d",
            "30 days ago",
            "{}",
            "+",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    push_many(&git, &src, &s.url(&DEMO), 1);
    wait_for(&s, "SELECT maintenance_runs::text FROM repositories", "1");
    let gone = git
        .cmd(&repo)
        .args(["cat-file", "-e", &orphan])
        .status()
        .unwrap();
    assert!(!gone.success(), "the unreferenced object was not pruned");

    // Packed, and nothing reachable was pruned.
    let counts = git.ok(&repo, &["count-objects", "-v"]);
    assert!(
        counts.contains("count: 0\n"),
        "loose objects remain:\n{counts}"
    );
    git.ok(&repo, &["cat-file", "-e", &format!("{side}^{{tree}}")]);
    git.ok(&repo, &["fsck", "--connectivity-only", "--no-dangling"]);
    assert_eq!(
        s.db.query_one("SELECT pushes_since_maintenance::text FROM repositories"),
        "0"
    );
    let size: u64 =
        s.db.query_one("SELECT size_bytes::text FROM repositories")
            .parse()
            .unwrap();
    assert!(size > 0);
    assert_eq!(
        s.db.query_one(
            "SELECT count(*)::text FROM jobs WHERE kind = 'repo.maintenance' AND state = 'done'"
        ),
        "1"
    );
}

#[test]
fn maintenance_never_overlaps_on_one_repository() {
    let s = TestServer::start();
    let (git, work) = setup(&s);
    let src = work.path().join("src");
    // Another run holds the lease.
    s.db.execute("UPDATE repositories SET maintenance_lease_until = now() + interval '1 hour'");
    push_many(&git, &src, &s.url(&DEMO), 50);
    wait_for(
        &s,
        "SELECT count(*)::text FROM jobs WHERE kind = 'repo.maintenance' AND state = 'done'",
        "1",
    );
    // The job saw the lease and did nothing.
    assert_eq!(
        s.db.query_one("SELECT maintenance_runs::text FROM repositories"),
        "0"
    );
    assert_eq!(
        s.db.query_one("SELECT (maintained_at IS NULL)::text FROM repositories"),
        "true"
    );
    // Further pushes don't queue a second job while the first one's run is
    // still the current one.
    push_many(&git, &src, &s.url(&DEMO), 3);
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM jobs WHERE kind = 'repo.maintenance'"),
        "1"
    );
}

#[test]
fn pushes_record_size_and_warn_over_the_soft_quota() {
    let s = TestServer::start_full(&[DEMO], "[limits]\nrepo_soft_quota_bytes = 1\n");
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git.ok(&src, &["init", "-q"]);
    std::fs::write(src.join("data.txt"), "x".repeat(10_000)).unwrap();
    git.ok(&src, &["add", "."]);
    git.ok(&src, &["commit", "-q", "-m", "data"]);
    let out = git
        .cmd(&src)
        .args(["push", &s.url(&DEMO), "HEAD:refs/heads/main"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("more than its soft quota of 1 bytes"),
        "{stderr}"
    );
    let size: u64 =
        s.db.query_one("SELECT size_bytes::text FROM repositories")
            .parse()
            .unwrap();
    assert!(size > 0);

    // Without a quota, no warning.
    let s = TestServer::start();
    let (git2, work2) = setup(&s);
    let src2 = work2.path().join("src");
    std::fs::write(src2.join("counter.txt"), "more\n").unwrap();
    git2.ok(&src2, &["commit", "-q", "-am", "more"]);
    let out = git2
        .cmd(&src2)
        .args(["push", &s.url(&DEMO), "HEAD:refs/heads/main"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&out.stderr).contains("quota"));
}
