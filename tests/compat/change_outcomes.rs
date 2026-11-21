// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! What each revision does to a stack's changes, with real jj and Git
//! clients (JJ-06): squashed, split, abandoned, returned, and reordered
//! changes get explicit outcomes, identities are never merged, and a thread
//! on a removed change's lines is never shown on other code.

use std::path::Path;

use serde_json::{Value, json};

use crate::change_identity::{Ctx, commit_file, entries, feedback, ids, lines, push_output, seed};
use crate::jj::{Jj, jj_binaries};

fn outcomes(stack: &Value) -> Vec<String> {
    entries(stack)
        .iter()
        .map(|e| e["outcome"].as_str().unwrap().to_owned())
        .collect()
}

fn removed(stack: &Value) -> Vec<(String, i64)> {
    stack["removed_changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["change_id"].as_str().unwrap().to_owned(),
                r["removed_in"].as_i64().unwrap(),
            )
        })
        .collect()
}

fn ids_of(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn new_thread(c: &Ctx, n: i64, path: &str, line: i64) -> String {
    let r = c.api(
        "POST",
        &format!("/merge-requests/{n}/threads"),
        Some(&json!({"body": "Look here", "path": path, "line_start": line})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    r.json()["id"].as_str().unwrap().to_owned()
}

/// Where thread `id` is shown now: `current`, `outdated`, or `general`.
fn thread_state(c: &Ctx, n: i64, id: &str) -> String {
    let list = c
        .api("GET", &format!("/merge-requests/{n}/threads"), None)
        .json();
    let t = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap_or_else(|| panic!("{id} not in {list}"));
    t["position"]["state"].as_str().unwrap().to_owned()
}

fn jj_push(jj: &Jj, dir: &Path, bookmark: &str) -> Vec<String> {
    let out = jj
        .cmd(dir)
        .args(["git", "push", "--bookmark", bookmark])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{err}");
    feedback(&err)
}

fn short(id: &str) -> &str {
    &id[..8]
}

#[test]
fn conf_jj_06_squash_split_abandon_return_and_reorder_in_jj() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    seed(&c, d.path());
    for (version, bin) in jj_binaries() {
        let v = version.replace(['.', ' '], "_");
        let jj = Jj::new(&version, &bin);
        let j = d.path().join(format!("jj-{v}"));
        jj.ok(d.path(), &["git", "clone", &c.url, j.to_str().unwrap()]);
        let bookmark = format!("outcomes-{v}");
        // Five changes; the third touches two files, for splitting.
        let mut made = Vec::new();
        for (i, name) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            let parent = if i == 0 { "main@origin" } else { "@" };
            jj.ok(&j, &["new", parent, "-m", &format!("Add {name}")]);
            std::fs::write(j.join(format!("{name}.txt")), lines(name, None)).unwrap();
            if *name == "c" {
                std::fs::write(j.join("c2.txt"), lines("c2", None)).unwrap();
            }
            made.push(jj.show(&j, "@", "change_id"));
        }
        let [a, b, cc, dd, e] = [0, 1, 2, 3, 4].map(|i| made[i].as_str());
        jj.ok(&j, &["bookmark", "create", &bookmark, "-r", "@"]);
        jj_push(&jj, &j, &bookmark);
        let n = c.open_mr(&bookmark);
        let first = c.stack(n, 1);
        assert_eq!(ids(&first), made, "{version}");
        assert_eq!(outcomes(&first), vec!["new"; 5], "{version}");
        let on_d = new_thread(&c, n, "d.txt", 3);

        // Squash b into a: a is updated, b is removed and stays its own
        // change.
        jj.ok(&j, &["squash", "-r", b, "-m", "Add a"]);
        let out = jj_push(&jj, &j, &bookmark);
        let s = c.stack(n, 2);
        assert_eq!(ids(&s), ids_of(&[a, cc, dd, e]), "{version}");
        assert_eq!(
            outcomes(&s),
            ["updated", "unchanged", "unchanged", "unchanged"],
            "{version}"
        );
        assert_eq!(removed(&s), vec![(b.to_owned(), 2)], "{version}");
        assert!(
            out.contains(&format!("{} removed", short(b))),
            "{version}: {out:?}"
        );

        // Split c: one part continues c, the other is a new change.
        jj.ok(&j, &["split", "-r", cc, "c2.txt", "-m", "Add c2"]);
        jj_push(&jj, &j, &bookmark);
        let s = c.stack(n, 3);
        let got = ids(&s);
        assert_eq!(got.len(), 5, "{version}");
        assert_eq!(
            (got[0].as_str(), got[3].as_str(), got[4].as_str()),
            (a, dd, e),
            "{version}"
        );
        let (kept, fresh) = if got[1] == cc { (1, 2) } else { (2, 1) };
        assert_eq!(got[kept], cc, "{version}: one part keeps c's ID");
        assert!(
            !made.contains(&got[fresh]),
            "{version}: the other part is new"
        );
        let o = outcomes(&s);
        assert_eq!(
            (o[0].as_str(), o[kept].as_str(), o[fresh].as_str()),
            ("unchanged", "updated", "new"),
            "{version}: {o:?}"
        );
        let c2 = got[fresh].clone();

        // Abandon d: removed in revision 4, and its thread is outdated.
        let d_commit = jj.show(&j, dd, "commit_id");
        jj.ok(&j, &["abandon", dd]);
        jj_push(&jj, &j, &bookmark);
        let s = c.stack(n, 4);
        assert!(!ids(&s).contains(&dd.to_owned()), "{version}");
        assert_eq!(
            removed(&s),
            vec![(b.to_owned(), 2), (dd.to_owned(), 4)],
            "{version}"
        );
        assert_eq!(thread_state(&c, n, &on_d), "outdated", "{version}");

        // Bring d back: it returns, the same change with a new version.
        jj.ok(&j, &["rebase", "-s", e, "-d", &d_commit]);
        let out = jj_push(&jj, &j, &bookmark);
        let s = c.stack(n, 5);
        assert_eq!(ids(&s)[3], dd, "{version}");
        assert_eq!(entries(&s)[3]["outcome"], "returned", "{version}");
        assert_eq!(entries(&s)[3]["removed_in"], 4, "{version}");
        assert_eq!(entries(&s)[4]["outcome"], "unchanged", "{version}");
        assert_eq!(removed(&s), vec![(b.to_owned(), 2)], "{version}");
        assert!(
            out.contains(&format!("{} returned (version 4) Add d", short(dd))),
            "{version}: {out:?}"
        );
        assert_eq!(thread_state(&c, n, &on_d), "current", "{version}");

        // Reorder: a moves to the top. Every change keeps its ID and, with
        // its own files, its patch.
        jj.ok(&j, &["rebase", "-r", a, "--insert-after", e]);
        jj.ok(&j, &["bookmark", "set", &bookmark, "-r", a]);
        jj_push(&jj, &j, &bookmark);
        let s = c.stack(n, 6);
        let mut want: Vec<String> = got[1..].to_vec();
        want.push(a.to_owned());
        assert_eq!(ids(&s), want, "{version}");
        assert_eq!(outcomes(&s), vec!["unchanged"; 5], "{version}");
        assert!(want.contains(&c2), "{version}");
        assert_eq!(removed(&s), vec![(b.to_owned(), 2)], "{version}");
    }
}

#[test]
fn conf_jj_06_drop_return_and_reorder_in_git() {
    let c = Ctx::start();
    let d = tempfile::tempdir().unwrap();
    let w = seed(&c, d.path());
    c.git.ok(&w, &["switch", "-q", "-c", "plain", "main"]);
    let ca = commit_file(&c.git, &w, "a.txt", &lines("a", None), "Add a");
    let cb = commit_file(&c.git, &w, "b.txt", &lines("b", None), "Add b");
    let head = commit_file(&c.git, &w, "c.txt", &lines("c", None), "Add c");
    c.push(&w, "plain");
    let n = c.open_mr("plain");
    let first = c.stack(n, 1);
    let made = ids(&first);
    let on_b = new_thread(&c, n, "b.txt", 3);

    // Drop b: it's removed, the others keep their patches.
    c.git.ok(&w, &["reset", "-q", "--hard", &ca]);
    c.git.ok(&w, &["cherry-pick", &head]);
    c.push(&w, "plain");
    let s = c.stack(n, 2);
    assert_eq!(ids(&s), vec![made[0].clone(), made[2].clone()]);
    assert_eq!(outcomes(&s), ["unchanged", "unchanged"]);
    assert_eq!(removed(&s), vec![(made[1].clone(), 2)]);
    assert_eq!(thread_state(&c, n, &on_b), "outdated");

    // Unrelated code in b's file and lines: a new change, and b's thread
    // stays outdated instead of moving onto it.
    commit_file(&c.git, &w, "b.txt", &lines("z", None), "Add other b");
    c.push(&w, "plain");
    let s = c.stack(n, 3);
    assert_eq!(outcomes(&s), ["unchanged", "unchanged", "new"]);
    assert!(!made.contains(&ids(&s)[2]));
    assert_eq!(thread_state(&c, n, &on_b), "outdated");
    let other = ids(&s)[2].clone();

    // The original stack again: b returns with its thread.
    c.git.ok(&w, &["reset", "-q", "--hard", &head]);
    let out = push_output(&c, &w, "plain");
    let s = c.stack(n, 4);
    assert_eq!(ids(&s), made);
    assert_eq!(outcomes(&s), ["unchanged", "returned", "unchanged"]);
    assert_eq!(entries(&s)[1]["removed_in"], 2);
    assert_eq!(removed(&s), vec![(other, 4)]);
    assert!(
        out.contains(&format!("{} returned (version 2) Add b", short(&made[1]))),
        "{out:?}"
    );
    assert_eq!(thread_state(&c, n, &on_b), "current");

    // Reorder by cherry-picking: same changes, identical patches.
    c.git.ok(&w, &["reset", "-q", "--hard", "main"]);
    c.git.ok(&w, &["cherry-pick", &head, &cb, &ca]);
    c.push(&w, "plain");
    let s = c.stack(n, 5);
    assert_eq!(
        ids(&s),
        vec![made[2].clone(), made[1].clone(), made[0].clone()]
    );
    assert_eq!(outcomes(&s), vec!["unchanged"; 3]);
}
