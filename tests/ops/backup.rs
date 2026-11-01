// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Backup and restore, including hostile backups.

use std::path::Path;

use sha2::{Digest, Sha256};
use slath_tests::fixtures;
use slath_tests::http::{create_repo, submit};
use slath_tests::server::{DEMO, GitClient, Instance, PASSWORD, TestServer};

fn refs(git: &GitClient, repo: &Path) -> String {
    git.ok(repo, &["for-each-ref", "--format=%(objectname) %(refname)"])
}

fn counts(db: &slath_tests::server::TestDb) -> String {
    db.query_one(
        "SELECT (SELECT count(*) FROM users) || ' ' || (SELECT count(*) FROM organizations) \
         || ' ' || (SELECT count(*) FROM repositories) || ' ' || (SELECT count(*) FROM ssh_keys) \
         || ' ' || (SELECT count(*) FROM repo_collaborators)",
    )
}

/// A populated instance: users, an org, repositories with history, a
/// collaborator, and a deleted repository in the trash.
fn populated() -> (TestServer, tempfile::TempDir) {
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("tags", &src, &[]);
    git.ok(
        &src,
        &[
            "push",
            &s.url(&DEMO),
            "refs/heads/*:refs/heads/*",
            "refs/tags/*:refs/tags/*",
        ],
    );
    let mut a = s.login("alice", PASSWORD);
    let r = submit(
        &mut a,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", "acme"), ("display_name", "")],
    );
    assert_eq!(r.status, 303);
    create_repo(&mut a, "acme", "tools", "public");
    let r = submit(
        &mut a,
        "/alice/demo/settings/access",
        "/alice/demo/settings/access",
        &[("username", "bob"), ("role", "write")],
    );
    assert_eq!(r.status, 303);
    create_repo(&mut a, "alice", "old", "private");
    let r = submit(
        &mut a,
        "/alice/old/settings/danger",
        "/alice/old/settings/danger",
        &[("confirm", "alice/old")],
    );
    assert_eq!(r.status, 303);
    (s, work)
}

fn backup(s: &TestServer, out: &Path) {
    let (ok, stdout, stderr) = s.slath(
        &[
            "backup",
            "create",
            "--output",
            out.to_str().unwrap(),
            "--output-format",
            "json",
        ],
        None,
    );
    assert!(ok, "backup failed: {stdout}{stderr}");
}

#[test]
fn conf_ops_02_backup_during_push_restores_to_a_clean_instance() {
    let (s, work) = populated();
    let git = GitClient::new();
    let src = work.path().join("src");
    let main_before = git.ok(&src, &["rev-parse", "refs/heads/main"]);

    // Push new branches continuously while the backup runs.
    let url = s.url(&DEMO);
    let pusher_src = src.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let pusher = std::thread::spawn(move || {
        let git = GitClient::new();
        let mut n = 0;
        while !stop2.load(std::sync::atomic::Ordering::Relaxed) && n < 200 {
            let file = pusher_src.join(format!("concurrent-{n}.txt"));
            std::fs::write(&file, format!("{n}\n").repeat(1000)).unwrap();
            git.ok(&pusher_src, &["add", "."]);
            git.ok(
                &pusher_src,
                &["commit", "-q", "-m", &format!("concurrent {n}")],
            );
            git.ok(
                &pusher_src,
                &[
                    "push",
                    "-q",
                    &url,
                    &format!("HEAD:refs/heads/concurrent-{n}"),
                ],
            );
            n += 1;
        }
        n
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    let out = tempfile::tempdir().unwrap();
    let dir = out.path().join("backup");
    backup(&s, &dir);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let pushed = pusher.join().unwrap();
    assert!(pushed > 0);

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["counts"]["users"], 2);
    assert_eq!(manifest["counts"]["repositories"], 3);
    let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    let db_password =
        s.db.url
            .split("://")
            .nth(1)
            .unwrap()
            .split('@')
            .next()
            .unwrap()
            .split(':')
            .nth(1)
            .unwrap();
    assert!(
        !config.contains(&format!(":{db_password}@")),
        "the database password leaked: {config}"
    );
    assert!(config.contains("[database]"));
    assert!(!dir.join("instance.key").exists());

    let target = Instance::new("");
    let (ok, stdout, stderr) = target.slath(
        &[
            "backup",
            "restore",
            "--input",
            dir.to_str().unwrap(),
            "--output-format",
            "json",
        ],
        None,
    );
    assert!(ok, "restore failed: {stdout}{stderr}");
    let summary: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(summary["ok"], true);
    assert_eq!(counts(&target.db), counts(&s.db));

    let t = target.start(&[]);
    // fsck is clean: database refs match Git.
    let (ok, stdout, stderr) = t.slath(&["fsck", "--output", "json"], None);
    assert!(ok, "fsck: {stdout}{stderr}");
    // Refs match the source for everything that existed before the backup,
    // and each restored concurrent branch matches what was pushed.
    let restored = refs(&git, &t.repo_path(&DEMO));
    let source = refs(&git, &s.repo_path(&DEMO));
    for line in restored.lines() {
        assert!(source.contains(line), "restored ref not in source: {line}");
    }
    assert!(restored.contains(&format!("{} refs/heads/main", main_before.trim())));
    assert!(restored.contains("refs/tags/"));
    // The restored instance works: sign in, clone, and see permissions.
    let mut a = t.login("alice", PASSWORD);
    assert_eq!(a.get("/alice/demo").status, 200);
    let mut b = t.login("bob", PASSWORD);
    assert_eq!(
        b.get("/alice/demo").status,
        200,
        "collaborator access survived"
    );
    assert_eq!(t.client().get("/alice/demo").status, 404, "still private");
    assert_eq!(t.client().get("/acme/tools").status, 200);
    let clone = work.path().join("clone");
    let mut tr = t;
    tr.token = s.token.clone();
    git.ok(
        work.path(),
        &[
            "clone",
            "-q",
            "--mirror",
            &tr.url(&DEMO),
            clone.to_str().unwrap(),
        ],
    );
    git.ok(&clone, &["fsck", "--strict", "--no-dangling"]);
    // The deleted repository is back in the trash and can be restored.
    let trash = tr.data_dir().join("trash");
    assert_eq!(std::fs::read_dir(&trash).unwrap().count(), 1);
    // Restoring repository config: Slath's own, not anything from the backup.
    let cfg = std::fs::read_to_string(tr.repo_path(&DEMO).join("config")).unwrap();
    assert!(
        cfg.contains("fsckObjects = true") || cfg.contains("fsckobjects = true"),
        "{cfg}"
    );
}

/// Writes `data` at `rel` inside the backup and lists it in the manifest
/// with a correct checksum (an attacker who controls the backup can do that).
fn plant(dir: &Path, rel: &str, data: &[u8]) {
    let path = dir.join(rel);
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let _ = std::fs::write(&path, data);
    list(dir, rel, data);
}

fn list(dir: &Path, rel: &str, data: &[u8]) {
    let mpath = dir.join("manifest.json");
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(&mpath).unwrap()).unwrap();
    let sum: String = Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    m["files"][rel] = serde_json::Value::String(sum);
    std::fs::write(&mpath, serde_json::to_vec(&m).unwrap()).unwrap();
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

/// Restoring a tampered backup fails before anything is written.
fn assert_refused(original: &Path, tamper: impl FnOnce(&Path), expect: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("b");
    copy_tree(original, &dir);
    tamper(&dir);
    let target = Instance::new("");
    let (ok, stdout, stderr) = target.slath(
        &["backup", "restore", "--input", dir.to_str().unwrap()],
        None,
    );
    assert!(!ok, "{expect}: restore succeeded: {stdout}");
    assert!(stderr.contains(expect), "{expect}: {stderr}");
    assert_eq!(
        target
            .db
            .query_one("SELECT count(*)::text FROM pg_tables WHERE schemaname = 'public'"),
        "0",
        "{expect}: the database was touched"
    );
    let repos = target.dir.path().join("data/repos");
    assert!(
        !repos.exists() || std::fs::read_dir(&repos).unwrap().next().is_none(),
        "{expect}: repositories were written"
    );
    assert!(!Path::new("/tmp/slath-pwned").exists());
}

#[test]
fn conf_sec_01_hostile_backups_are_refused() {
    let s = TestServer::start();
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("basic", &src, &[]);
    git.ok(&src, &["push", &s.url(&DEMO), "refs/heads/*:refs/heads/*"]);
    let out = tempfile::tempdir().unwrap();
    let dir = out.path().join("good");
    backup(&s, &dir);
    let id = s.db.query_one("SELECT id::text FROM repositories LIMIT 1");
    let repo = format!("repos/{id}.git");

    // Repository config and hooks never come from a backup.
    let evil_config = b"[core]\n\tfsmonitor = touch /tmp/slath-pwned\n\tsshCommand = touch /tmp/slath-pwned\n\thooksPath = /tmp\n[include]\n\tpath = /etc/passwd\n[uploadpack]\n\tpackObjectsHook = touch /tmp/slath-pwned\n";
    assert_refused(
        &dir,
        |d| plant(d, &format!("{repo}/config"), evil_config),
        "unexpected file",
    );
    assert_refused(
        &dir,
        |d| {
            plant(
                d,
                &format!("{repo}/hooks/post-receive"),
                b"#!/bin/sh\ntouch /tmp/slath-pwned\n",
            )
        },
        "unexpected file",
    );
    assert_refused(
        &dir,
        |d| plant(d, &format!("{repo}/objects/info/alternates"), b"/etc\n"),
        "unexpected file",
    );
    // Path traversal and absolute paths in the manifest.
    for bad in [
        "../../etc/cron.d/x",
        "/abs/path",
        "db/../../x.copy",
        "repos/../../x",
        "repos/x.git/refs/heads/main",
    ] {
        assert_refused(&dir, |d| list(d, bad, b"x"), "backup");
    }
    // Links and unlisted files.
    assert_refused(
        &dir,
        |d| std::os::unix::fs::symlink("/etc", d.join(format!("{repo}/refs/evil"))).unwrap(),
        "symbolic link",
    );
    assert_refused(
        &dir,
        |d| {
            std::fs::hard_link(
                d.join("config.toml"),
                d.join(format!("{repo}/refs/heads/hard")),
            )
            .unwrap();
        },
        "hard link",
    );
    assert_refused(
        &dir,
        |d| std::fs::write(d.join("extra.sh"), "x").unwrap(),
        "doesn't list",
    );
    // Tampered data.
    assert_refused(
        &dir,
        |d| {
            let p = d.join("db/users.copy");
            let mut data = std::fs::read(&p).unwrap();
            data.extend_from_slice(b"forged\n");
            std::fs::write(p, data).unwrap();
        },
        "checksum mismatch",
    );
    // A data file for a table the manifest doesn't declare.
    assert_refused(&dir, |d| plant(d, "db/evil.copy", b""), "unlisted table");
}

#[test]
fn conf_ops_02_restore_needs_an_empty_target() {
    let s = TestServer::start();
    let out = tempfile::tempdir().unwrap();
    let dir = out.path().join("b");
    backup(&s, &dir);
    // Into the running instance itself: refused, nothing changes.
    let before = counts(&s.db);
    let (ok, _, stderr) = s.slath(
        &["backup", "restore", "--input", dir.to_str().unwrap()],
        None,
    );
    assert!(!ok);
    assert!(stderr.contains("not empty"), "{stderr}");
    assert_eq!(counts(&s.db), before);
    // A second backup into the same directory is refused too.
    let (ok, _, stderr) = s.slath(
        &["backup", "create", "--output", dir.to_str().unwrap()],
        None,
    );
    assert!(!ok);
    assert!(stderr.contains("not empty"), "{stderr}");
}
