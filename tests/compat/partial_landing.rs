// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Partial landing (REVIEW-08): merging a stack's changes up to one of
//! them lands exactly that prefix, with every strategy; the merge request
//! stays open with the rest as a new revision based on the new target, and
//! the rest keeps its change IDs, for Git-made and jj-made stacks.

use std::path::Path;

use serde_json::{Value, json};

use crate::change_identity::{Ctx, commit_file, ids, seed};
use crate::jj::{Jj, jj_binaries};

fn merge(c: &Ctx, n: i64, body: Value) -> slath_tests::http::Resp {
    c.api("POST", &format!("/merge-requests/{n}/merge"), Some(&body))
}

fn head(c: &Ctx, n: i64) -> String {
    c.api("GET", &format!("/merge-requests/{n}"), None).json()["head_oid"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn state(c: &Ctx, n: i64) -> String {
    c.api("GET", &format!("/merge-requests/{n}"), None).json()["state"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The files of the server's `main`.
fn main_files(c: &Ctx) -> Vec<String> {
    c.git
        .ok(
            &c.s.repo_path(&slath_tests::server::DEMO),
            &["ls-tree", "--name-only", "main"],
        )
        .lines()
        .map(str::to_owned)
        .collect()
}

fn main_head(c: &Ctx) -> String {
    c.git
        .ok(
            &c.s.repo_path(&slath_tests::server::DEMO),
            &["rev-parse", "main"],
        )
        .trim()
        .to_owned()
}

/// A Git stack of three changes on `branch`, each adding `<branch>-<x>.txt`.
fn git_stack(c: &Ctx, w: &Path, branch: &str) -> i64 {
    c.git.ok(w, &["switch", "-q", "-c", branch, "origin/main"]);
    for x in ["a", "b", "c"] {
        commit_file(
            &c.git,
            w,
            &format!("{branch}-{x}.txt"),
            &format!("{x}\n"),
            &format!("Add {x}"),
        );
    }
    c.push(w, branch);
    let n = c.open_mr(branch);
    c.stack(n, 1);
    n
}

#[test]
fn conf_review_08_exactly_the_prefix_lands_with_each_strategy() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    seed(&c, d.path());
    let w = d.path().join("w");
    c.clone(&w);
    for strategy in ["fast-forward", "rebase", "merge-commit", "squash"] {
        c.git.ok(&w, &["fetch", "-q", "origin"]);
        let n = git_stack(&c, &w, strategy);
        let before = ids(&c.stack(n, 1));
        // The requirements of a prefix are its own.
        let m = c
            .api(
                "GET",
                &format!("/merge-requests/{n}/mergeability?upto_change={}", before[1]),
                None,
            )
            .json();
        assert_eq!(m["mergeable"], true, "{strategy}: {m}");
        let r = merge(
            &c,
            n,
            json!({"strategy": strategy, "expected_head_oid": head(&c, n), "upto_change": &before[1][..8]}),
        );
        assert_eq!(r.status, 200, "{strategy}: {}", r.body);
        let files = main_files(&c);
        for (x, landed) in [("a", true), ("b", true), ("c", false)] {
            assert_eq!(
                files.contains(&format!("{strategy}-{x}.txt")),
                landed,
                "{strategy}: {x} in {files:?}"
            );
        }
        // Still open, with the rest as a new revision on the new target
        // that keeps its change ID.
        assert_eq!(state(&c, n), "open", "{strategy}");
        let s = c.stack(n, 2);
        assert_eq!(ids(&s), vec![before[2].clone()], "{strategy}: {s}");
        let base = c.api("GET", &format!("/merge-requests/{n}/revisions"), None);
        let base = &base.json()[1];
        assert_eq!(
            base["base_oid"],
            main_head(&c).as_str(),
            "{strategy}: {base}"
        );
        // The timeline names what landed.
        let mut ui = c.s.login("alice", slath_tests::server::PASSWORD);
        let r = ui.get(&format!("/alice/demo/merge-requests/{n}"));
        assert!(
            r.body.contains(&format!(
                "landed changes {}, {} into main",
                &before[0][..8],
                &before[1][..8]
            )),
            "{strategy}: {}",
            r.body
        );
        // The rest merges as usual, and then the merge request is merged.
        let r = merge(
            &c,
            n,
            json!({"strategy": strategy, "expected_head_oid": head(&c, n)}),
        );
        assert_eq!(r.status, 200, "{strategy}: {}", r.body);
        assert_eq!(state(&c, n), "merged", "{strategy}");
        assert!(main_files(&c).contains(&format!("{strategy}-c.txt")));
    }
    // Landing the rest added no copies of what landed before: Add a is
    // on main once for each strategy but squash.
    let log = c.git.ok(
        &c.s.repo_path(&slath_tests::server::DEMO),
        &["log", "--format=%s", "main"],
    );
    assert_eq!(log.lines().filter(|l| *l == "Add a").count(), 3, "{log}");
}

#[test]
fn conf_review_08_unknown_changes_and_the_top_change() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    seed(&c, d.path());
    let w = d.path().join("w");
    c.clone(&w);
    let n = git_stack(&c, &w, "topic");
    let all = ids(&c.stack(n, 1));
    for bad in ["zzzzzzzzzzzzzzzz", &all[0][..4]] {
        let r = merge(
            &c,
            n,
            json!({"strategy": "rebase", "expected_head_oid": head(&c, n), "upto_change": bad}),
        );
        assert_eq!(r.status, 422, "{bad}: {}", r.body);
        assert!(r.body.contains("upto_change"), "{}", r.body);
        let r = c.api(
            "GET",
            &format!("/merge-requests/{n}/mergeability?upto_change={bad}"),
            None,
        );
        assert_eq!(r.status, 422, "{}", r.body);
    }
    // Up to the top change is the whole merge request.
    let r = merge(
        &c,
        n,
        json!({"strategy": "rebase", "expected_head_oid": head(&c, n), "upto_change": all[2]}),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(state(&c, n), "merged");
}

#[test]
fn conf_review_08_partial_landing_of_jj_stacks_continues_their_ids() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    seed(&c, d.path());
    for (version, bin) in jj_binaries() {
        let v = version.replace(['.', ' '], "_");
        let jj = Jj::new(&version, &bin);
        let j = d.path().join(format!("jj-{v}"));
        jj.ok(d.path(), &["git", "clone", &c.url, j.to_str().unwrap()]);
        let bookmark = format!("land-{v}");
        let mut made = Vec::new();
        for (i, x) in ["a", "b", "c"].iter().enumerate() {
            let parent = if i == 0 { "main@origin" } else { "@" };
            jj.ok(&j, &["new", parent, "-m", &format!("Add {x}")]);
            std::fs::write(j.join(format!("{v}-{x}.txt")), format!("{x}\n")).unwrap();
            made.push(jj.show(&j, "@", "change_id"));
        }
        jj.ok(&j, &["bookmark", "create", &bookmark, "-r", "@"]);
        jj.ok(&j, &["git", "push", "--bookmark", &bookmark]);
        let n = c.open_mr(&bookmark);
        assert_eq!(ids(&c.stack(n, 1)), made, "{version}");
        let r = merge(
            &c,
            n,
            json!({"strategy": "rebase", "expected_head_oid": head(&c, n), "upto_change": made[1]}),
        );
        assert_eq!(r.status, 200, "{version}: {}", r.body);
        assert_eq!(ids(&c.stack(n, 2)), vec![made[2].clone()], "{version}");
        // jj sees the landed changes on main and the rest on top of it,
        // all with the same IDs.
        jj.ok(&j, &["git", "fetch"]);
        let on_main = jj.ok(
            &j,
            &[
                "log",
                "--no-graph",
                "-r",
                "::main@origin ~ root()",
                "-T",
                "change_id ++ \"\\n\"",
            ],
        );
        for id in &made[..2] {
            assert!(on_main.contains(id.as_str()), "{version}: {on_main}");
        }
        assert!(!on_main.contains(made[2].as_str()), "{version}");
        let parent = jj.show(&j, &format!("{bookmark}@origin-"), "change_id");
        assert_eq!(parent, made[1], "{version}");
        assert_eq!(
            jj.show(&j, &format!("{bookmark}@origin"), "change_id"),
            made[2],
            "{version}"
        );
    }
}
