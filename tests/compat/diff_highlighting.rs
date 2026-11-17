// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Syntax highlighting in diffs (A11Y-01, DESIGN-01): commit and merge
//! request diffs are highlighted like file views, from each side's whole
//! file, so a line inside a comment that starts above the hunk reads as a
//! comment; the character emphasis stays on top; files past
//! `[limits] diff_highlight_max_file_bytes` show plain with a note.

use serde_json::json;
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

fn commit(git: &GitClient, w: &std::path::Path, file: &str, text: &str, msg: &str) -> String {
    std::fs::write(w.join(file), text).unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", msg]);
    git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// A Rust file with a block comment opened long before the changed line.
fn source(value: u32) -> String {
    let mut s = String::from("/*\n");
    for i in 0..30 {
        s.push_str(&format!(" * line {i} of a long comment\n"));
    }
    s.push_str(&format!(
        " * the value is {value}\n */\nfn main() {{\n    let x = {value};\n}}\n"
    ));
    s
}

#[test]
fn conf_design_01_diffs_are_highlighted_from_whole_files() {
    let s = TestServer::start_full(&[DEMO], "[limits]\ndiff_highlight_max_file_bytes = 4096\n");
    let mut alice = s.login("alice", PASSWORD);
    let token = slath_tests::http::create_token(&mut alice, "t", &["repo:write", "mrs:write"]);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    let url = s.url(&DEMO);
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("big.txt"), "x\n".repeat(4000)).unwrap();
    commit(&git, &w, "main.rs", &source(1), "init");
    git.ok(&w, &["push", "-q", &url, "main"]);
    git.ok(&w, &["switch", "-q", "-c", "topic"]);
    std::fs::write(w.join("big.txt"), "x\n".repeat(3999) + "y\n").unwrap();
    let head = commit(&git, &w, "main.rs", &source(2), "Two");
    git.ok(&w, &["push", "-q", &url, "topic"]);

    for split in ["unified", "split"] {
        let page = alice
            .get(&format!("/alice/demo/commit/{head}?view={split}"))
            .body;
        // The changed comment line is a comment, though `/*` is 30 lines up.
        let line = page
            .split(" * the value is ")
            .next()
            .and_then(|before| before.rsplit("<code>").next())
            .unwrap_or_default();
        assert!(line.contains("syn-comment"), "{split}: {line}");
        // Code is highlighted, and the emphasis is on top of it.
        assert!(
            page.contains("syn-keyword") || page.contains("syn-storage"),
            "{split}"
        );
        assert!(
            page.contains("<ins><span class=\"visually-hidden\">changed: </span>"),
            "{split}"
        );
        assert!(
            page.contains("<del><span class=\"visually-hidden\">changed: </span>"),
            "{split}"
        );
        // Past the limit: plain, with a note.
        assert!(
            page.contains("Shown without highlighting: the file is larger than 4 KiB."),
            "{split}"
        );
    }

    // A merge request's changes, too.
    let r = alice.api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&token),
        Some(&json!({"source_branch": "topic", "title": "Two"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let start = std::time::Instant::now();
    loop {
        let m = alice
            .api(
                "GET",
                "/api/v1/repos/alice/demo/merge-requests/1",
                Some(&token),
                None,
            )
            .json();
        if m["revisions"] == 1 {
            break;
        }
        assert!(start.elapsed().as_secs() < 30, "no revision");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let changes = alice.get("/alice/demo/merge-requests/1/changes").body;
    assert!(changes.contains("syn-comment"), "MR changes");
    assert!(changes.contains("Shown without highlighting"), "MR changes");
}
