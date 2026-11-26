// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Facts (AGENT-14, D-40): on a fixture corpus, the facts of a revision and
//! of each change version are exact, the mergeability data carries them,
//! and Git-made, jj-made, and mixed stacks give the same facts.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, TestServer};

use crate::fork_merge_requests::{api, eventually, token};
use crate::jj::{Jj, jj_binaries};

fn write(w: &Path, file: &str, data: &[u8]) {
    if let Some(dir) = Path::new(file).parent() {
        std::fs::create_dir_all(w.join(dir)).unwrap();
    }
    std::fs::write(w.join(file), data).unwrap();
}

fn cargo_lock(packages: &[(&str, &str)]) -> String {
    let mut s = "version = 4\n".to_owned();
    for (n, v) in packages {
        s.push_str(&format!(
            "\n[[package]]\nname = \"{n}\"\nversion = \"{v}\"\n"
        ));
    }
    s
}

/// The main branch, and a two-commit stack with every kind of fact.
fn fixture(git: &GitClient, w: &Path, url: &str) {
    git.ok(w, &["init", "-q", "-b", "main"]);
    write(
        w,
        "Cargo.lock",
        cargo_lock(&[("serde", "1.0.1"), ("old", "0.1.0")]).as_bytes(),
    );
    write(w, "requirements.txt", b"requests==2.31.0\n");
    write(w, "package.json", b"{}\n");
    write(w, "tests/old_test.rs", b"#[test]\nfn t() {}\n");
    write(w, "src/a_test.go", b"package a\n");
    write(w, "run.sh", b"#!/bin/sh\n");
    write(w, "AGENTS.md", b"Be nice.\n");
    write(w, "src/lib.rs", b"fn a() {}\n");
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", url, "main"]);

    git.ok(w, &["switch", "-q", "-c", "git-stack"]);
    // First: dependencies, a deleted test, a binary, modes.
    write(
        w,
        "Cargo.lock",
        cargo_lock(&[("serde", "1.0.2"), ("rand", "0.9.0")]).as_bytes(),
    );
    write(w, "package.json", b"{\"name\": \"x\"}\n");
    git.ok(w, &["rm", "-q", "tests/old_test.rs", "src/a_test.go"]);
    write(w, "logo.bin", b"\x00\x01\x02binary");
    std::os::unix::fs::symlink("run.sh", w.join("run")).unwrap();
    std::fs::set_permissions(
        w.join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["update-index", "--chmod=+x", "run.sh"]);
    git.ok(w, &["commit", "-q", "-m", "First"]);
    // Second: a sensitive path and a possible secret, built here so the
    // repository's own secret check doesn't see one.
    write(w, "AGENTS.md", b"Be very nice.\n");
    let assignment = format!("{}word = \"{}\"\n", "pass", "Zq7rT2mW9xLp4vNs");
    write(
        w,
        "src/config.py",
        format!("DEBUG = True\n{assignment}").as_bytes(),
    );
    write(w, "requirements.txt", b"requests==2.31.0\nDjango==4.2.1\n");
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Second"]);
    git.ok(w, &["push", "-q", url, "git-stack"]);
}

/// The facts of the whole stack.
fn whole() -> Value {
    json!({
        "size": {"files": 10, "additions": 9, "deletions": 8},
        "outside_scope": null,
        "sensitive_paths": ["AGENTS.md"],
        "dependencies": [
            {"path": "Cargo.lock", "ecosystem": "cargo", "packages": true,
             "added": [{"name": "rand", "version": "0.9.0"}],
             "removed": [{"name": "old", "version": "0.1.0"}],
             "updated": [{"name": "serde", "from": "1.0.1", "to": "1.0.2"}]},
            {"path": "package.json", "ecosystem": "npm", "packages": false,
             "added": [], "removed": [], "updated": []},
            {"path": "requirements.txt", "ecosystem": "python", "packages": true,
             "added": [{"name": "django", "version": "4.2.1"}],
             "removed": [], "updated": []},
        ],
        "deleted_tests": ["src/a_test.go", "tests/old_test.rs"],
        "added_binaries": ["logo.bin"],
        "mode_changes": [
            {"path": "run", "old_mode": null, "new_mode": "120000"},
            {"path": "run.sh", "old_mode": "100644", "new_mode": "100755"},
        ],
        "secrets": [{"path": "src/config.py", "line": 2, "rule": "generic-assignment",
                     "description": "Possible password or key in an assignment",
                     "tier": "warn"}],
        "incomplete": [],
    })
}

#[test]
fn conf_agent_14_facts_are_exact_for_git_jj_and_mixed_stacks() {
    let s = TestServer::start();
    let alice = token(&s, "alice");
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    let url = s.url(&DEMO);
    fixture(&git, &w, &url);

    // The same trees, rewritten by jj (every change) and mixed (only the
    // second).
    let mut stacks = vec!["git-stack"];
    if let Some((version, bin)) = jj_binaries().into_iter().last() {
        let jj = Jj::new(&version, &bin);
        let j = d.path().join("jj");
        jj.ok(d.path(), &["git", "clone", &url, j.to_str().unwrap()]);
        jj.ok(&j, &["bookmark", "track", "git-stack@origin"]);
        jj.ok(&j, &["describe", "-r", "git-stack", "-m", "Second (jj)"]);
        jj.ok(
            &j,
            &["bookmark", "create", "mixed-stack", "-r", "git-stack"],
        );
        jj.ok(&j, &["describe", "-r", "git-stack-", "-m", "First (jj)"]);
        jj.ok(&j, &["bookmark", "create", "jj-stack", "-r", "git-stack"]);
        jj.ok(
            &j,
            &[
                "git",
                "push",
                "--bookmark",
                "mixed-stack",
                "--bookmark",
                "jj-stack",
            ],
        );
        stacks.extend(["jj-stack", "mixed-stack"]);
    }

    for branch in &stacks {
        let r = api(
            &s,
            &alice,
            "POST",
            "/alice/demo/merge-requests",
            Some(json!({"source_branch": branch, "title": branch})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        let path = format!("/alice/demo/merge-requests/{n}");
        eventually(&s, "no first revision", || {
            api(&s, &alice, "GET", &path, None).json()["revisions"] == 1
        });
        let f = api(&s, &alice, "GET", &format!("{path}/facts"), None);
        assert_eq!(f.status, 200, "{}", f.body);
        assert_eq!(f.json(), whole(), "{branch}");
        // Mergeability carries the same.
        let mb = api(&s, &alice, "GET", &format!("{path}/mergeability"), None).json();
        assert_eq!(mb["facts"], whole(), "{branch}");

        // Each change version has its own.
        let stack = api(&s, &alice, "GET", &format!("{path}/stack"), None).json();
        let commits: Vec<String> = stack["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["commit_oid"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(commits.len(), 2, "{branch}");
        let first = api(
            &s,
            &alice,
            "GET",
            &format!("{path}/facts?commit={}", commits[0]),
            None,
        )
        .json();
        assert_eq!(first["sensitive_paths"], json!([]), "{branch}");
        assert_eq!(first["secrets"], json!([]), "{branch}");
        assert_eq!(first["dependencies"].as_array().unwrap().len(), 2);
        assert_eq!(first["deleted_tests"], whole()["deleted_tests"]);
        assert_eq!(first["added_binaries"], json!(["logo.bin"]));
        assert_eq!(first["mode_changes"], whole()["mode_changes"]);
        let second = api(
            &s,
            &alice,
            "GET",
            &format!("{path}/facts?commit={}", commits[1]),
            None,
        )
        .json();
        assert_eq!(second["sensitive_paths"], json!(["AGENTS.md"]));
        assert_eq!(second["secrets"], whole()["secrets"]);
        assert_eq!(second["deleted_tests"], json!([]));
        assert_eq!(second["added_binaries"], json!([]));
        assert_eq!(second["mode_changes"], json!([]));
        assert_eq!(
            second["dependencies"],
            json!([whole()["dependencies"][2].clone()])
        );
        assert_eq!(
            second["size"],
            json!({"files": 3, "additions": 4, "deletions": 1})
        );
        // A commit that isn't a version of it, and a revision it doesn't
        // have, are not found.
        let other = api(&s, &alice, "GET", &format!("{path}/facts?revision=9"), None);
        assert_eq!(other.status, 404);
    }

    // Readers only: on the private repository an anonymous reader finds
    // nothing; once it's public, the facts are there.
    let facts = "/api/v1/repos/alice/demo/merge-requests/1/facts";
    assert_eq!(s.client().api("GET", facts, None, None).status, 404);
    let r = api(
        &s,
        &alice,
        "PATCH",
        "/alice/demo",
        Some(json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let anon = s.client().api("GET", facts, None, None);
    assert_eq!(anon.status, 200, "{}", anon.body);
}

#[test]
fn conf_agent_14_a_lockfile_that_cant_be_read_is_named_not_guessed() {
    let s = TestServer::start_full(&[DEMO], "[limits]\nfacts_file_max_bytes = 200\n");
    let alice = token(&s, "alice");
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let url = s.url(&DEMO);
    git.ok(w, &["init", "-q", "-b", "main"]);
    write(w, "Cargo.lock", cargo_lock(&[("a", "1")]).as_bytes());
    write(w, "go.mod", b"module m\n");
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &url, "main"]);
    git.ok(w, &["switch", "-q", "-c", "deps"]);
    let many: Vec<(String, String)> = (0..20).map(|i| (format!("p{i}"), "1".into())).collect();
    let many: Vec<(&str, &str)> = many.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    write(w, "Cargo.lock", cargo_lock(&many).as_bytes());
    write(w, "package-lock.json", b"{ not json");
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "deps"]);
    git.ok(w, &["push", "-q", &url, "deps"]);
    let r = api(
        &s,
        &alice,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_branch": "deps", "title": "deps"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let path = format!("/alice/demo/merge-requests/{}", r.json()["number"]);
    eventually(&s, "no first revision", || {
        api(&s, &alice, "GET", &path, None).json()["revisions"] == 1
    });
    let f = api(&s, &alice, "GET", &format!("{path}/facts"), None).json();
    let deps = f["dependencies"].as_array().unwrap();
    assert_eq!(deps.len(), 2, "{f}");
    assert!(deps.iter().all(|d| d["packages"] == false), "{f}");
    let notes: Vec<&str> = f["incomplete"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap())
        .collect();
    assert_eq!(notes.len(), 2, "{f}");
    assert!(
        notes[0].starts_with("Cargo.lock is larger than limits.facts_file_max_bytes"),
        "{notes:?}"
    );
    assert!(
        notes[1].starts_with("package-lock.json couldn't be read"),
        "{notes:?}"
    );
}
