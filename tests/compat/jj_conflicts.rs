// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conflicts recorded by jj (JJ-07): commits made by real jj that hold a
//! conflict are shown as conflicted, never as a clean tree, by either
//! signal (the `jj:trees` header or the `.jjconflict-*` tree entries), and
//! they block merging until a push resolves them.

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use serde_json::{Value, json};
use slath_tests::server::PASSWORD;

use crate::change_identity::{Ctx, entries, seed};
use crate::jj::{git_store, jj_binaries};

fn conflicted(stack: &Value) -> Vec<bool> {
    entries(stack)
        .iter()
        .map(|e| e["conflicted"].as_bool().unwrap())
        .collect()
}

/// Writes `raw` as a commit object into `git_dir`, returning its ID.
fn write_commit(c: &Ctx, git_dir: &Path, raw: &str) -> String {
    let mut child = c
        .git
        .cmd(git_dir)
        .args(["hash-object", "-t", "commit", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(raw.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// The commit `raw` without jj's conflict headers (multi-line headers
/// continue with a space).
fn without_jj_headers(raw: &str) -> String {
    let (headers, message) = raw.split_once("\n\n").unwrap();
    let mut out = Vec::new();
    let mut dropping = false;
    for line in headers.lines() {
        if line.starts_with(' ') {
            if !dropping {
                out.push(line);
            }
            continue;
        }
        dropping = line.starts_with("jj:");
        if !dropping {
            out.push(line);
        }
    }
    format!("{}\n\n{message}", out.join("\n"))
}

fn merge_blocked(c: &Ctx, n: i64, head: &str) -> String {
    let m = c
        .api("GET", &format!("/merge-requests/{n}/mergeability"), None)
        .json();
    let identity = m["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "identity")
        .unwrap()
        .clone();
    assert_eq!(identity["state"], "unmet", "{m}");
    let r = c.api(
        "POST",
        &format!("/merge-requests/{n}/merge"),
        Some(
            &json!({"strategy": "merge-commit", "expected_head_oid": head, "delete_source": false}),
        ),
    );
    assert_eq!(r.status, 409, "{}", r.body);
    identity["reason"].as_str().unwrap().to_owned()
}

#[test]
fn conf_jj_07_real_jj_conflicts_are_detected_by_either_signal_and_block_merging() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    seed(&c, d.path());
    let mut ui = c.s.login("alice", PASSWORD);
    for (version, bin) in jj_binaries() {
        let v = version.replace(['.', ' '], "_");
        let jj = crate::jj::Jj::new(&version, &bin);
        let j = d.path().join(format!("jj-{v}"));
        jj.ok(d.path(), &["git", "clone", &c.url, j.to_str().unwrap()]);
        // A clean change, then one whose parent jj abandons: rebased onto
        // the clean change, its edit of README conflicts. The change above
        // it inherits the conflict.
        jj.ok(&j, &["new", "main@origin", "-m", "Add k"]);
        std::fs::write(j.join("k.txt"), "k\n").unwrap();
        let k = jj.show(&j, "@", "change_id");
        jj.ok(&j, &["new", "-m", "README one"]);
        std::fs::write(j.join("README"), "one\n").unwrap();
        let one = jj.show(&j, "@", "change_id");
        jj.ok(&j, &["new", "-m", "README two"]);
        std::fs::write(j.join("README"), "two\n").unwrap();
        let two = jj.show(&j, "@", "change_id");
        jj.ok(&j, &["new", "-m", "Add top"]);
        std::fs::write(j.join("top.txt"), "top\n").unwrap();
        let top = jj.show(&j, "@", "change_id");
        jj.ok(&j, &["abandon", &one]);
        assert_eq!(jj.show(&j, &two, "conflict"), "true", "{version}");
        assert_eq!(jj.show(&j, &top, "conflict"), "true", "{version}");
        let head = jj.show(&j, &top, "commit_id");
        // jj refuses to push conflicts; Git in the same repository doesn't.
        let store = git_store(&j);
        let bookmark = format!("conflict-{v}");
        let clean = jj.show(&j, &k, "commit_id");
        c.git.ok(
            &store,
            &[
                "push",
                "--quiet",
                &c.url,
                &format!("{clean}:refs/heads/{bookmark}"),
            ],
        );
        let n = c.open_mr(&bookmark);
        c.stack(n, 1);
        let out = c
            .git
            .cmd(&store)
            .args(["push", &c.url, &format!("{head}:refs/heads/{bookmark}")])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{version}: {err}");
        assert!(
            err.contains(&format!(
                "2 changes have a conflict recorded by jj; resolve {} first: ",
                &two[..8]
            )) && err.contains(&format!("/changes/{two}")),
            "{version}: {err}"
        );
        let s = c.stack(n, 2);
        assert_eq!(conflicted(&s), [false, true, true], "{version}: {s}");
        assert_eq!(s["merge_blocked"], true, "{version}");
        let reason = merge_blocked(&c, n, &head);
        assert!(reason.contains("2 commits with a conflict"), "{reason}");
        // The list and the readiness card name it.
        let list = ui.get("/alice/demo/merge-requests?state=all").body;
        assert!(
            list.contains(r#"tone-blocking" data-tip="Conflict recorded by jj">"#),
            "{version}: {list}"
        );

        // The conflicted change's page explains the stored tree and how to
        // resolve it; the change above points at it.
        let r = ui.get(&format!("/alice/demo/merge-requests/{n}/changes/{two}"));
        assert_eq!(r.status, 200, "{}", r.body);
        assert!(r.body.contains("Conflict recorded by jj"), "{version}");
        assert!(r.body.contains(".jjconflict-side-*"), "{version}");
        for line in [
            format!("jj bookmark track {bookmark}@origin"),
            format!("jj new {two}"),
            format!("jj git push --bookmark {bookmark}"),
        ] {
            assert!(r.body.contains(&line), "{version}: {line}");
        }
        assert!(!r.body.contains("below has a conflict too"), "{version}");
        let r = ui.get(&format!("/alice/demo/merge-requests/{n}/changes/{top}"));
        assert!(r.body.contains("below has a conflict too"), "{version}");
        assert!(r.body.contains(&format!("jj new {two}")), "{version}");
        // The clean change has no frame (its stack column still names the
        // others' conflicts).
        let r = ui.get(&format!("/alice/demo/merge-requests/{n}/changes/{k}"));
        assert!(!r.body.contains(r#"id="conflict-title""#), "{version}");
        assert!(r.body.contains("Conflict recorded by jj"), "{version}");

        // Each signal alone: the tree entries without jj's headers, and
        // the header on a clean tree, each under a change ID of its own.
        let two_oid = jj.show(&j, &two, "commit_id");
        let raw = c.git.ok(&store, &["cat-file", "commit", &two_oid]);
        assert!(raw.contains("\njj:trees "), "{version}: {raw}");
        let own_id = |raw: &str, letter: char| {
            raw.replace(
                &format!("change-id {two}"),
                &format!("change-id {}", letter.to_string().repeat(32)),
            )
        };
        let tree_only = write_commit(&c, &store, &own_id(&without_jj_headers(&raw), 'y'));
        let clean_tree = c
            .git
            .ok(&store, &["rev-parse", &format!("{two_oid}~1^{{tree}}")]);
        let (first, rest) = raw.split_once('\n').unwrap();
        assert!(first.starts_with("tree "));
        let header_only = write_commit(
            &c,
            &store,
            &own_id(&format!("tree {}\n{rest}", clean_tree.trim()), 'z'),
        );
        let ls = c.git.ok(&store, &["ls-tree", "--name-only", &header_only]);
        assert!(!ls.contains(".jjconflict-"), "{version}: {ls}");
        for (name, oid) in [("tree", &tree_only), ("header", &header_only)] {
            let branch = format!("{name}-only-{v}");
            c.git.ok(
                &store,
                &[
                    "push",
                    "--quiet",
                    &c.url,
                    &format!("{oid}:refs/heads/{branch}"),
                ],
            );
            let m = c.open_mr(&branch);
            let s = c.stack(m, 1);
            assert_eq!(conflicted(&s), [false, true], "{version} {name}: {s}");
            let reason = merge_blocked(&c, m, oid);
            assert!(
                reason.contains("1 commit with a conflict"),
                "{name}: {reason}"
            );
        }

        // The page's jj commands resolve it, and the push unblocks the
        // merge request.
        jj.ok(&j, &["git", "fetch"]);
        jj.ok(&j, &["bookmark", "track", &format!("{bookmark}@origin")]);
        jj.ok(&j, &["new", &two]);
        std::fs::write(j.join("README"), "two\n").unwrap();
        jj.ok(&j, &["squash"]);
        assert_eq!(jj.show(&j, &top, "conflict"), "false", "{version}");
        jj.ok(&j, &["bookmark", "set", &bookmark, "-r", &top]);
        let out = jj
            .cmd(&j)
            .args(["git", "push", "--bookmark", &bookmark])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{version}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = c.stack(n, 3);
        assert_eq!(conflicted(&s), [false, false, false], "{version}: {s}");
        assert_eq!(s["merge_blocked"], false, "{version}");
    }
}
