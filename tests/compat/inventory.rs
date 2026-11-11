// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Diff inventory (DIFF-02, D-54): on a fixture with every kind of change,
//! each file of a revision and of each change version is in exactly one
//! class, limits are named, files past the file limit are counted, and
//! Git-made, jj-made, and mixed stacks give the same inventory.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, TestServer};

use crate::fork_merge_requests::{api, eventually, token};
use crate::jj::{Jj, jj_binaries};

/// A synthesized commit for a submodule entry (no second repository).
fn submodule_commit(git: &GitClient, w: &Path, msg: &str) -> String {
    let tree = String::from_utf8(
        git.cmd(w)
            .args(["mktree"])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    git.ok(w, &["commit-tree", "-m", msg, tree.trim()])
        .trim()
        .to_owned()
}

fn write(w: &Path, file: &str, data: &[u8]) {
    std::fs::write(w.join(file), data).unwrap();
}

/// The main branch, and a two-commit stack touching every class.
fn fixture(git: &GitClient, w: &Path, url: &str) {
    git.ok(w, &["init", "-q", "-b", "main"]);
    write(w, "a.txt", b"one\ntwo\n");
    write(w, "ws.txt", b"a  b\n");
    write(w, "old.txt", b"rename me\n");
    write(w, "run.sh", b"#!/bin/sh\n");
    write(w, "gen.txt", b"g\n");
    write(w, ".gitattributes", b"gen.txt linguist-generated\n");
    write(w, "bin.dat", b"\x00\x01binary");
    write(w, "y-big.txt", b"start\n");
    write(w, "zz1.txt", b"1\n");
    write(w, "zz2.txt", b"2\n");
    std::os::unix::fs::symlink("a.txt", w.join("link")).unwrap();
    git.ok(w, &["add", "-A"]);
    let sub = submodule_commit(git, w, "sub 1");
    git.ok(
        w,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{sub},vendor/lib"),
        ],
    );
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", url, "main"]);

    git.ok(w, &["switch", "-q", "-c", "git-stack"]);
    write(w, "a.txt", b"ONE\ntwo\n");
    git.ok(w, &["mv", "old.txt", "new.txt"]);
    std::fs::set_permissions(
        w.join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    git.ok(w, &["update-index", "--chmod=+x", "run.sh"]);
    write(w, "ws.txt", b"a b\n");
    git.ok(w, &["add", "a.txt", "ws.txt"]);
    git.ok(w, &["commit", "-q", "-m", "First"]);
    write(w, "gen.txt", b"G\n");
    write(w, "bin.dat", b"\x00\x02binary");
    std::fs::remove_file(w.join("link")).unwrap();
    std::os::unix::fs::symlink("ws.txt", w.join("link")).unwrap();
    let big: String = (0..1000).map(|i| format!("line {i}\n")).collect();
    write(w, "y-big.txt", big.as_bytes());
    write(w, "zz1.txt", b"one\n");
    write(w, "zz2.txt", b"two\n");
    git.ok(w, &["add", "-A"]);
    let sub = submodule_commit(git, w, "sub 2");
    git.ok(
        w,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{sub},vendor/lib"),
        ],
    );
    git.ok(w, &["commit", "-q", "-m", "Second"]);
    git.ok(w, &["push", "-q", url, "git-stack"]);
}

/// `(path, class)` of an inventory's files, and its left-out count.
type Classes = (Vec<(String, String)>, i64);

fn classes(inv: &Value) -> Classes {
    let files = inv["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["class"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    (files, inv["left_out"].as_i64().unwrap())
}

#[test]
fn conf_diff_02_every_file_in_one_class_for_git_jj_and_mixed_stacks() {
    let s = TestServer::start_full(
        &[DEMO],
        "[limits]\ndiff_max_files = 10\ndiff_max_lines = 300\n",
    );
    let alice = token(&s, "alice");
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    let url = s.url(&DEMO);
    fixture(&git, &w, &url);

    // The same stack, rewritten by jj (every change) and mixed (only the
    // second), so the trees are the same.
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

    let expected: Vec<(String, String)> = [
        ("a.txt", "content"),
        ("bin.dat", "binary"),
        ("gen.txt", "generated"),
        ("link", "symlink"),
        ("new.txt", "rename"),
        ("run.sh", "mode_only"),
        ("vendor/lib", "submodule"),
        ("ws.txt", "whitespace_only"),
        ("y-big.txt", "not_shown"),
        ("zz1.txt", "not_shown"),
    ]
    .iter()
    .map(|(p, c)| ((*p).to_owned(), (*c).to_owned()))
    .collect();

    let mut versions: Vec<Vec<Classes>> = Vec::new();
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
        // The revision: every file once, limits named.
        let inv = api(&s, &alice, "GET", &format!("{path}/inventory"), None);
        assert_eq!(inv.status, 200, "{}", inv.body);
        let inv = inv.json();
        assert_eq!(classes(&inv), (expected.clone(), 1), "{branch}: {inv}");
        assert_eq!(inv["files_limit"], "diff_max_files");
        let big = inv["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == "y-big.txt")
            .unwrap();
        assert!(
            big["reason"].as_str().unwrap().contains("diff_max_lines"),
            "{big}"
        );
        let a = inv["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == "a.txt")
            .unwrap();
        assert_eq!(a["hunks"], json!(["content"]));
        assert_eq!(a["folded"], false);
        // The Changes tab lists the same folded files with their reasons.
        let mut web = s.login("alice", slath_tests::server::PASSWORD);
        let html = web.get(&format!("{path}/changes")).body;
        for f in inv["files"].as_array().unwrap() {
            let entry = format!(
                "<code>{}</code></a>: {}</li>",
                f["path"].as_str().unwrap(),
                f["reason"].as_str().unwrap()
            );
            assert_eq!(
                html.contains(&entry),
                f["folded"] == true,
                "{branch}: {entry}"
            );
        }
        assert!(
            html.contains("1 more file changed but isn't listed"),
            "{branch}"
        );
        // Each change version.
        let stack = api(&s, &alice, "GET", &format!("{path}/stack"), None).json();
        let mut per = Vec::new();
        for e in stack["entries"].as_array().unwrap() {
            let c = e["commit_oid"].as_str().unwrap();
            let v = api(
                &s,
                &alice,
                "GET",
                &format!("{path}/inventory?commit={c}"),
                None,
            );
            assert_eq!(v.status, 200, "{}", v.body);
            per.push(classes(&v.json()));
        }
        assert_eq!(per.len(), 2, "{branch}: two changes");
        assert_eq!(
            per[0].0,
            [
                ("a.txt", "content"),
                ("new.txt", "rename"),
                ("run.sh", "mode_only"),
                ("ws.txt", "whitespace_only")
            ]
            .iter()
            .map(|(p, c)| ((*p).to_owned(), (*c).to_owned()))
            .collect::<Vec<_>>(),
            "{branch}"
        );
        versions.push(per);
    }
    assert!(versions.windows(2).all(|w| w[0] == w[1]), "{versions:?}");

    // The commit API classifies the same way.
    let head = git.ok(&w, &["rev-parse", "git-stack~1"]);
    let c = api(
        &s,
        &alice,
        "GET",
        &format!("/alice/demo/commits/{}", head.trim()),
        None,
    )
    .json();
    let from_commit: Vec<(String, String)> = c["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["class"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(from_commit, versions[0][0].0, "{c}");
}
