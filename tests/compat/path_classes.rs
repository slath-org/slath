// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Path classes (REVIEW-15): classes come only from `.slath/paths.toml` on
//! the default branch and from the built-in set, they're facts per change
//! version that Git-made, jj-made, and mixed stacks share, and invalid
//! entries are reported rather than ignored.

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, TestServer};

use crate::facts::{with_jj_and_mixed, write};
use crate::fork_merge_requests::{api, eventually, token};

const PATHS_TOML: &str = r#"
[migration]
paths = ["db/migrations/**"]

[tests]
paths = ["checks/**"]

[ci]
paths = ["build/pipeline.yml"]

[Deploy]
paths = ["deploy/**"]

[docs]
paths = ["/docs"]
"#;

/// Opens a merge request for `branch` and waits for its first revision.
fn open(s: &TestServer, token: &str, branch: &str) -> String {
    let r = api(
        s,
        token,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_branch": branch, "title": branch})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let path = format!("/alice/demo/merge-requests/{}", r.json()["number"]);
    eventually(s, "no first revision", || {
        api(s, token, "GET", &path, None).json()["revisions"] == 1
    });
    path
}

fn names(v: &Value) -> Vec<&str> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|c| c["class"].as_str().unwrap())
        .collect()
}

#[test]
fn conf_review_15_classes_come_from_the_default_branch_for_every_tool() {
    let s = TestServer::start();
    let alice = token(&s, "alice");
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    let url = s.url(&DEMO);

    git.ok(&w, &["init", "-q", "-b", "main"]);
    write(&w, ".slath/paths.toml", PATHS_TOML.as_bytes());
    write(&w, "db/migrations/0001.sql", b"create table a ();\n");
    write(&w, "checks/smoke.sh", b"true\n");
    write(&w, "tests/a_test.rs", b"#[test]\nfn t() {}\n");
    write(&w, "src/auth/token.rs", b"fn t() {}\n");
    write(&w, "Dockerfile", b"FROM scratch\n");
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "init"]);
    git.ok(&w, &["push", "-q", &url, "main"]);

    git.ok(&w, &["switch", "-q", "-c", "git-stack"]);
    // First: a migration, and a deleted test of each kind.
    write(&w, "db/migrations/0002.sql", b"create table b ();\n");
    git.ok(&w, &["rm", "-q", "checks/smoke.sh", "tests/a_test.rs"]);
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "First"]);
    // Second: the file defines a class for docs, and docs change. The
    // merge request is still read with the default branch's classes.
    write(
        &w,
        ".slath/paths.toml",
        format!("{PATHS_TOML}\n[handbook]\npaths = [\"handbook/**\"]\n").as_bytes(),
    );
    write(&w, "handbook/a.md", b"# A\n");
    write(&w, "Dockerfile", b"FROM alpine\n");
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "Second"]);
    git.ok(&w, &["push", "-q", &url, "git-stack"]);

    let whole = json!([
        {"class": "containers", "paths": ["Dockerfile"]},
        {"class": "migration", "paths": ["db/migrations/0002.sql"]},
        {"class": "tests", "paths": ["checks/smoke.sh"]},
    ]);
    for branch in with_jj_and_mixed(d.path(), &url, "git-stack") {
        let path = open(&s, &alice, &branch);
        let f = api(&s, &alice, "GET", &format!("{path}/facts"), None).json();
        assert_eq!(f["classes"], whole, "{branch}: {f}");
        // The repository's `tests` class replaces the built-in guess.
        assert_eq!(f["deleted_tests"], json!(["checks/smoke.sh"]), "{branch}");
        // `.slath/paths.toml` is sensitive itself.
        assert_eq!(f["sensitive_paths"], json!([".slath/paths.toml"]));
        let mb = api(&s, &alice, "GET", &format!("{path}/mergeability"), None).json();
        assert_eq!(mb["facts"]["classes"], whole, "{branch}");

        let stack = api(&s, &alice, "GET", &format!("{path}/stack"), None).json();
        let commits: Vec<&str> = stack["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["commit_oid"].as_str().unwrap())
            .collect();
        let of = |c: &str| api(&s, &alice, "GET", &format!("{path}/facts?commit={c}"), None).json();
        assert_eq!(
            names(&of(commits[0])["classes"]),
            ["migration", "tests"],
            "{branch}"
        );
        assert_eq!(
            names(&of(commits[1])["classes"]),
            ["containers"],
            "{branch}"
        );
    }

    // The validation view: what's in effect, the errors, and proposals.
    let v = api(&s, &alice, "GET", "/alice/demo/path-classes", None);
    assert_eq!(v.status, 200, "{}", v.body);
    let v = v.json();
    assert_eq!(v["has_file"], true);
    assert_eq!(v["files_cut"], false);
    let classes: Vec<(&str, bool, u64)> = v["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap(),
                c["built_in"].as_bool().unwrap(),
                c["files"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        classes,
        [
            ("ci", true, 0),
            ("containers", true, 1),
            ("dependencies", true, 0),
            ("instructions", true, 0),
            ("migration", false, 1),
            ("owners", true, 0),
            ("tests", false, 1),
        ]
    );
    let ci = &v["classes"][0];
    assert_eq!(ci["file_globs"], json!(["build/pipeline.yml"]));
    assert_eq!(
        v["errors"],
        json!([
            {"class": "Deploy", "message": "The class name must start with a lowercase letter."},
            {"class": "docs", "message": "The path \"/docs\" must not start or end with /."},
        ])
    );
    assert_eq!(
        v["proposals"],
        json!([{"name": "auth", "globs": ["src/auth/**"]}])
    );

    // Readers only.
    let anon = s
        .client()
        .api("GET", "/api/v1/repos/alice/demo/path-classes", None, None);
    assert_eq!(anon.status, 404);
}

#[test]
fn conf_review_15_an_unreadable_file_leaves_the_built_in_classes() {
    let s = TestServer::start();
    let alice = token(&s, "alice");
    // An empty repository has the built-in classes and nothing else.
    let v = api(&s, &alice, "GET", "/alice/demo/path-classes", None).json();
    assert_eq!(v["has_file"], false);
    assert_eq!(v["classes"].as_array().unwrap().len(), 5, "{v}");

    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    git.ok(w, &["init", "-q", "-b", "main"]);
    write(w, ".slath/paths.toml", b"[migration\npaths = 1\n");
    write(w, "db/migrations/0001.sql", b"x\n");
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &s.url(&DEMO), "main"]);
    let v = api(&s, &alice, "GET", "/alice/demo/path-classes", None).json();
    assert_eq!(v["classes"].as_array().unwrap().len(), 5, "{v}");
    let errors = v["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{v}");
    assert_eq!(errors[0]["class"], Value::Null);
    assert!(
        errors[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("The file isn't valid TOML; only the built-in classes apply"),
        "{v}"
    );
    // A class the file can't define is still proposed from the tree.
    assert_eq!(v["proposals"][0]["name"], "migration", "{v}");
}
