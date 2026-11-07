// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reserved refs (GIT-09): merge request heads are advertised read-only,
//! internal refs are hidden, and the reconciler keeps revision pins.

use slath_tests::server::{DEMO, GitClient, TestServer};

#[test]
fn conf_git_09_reserved_refs_are_hidden_or_read_only_and_pins_are_kept() {
    let s = TestServer::start();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("a.txt"), "a\n").unwrap();
    git.ok(&w, &["add", "a.txt"]);
    git.ok(&w, &["commit", "-q", "-m", "first"]);
    let first = git.ok(&w, &["rev-parse", "HEAD"]).trim().to_owned();
    std::fs::write(w.join("a.txt"), "b\n").unwrap();
    git.ok(&w, &["commit", "-q", "-am", "second"]);
    let second = git.ok(&w, &["rev-parse", "HEAD"]).trim().to_owned();
    let url = s.url(&DEMO);
    git.ok(&w, &["push", "-q", &url, "main"]);

    // What the server would write for merge request !1.
    let mr_id = "0192f000-0000-7000-8000-000000000001";
    let bare = s.repo_path(&DEMO);
    git.ok(&bare, &["update-ref", "refs/merge-requests/1/head", &first]);
    let pin = format!("refs/slath/mr/{mr_id}/r1");
    git.ok(&bare, &["update-ref", &pin, &first]);

    let advertised = git.ok(&w, &["ls-remote", &url]);
    assert!(
        advertised.contains("refs/merge-requests/1/head"),
        "{advertised}"
    );
    assert!(!advertised.contains("refs/slath"), "{advertised}");
    git.ok(&w, &["fetch", "-q", &url, "refs/merge-requests/1/head"]);
    assert_eq!(git.ok(&w, &["rev-parse", "FETCH_HEAD"]).trim(), first);
    // A hidden ref can't be fetched by name.
    git.fails(&w, &["fetch", "-q", &url, &pin]);

    // Clients can't write either kind.
    for target in [
        "refs/merge-requests/1/head",
        "refs/merge-requests/2/head",
        pin.as_str(),
    ] {
        let err = git.fails(&w, &["push", &url, &format!("HEAD:{target}")]);
        assert!(err.contains("reserved"), "{target}: {err}");
    }
    assert_eq!(
        git.ok(&bare, &["rev-parse", "refs/merge-requests/1/head"])
            .trim(),
        first
    );

    // Server-side refs aren't part of the ref snapshot.
    let (ok, out, _) = s.slath(&["fsck"], None);
    assert!(ok, "{out}");

    // A revision whose pin went missing is repinned while its commit exists.
    s.db.execute(&format!(
        "INSERT INTO merge_requests (id, repo_id, number, title, target_branch, source_repo_id, source_ref)
         SELECT '{mr_id}', id, 1, 'Topic', 'main', id, 'refs/heads/topic'::bytea FROM repositories;
         INSERT INTO mr_revisions (id, mr_id, seq, base_oid, head_oid)
         VALUES (gen_random_uuid(), '{mr_id}', 1, '{first}', '{first}'),
                (gen_random_uuid(), '{mr_id}', 2, '{first}', '{second}');"
    ));
    let (ok, out, _) = s.slath(&["fsck", "--output", "json"], None);
    assert!(!ok, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["repositories"][0]["unpinned_revisions"], 1, "{out}");
    let (ok, out, _) = s.slath(&["fsck", "--reconcile"], None);
    assert!(ok, "{out}");
    assert_eq!(
        git.ok(&bare, &["rev-parse", &format!("refs/slath/mr/{mr_id}/r2")])
            .trim(),
        second
    );
    let (ok, out, _) = s.slath(&["fsck"], None);
    assert!(ok, "{out}");

    // A revision whose commits are gone is flagged, never silently dropped.
    let gone = "1111111111111111111111111111111111111111";
    s.db.execute(&format!(
        "INSERT INTO mr_revisions (id, mr_id, seq, base_oid, head_oid)
         VALUES (gen_random_uuid(), '{mr_id}', 3, '{first}', '{gone}')"
    ));
    let (ok, out, _) = s.slath(&["fsck", "--reconcile"], None);
    assert!(!ok, "{out}");
    assert!(out.contains("revision 3: commits are gone"), "{out}");
    assert_eq!(
        s.db.query_one("SELECT missing_objects::text FROM mr_revisions WHERE seq = 3"),
        "true"
    );
}
