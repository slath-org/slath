// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Suggestions (REVIEW-09): applying one or several makes the specified
//! commit on the source branch, through compare-and-swap; outdated,
//! overlapping, and secret-bearing suggestions are refused with a reason.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::fork_merge_requests::{api, eventually, token};
use crate::jj::{Jj, jj_binaries};

pub(crate) fn write(git: &GitClient, w: &Path, file: &str, text: &str, msg: &str) -> String {
    std::fs::write(w.join(file), text).unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", msg]);
    git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned()
}

pub(crate) fn mr(s: &TestServer, t: &str, n: i64) -> Value {
    api(
        s,
        t,
        "GET",
        &format!("/alice/demo/merge-requests/{n}"),
        None,
    )
    .json()
}

/// A line comment with `body` on `path` (new side); its comment's ID.
pub(crate) fn suggest(
    s: &TestServer,
    t: &str,
    n: i64,
    path: &str,
    lines: (i32, i32),
    body: &str,
) -> String {
    let r = api(
        s,
        t,
        "POST",
        &format!("/alice/demo/merge-requests/{n}/threads"),
        Some(
            json!({"body": body, "path": path, "side": "new", "line_start": lines.0, "line_end": lines.1}),
        ),
    );
    assert!(r.status < 300, "{}", r.body);
    r.json()["comments"][0]["id"].as_str().unwrap().to_owned()
}

pub(crate) fn apply(s: &TestServer, t: &str, n: i64, body: Value) -> slath_tests::http::Resp {
    api(
        s,
        t,
        "POST",
        &format!("/alice/demo/merge-requests/{n}/suggestions/apply"),
        Some(body),
    )
}

#[test]
fn conf_review_09_suggestions_apply_as_a_commit_with_author_and_trailer() {
    let s = TestServer::start();
    for u in ["bob", "carol"] {
        s.create_user(u, PASSWORD, false);
    }
    let alice = token(&s, "alice");
    let (bob, carol) = (token(&s, "bob"), token(&s, "carol"));
    for u in ["bob", "carol"] {
        let r = api(
            &s,
            &alice,
            "PUT",
            &format!("/alice/demo/collaborators/{u}"),
            Some(json!({"role": "write"})),
        );
        assert!(r.status < 300, "{}", r.body);
    }
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    let url = s.url(&DEMO);
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    write(&git, &w, "README", "demo\n", "init");
    git.ok(&w, &["push", "-q", &url, "main"]);
    git.ok(&w, &["switch", "-q", "-c", "topic"]);
    write(&git, &w, "a.txt", "one\ntwo\nthree\nfour\nfive", "Add a");
    write(&git, &w, "b.txt", "alpha\nbeta\n", "Add b");
    git.ok(&w, &["push", "-q", &url, "topic"]);
    let r = api(
        &s,
        &alice,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Topic"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    eventually(&s, "no first revision", || {
        mr(&s, &alice, n)["revisions"] == 1
    });
    let head = mr(&s, &alice, n)["head_oid"].as_str().unwrap().to_owned();

    // Two suggestions by carol (one at the end of a file without a final
    // newline), one comment without a suggestion.
    let s1 = suggest(
        &s,
        &carol,
        n,
        "a.txt",
        (2, 3),
        "Shout:\n```suggestion\nTWO\nTHREE\n```",
    );
    let s2 = suggest(&s, &carol, n, "a.txt", (5, 5), "```suggestion\nFIVE\n```");
    let plain = suggest(&s, &carol, n, "b.txt", (1, 1), "Nice.");
    let r = apply(&s, &alice, n, json!({"comments": [plain]}));
    assert_eq!(r.status, 422, "{}", r.body);
    let r = apply(
        &s,
        &alice,
        n,
        json!({"comments": [s1], "expected_head_oid": "0".repeat(40)}),
    );
    assert_eq!(r.status, 409, "{}", r.body);
    let r = apply(
        &s,
        &alice,
        n,
        json!({"comments": [s1, s2], "expected_head_oid": head}),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["revisions"], 2);
    let repo = s.repo_path(&DEMO);
    let show = |what: &str| git.ok(&repo, &["show", "-s", &format!("--format={what}"), "topic"]);
    assert_eq!(
        git.ok(&repo, &["show", "topic:a.txt"]),
        "one\nTWO\nTHREE\nfour\nFIVE"
    );
    assert_eq!(git.ok(&repo, &["rev-parse", "topic^"]).trim(), head);
    assert!(
        show("%an <%ae>").starts_with("carol <carol@users.noreply."),
        "{}",
        show("%an <%ae>")
    );
    assert!(show("%cn <%ce>").starts_with("alice <alice@users.noreply."));
    let message = show("%B");
    assert!(
        message.starts_with("Apply 2 suggestions from review\n"),
        "{message}"
    );
    assert!(
        message.contains("\nCo-authored-by: carol <carol@users.noreply."),
        "{message}"
    );
    // Their threads are resolved; the plain comment's isn't.
    let threads = api(
        &s,
        &alice,
        "GET",
        &format!("/alice/demo/merge-requests/{n}/threads"),
        None,
    )
    .json();
    let resolved: Vec<bool> = threads
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["resolved"].as_bool().unwrap())
        .collect();
    assert_eq!(resolved.iter().filter(|r| **r).count(), 2, "{threads}");

    // Suggestions by two people: the applier is the author, both co-author.
    let s3 = suggest(&s, &carol, n, "b.txt", (1, 1), "```suggestion\nALPHA\n```");
    let s4 = suggest(&s, &bob, n, "b.txt", (2, 2), "```suggestion\nBETA\n```");
    let r = apply(&s, &alice, n, json!({"comments": [s3, s4]}));
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(show("%an").starts_with("alice"));
    let message = show("%B");
    assert!(
        message.contains("Co-authored-by: carol") && message.contains("Co-authored-by: bob"),
        "{message}"
    );

    // Overlapping suggestions, and one whose lines changed since.
    let s5 = suggest(&s, &carol, n, "b.txt", (1, 2), "```suggestion\nab\n```");
    let s6 = suggest(&s, &bob, n, "b.txt", (2, 2), "```suggestion\nb\n```");
    let r = apply(&s, &alice, n, json!({"comments": [s5, s6]}));
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("same lines"), "{}", r.body);
    git.ok(&w, &["fetch", "-q", &url, "topic"]);
    git.ok(&w, &["reset", "-q", "--hard", "FETCH_HEAD"]);
    write(&git, &w, "b.txt", "changed\nBETA\n", "Change b");
    git.ok(&w, &["push", "-q", &url, "topic"]);
    eventually(&s, "no fourth revision", || {
        mr(&s, &alice, n)["revisions"] == 4
    });
    let r = apply(&s, &alice, n, json!({"comments": [s5]}));
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("outdated"), "{}", r.body);

    // Push protection looks at what a suggestion adds.
    s.db.execute("UPDATE repositories SET push_protection = 'block'");
    let (_, secret) = slath_core::secret_scan::samples()
        .into_iter()
        .find(|(r, _)| *r == "github-token")
        .unwrap();
    let s7 = suggest(
        &s,
        &carol,
        n,
        "b.txt",
        (2, 2),
        &format!("```suggestion\ntoken = {secret}\n```"),
    );
    let r = apply(&s, &alice, n, json!({"comments": [s7]}));
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.body.contains("looks like a secret") && !r.body.contains(&secret),
        "{}",
        r.body
    );
    git.ok(&repo, &["fsck", "--strict", "--no-dangling"]);
}

fn change_ids(s: &TestServer, t: &str, n: i64) -> Vec<String> {
    api(
        s,
        t,
        "GET",
        &format!("/alice/demo/merge-requests/{n}/stack"),
        None,
    )
    .json()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["change_id"].as_str().unwrap().to_owned())
        .collect()
}

fn open_mr(s: &TestServer, t: &str, branch: &str) -> (i64, String) {
    let r = api(
        s,
        t,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_branch": branch, "title": branch})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    eventually(s, "no first revision", || mr(s, t, n)["revisions"] == 1);
    (n, mr(s, t, n)["head_oid"].as_str().unwrap().to_owned())
}

#[test]
fn conf_review_09_folded_suggestions_keep_every_change_for_git_and_jj() {
    let s = TestServer::start();
    s.create_user("carol", PASSWORD, false);
    let alice = token(&s, "alice");
    let carol = token(&s, "carol");
    let r = api(
        &s,
        &alice,
        "PUT",
        "/alice/demo/collaborators/carol",
        Some(json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    let url = s.url(&DEMO);
    let repo = s.repo_path(&DEMO);
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    write(&git, &w, "README", "demo\n", "init");
    git.ok(&w, &["push", "-q", &url, "main"]);
    // A Git-made stack of two commits, on two branches.
    git.ok(&w, &["switch", "-q", "-c", "fold"]);
    write(&git, &w, "a.txt", "one\ntwo\nthree\n", "Add a");
    std::fs::write(w.join("b.txt"), "bee\n").unwrap();
    write(
        &git,
        &w,
        "a.txt",
        "one\ntwo\nthree\nfour\n",
        "Add b, extend a",
    );
    git.ok(&w, &["push", "-q", &url, "fold", "fold:on-top"]);
    let (fold, head) = open_mr(&s, &alice, "fold");
    let (top, _) = open_mr(&s, &alice, "on-top");
    let ids = change_ids(&s, &alice, fold);
    let suggestion = "```suggestion\nTWO\n```";

    // Lines from both commits can't be folded.
    let both = suggest(&s, &carol, fold, "a.txt", (3, 4), "```suggestion\nx\n```");
    let r = apply(
        &s,
        &alice,
        fold,
        json!({"comments": [both], "mode": "fold"}),
    );
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("don't come from one change"), "{}", r.body);

    // Folded: "Add a" gets the fix, "Add b" is rebased, IDs stay.
    let c = suggest(&s, &carol, fold, "a.txt", (2, 2), suggestion);
    let r = apply(
        &s,
        &alice,
        fold,
        json!({"comments": [c], "mode": "fold", "expected_head_oid": head}),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    eventually(&s, "no second revision", || {
        mr(&s, &alice, fold)["revisions"] == 2
    });
    assert_eq!(
        change_ids(&s, &alice, fold),
        ids,
        "every change keeps its ID"
    );
    let show = |rev: &str, what: &str| {
        git.ok(&repo, &["show", "-s", &format!("--format={what}"), rev])
            .trim()
            .to_owned()
    };
    assert_eq!(
        git.ok(&repo, &["show", "fold~1:a.txt"]),
        "one\nTWO\nthree\n"
    );
    assert_eq!(show("fold~1", "%s"), "Add a");
    assert!(show("fold~1", "%B").contains("\n\nCo-authored-by: carol <carol@users.noreply."));
    assert!(show("fold~1", "%cn").starts_with("alice"));
    assert_eq!(
        show("fold~1", "%an"),
        git.ok(&w, &["show", "-s", "--format=%an", "HEAD~1"]).trim()
    );
    assert_eq!(show("fold", "%s"), "Add b, extend a");
    assert_eq!(
        git.ok(&repo, &["rev-parse", "fold~2"]),
        git.ok(&repo, &["rev-parse", "main"])
    );
    // The same suggestion as a commit on top gives the same tree.
    let c = suggest(&s, &carol, top, "a.txt", (2, 2), suggestion);
    let r = apply(&s, &alice, top, json!({"comments": [c]}));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        git.ok(&repo, &["rev-parse", "fold^{tree}"]),
        git.ok(&repo, &["rev-parse", "on-top^{tree}"])
    );

    // A branch that blocks force pushes takes a new commit, not a fold.
    let r = api(
        &s,
        &alice,
        "POST",
        "/alice/demo/protections",
        Some(json!({"pattern": "fold", "block_force_push": true, "push_role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let c = suggest(&s, &carol, fold, "a.txt", (1, 1), "```suggestion\nONE\n```");
    let r = apply(&s, &alice, fold, json!({"comments": [c], "mode": "fold"}));
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("force pushes"), "{}", r.body);

    // A jj-made stack: jj sees the same changes after fetching.
    for (version, bin) in jj_binaries() {
        let v = version.replace(['.', ' '], "-");
        let jj = Jj::new(&version, &bin);
        let j = d.path().join(format!("jj-{v}"));
        jj.ok(d.path(), &["git", "clone", &url, j.to_str().unwrap()]);
        let topic = format!("jj-fold-{v}");
        jj.ok(&j, &["new", "main@origin", "-m", "Add j"]);
        std::fs::write(j.join(format!("{topic}.txt")), "jay\nkay\n").unwrap();
        jj.ok(&j, &["new", "-m", "Add k"]);
        std::fs::write(j.join(format!("{topic}-k.txt")), "kay\n").unwrap();
        jj.ok(&j, &["bookmark", "create", &topic, "-r", "@"]);
        jj.ok(&j, &["git", "push", "--bookmark", &topic]);
        let first = jj.show(&j, &format!("{topic}-"), "change_id");
        let second = jj.show(&j, &topic, "change_id");
        let (n, head) = open_mr(&s, &alice, &topic);
        let c = suggest(
            &s,
            &carol,
            n,
            &format!("{topic}.txt"),
            (1, 1),
            "```suggestion\nJAY\n```",
        );
        let r = apply(
            &s,
            &alice,
            n,
            json!({"comments": [c], "mode": "fold", "expected_head_oid": head}),
        );
        assert_eq!(r.status, 200, "{version}: {}", r.body);
        jj.ok(&j, &["git", "fetch"]);
        let rev = format!("{topic}@origin");
        assert_eq!(jj.show(&j, &rev, "change_id"), second, "{version}");
        assert_eq!(
            jj.show(&j, &format!("{rev}-"), "change_id"),
            first,
            "{version}"
        );
        assert_eq!(
            jj.ok(
                &j,
                &[
                    "file",
                    "show",
                    "-r",
                    &format!("{rev}-"),
                    &format!("{topic}.txt")
                ]
            ),
            "JAY\nkay\n",
            "{version}"
        );
    }
    git.ok(&repo, &["fsck", "--strict", "--no-dangling"]);
}

#[test]
fn conf_review_09_suggestions_in_the_ui() {
    let s = TestServer::start();
    s.create_user("carol", PASSWORD, false);
    let alice = token(&s, "alice");
    let carol = token(&s, "carol");
    let r = api(
        &s,
        &alice,
        "PUT",
        "/alice/demo/collaborators/carol",
        Some(json!({"role": "read"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    let url = s.url(&DEMO);
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    write(&git, &w, "README", "demo\n", "init");
    git.ok(&w, &["push", "-q", &url, "main"]);
    git.ok(&w, &["switch", "-q", "-c", "ui"]);
    write(&git, &w, "a.txt", "one\ntwo\nthree\nfour\n", "Add a");
    git.ok(&w, &["push", "-q", &url, "ui"]);
    let (n, _) = open_mr(&s, &alice, "ui");
    let page = format!("/alice/demo/merge-requests/{n}");
    let first = suggest(&s, &carol, n, "a.txt", (1, 1), "```suggestion\nONE\n```");
    let second = suggest(&s, &carol, n, "a.txt", (3, 3), "```suggestion\nTHREE\n```");
    let third = suggest(&s, &carol, n, "a.txt", (4, 4), "```suggestion\nFOUR\n```");

    // A reader sees the suggestion, not the actions.
    let body = s.login("carol", PASSWORD).get(&page).body;
    assert!(body.contains("Suggested change"), "{body}");
    assert!(!body.contains("id=\"apply-suggestions\""), "{body}");
    // The author applies one with its own button, then two as a folded batch.
    let mut web = s.login("alice", PASSWORD);
    let body = web.get(&page).body;
    assert!(
        body.contains(&format!("name=\"only\" value=\"{first}\"")),
        "{body}"
    );
    let r = slath_tests::http::submit(
        &mut web,
        &page,
        &format!("{page}/suggestions/apply"),
        &[("only", &first), ("mode", "commit")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(
        web.get(&format!("{page}?status=applied"))
            .body
            .contains("Suggestions applied.")
    );
    let r = slath_tests::http::submit(
        &mut web,
        &page,
        &format!("{page}/suggestions/apply"),
        &[
            ("comments", &second),
            ("comments", &third),
            ("mode", "fold"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let repo = s.repo_path(&DEMO);
    assert_eq!(
        git.ok(&repo, &["show", "ui:a.txt"]),
        "ONE\ntwo\nTHREE\nFOUR\n"
    );
    assert_eq!(
        git.ok(&repo, &["show", "ui~1:a.txt"]),
        "one\ntwo\nTHREE\nFOUR\n"
    );
    // Applied suggestions are resolved and can't be applied again; a
    // suggestion on changed lines says it's outdated.
    let late = suggest(&s, &carol, n, "a.txt", (2, 2), "```suggestion\nTWO\n```");
    git.ok(&w, &["fetch", "-q", &url, "ui"]);
    git.ok(&w, &["reset", "-q", "--hard", "FETCH_HEAD"]);
    write(&git, &w, "a.txt", "ONE\n2\nTHREE\nFOUR\n", "Number two");
    git.ok(&w, &["push", "-q", &url, "ui"]);
    eventually(&s, "no fourth revision", || {
        mr(&s, &alice, n)["revisions"] == 4
    });
    let body = web.get(&page).body;
    assert!(body.contains("Outdated: its lines changed since"), "{body}");
    assert!(!body.contains(&format!("value=\"{late}\"")), "{body}");
}
