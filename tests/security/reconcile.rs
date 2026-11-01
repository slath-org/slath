// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reconciler and `slath fsck`.

use slath_tests::server::{DEMO, GitClient, TestServer};

fn push_main(s: &TestServer, git: &GitClient, dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    git.ok(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git.ok(dir, &["add", "a.txt"]);
    git.ok(dir, &["commit", "-q", "-m", "first"]);
    git.ok(
        dir,
        &[
            "push",
            "-q",
            &s.url(&DEMO),
            "main",
            "HEAD:refs/heads/doomed",
        ],
    );
}

#[test]
fn conf_ops_06_refs_changed_behind_slaths_back_are_repaired_by_fsck() {
    let s = TestServer::start();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    push_main(&s, &git, &w);
    let bare = s.repo_path(&DEMO);
    let head = git.ok(&w, &["rev-parse", "HEAD"]).trim().to_owned();
    // Behind Slath's back: a new ref and a deleted one.
    git.ok(&bare, &["update-ref", "refs/heads/sneaky", &head]);
    git.ok(&bare, &["update-ref", "-d", "refs/heads/doomed"]);
    let (ok, out, _) = s.slath(&["fsck", "--output", "json"], None);
    assert!(!ok, "differences are reported: {out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let diffs = v["repositories"][0]["ref_differences"].as_array().unwrap();
    assert_eq!(diffs.len(), 2, "{out}");
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM push_events WHERE transport = 'reconcile'"),
        "0",
        "fsck alone changes nothing"
    );
    let (ok, out, _) = s.slath(&["fsck", "--reconcile", "--output", "json"], None);
    assert!(ok, "{out}");
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM push_events WHERE transport = 'reconcile'"),
        "1"
    );
    let refs = s.db.query_one(
        "SELECT string_agg(convert_from(name, 'UTF8'), ',' ORDER BY name) FROM git_refs",
    );
    assert_eq!(refs, "refs/heads/main,refs/heads/sneaky");
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM jobs WHERE kind = 'ref.updated'"),
        "2"
    );
    // Clean afterwards.
    let (ok, out, _) = s.slath(&["fsck"], None);
    assert!(ok, "{out}");
    assert!(out.contains("clean"));
}

#[test]
fn conf_ops_06_lost_post_receive_is_repaired_by_the_periodic_reconciler() {
    let s = TestServer::start_full(&[DEMO], "[maintenance]\nreconcile_interval = \"1s\"\n");
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    push_main(&s, &git, &d.path().join("w"));
    // As if post-receive had never reached the database.
    s.db.execute("DELETE FROM git_refs; DELETE FROM push_events;");
    for _ in 0..50 {
        if s.db.query_one("SELECT count(*)::text FROM git_refs") == "2" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert_eq!(s.db.query_one("SELECT count(*)::text FROM git_refs"), "2");
    assert_eq!(
        s.db.query_one("SELECT transport FROM push_events"),
        "reconcile"
    );
}

#[test]
fn fsck_reports_missing_data_and_orphans() {
    let s = TestServer::start();
    let bare = s.repo_path(&DEMO);
    let orphan = s
        .data_dir()
        .join("repos")
        .join("ff")
        .join("ffffffff-0000-7000-8000-000000000000.git");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::rename(&bare, bare.with_extension("moved")).unwrap();
    let (ok, out, _) = s.slath(&["fsck"], None);
    assert!(!ok);
    assert!(out.contains("missing  alice/demo"), "{out}");
    assert!(out.contains("orphan"), "{out}");
}
