// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Starts a `slath serve` with a fresh database and data directory for the
//! browser tests, seeds test accounts and a repository, prints
//! `SLATH_E2E_URL=<base url>`, and runs until stdin closes (the Playwright
//! launcher holds the pipe open).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};

use std::path::Path;

use slath_tests::server::{DEMO, GitClient, Instance, PASSWORD, TestServer};

/// The Playwright projects (tests/e2e/playwright.config.ts): each gets its
/// own branch to open a merge request from, so parallel runs don't collide.
const PROJECTS: [&str; 12] = [
    "chromium-light-desktop",
    "chromium-dark-desktop",
    "chromium-light-touch",
    "chromium-dark-touch",
    "firefox-light-desktop",
    "firefox-dark-desktop",
    "firefox-light-touch",
    "firefox-dark-touch",
    "webkit-light-desktop",
    "webkit-dark-desktop",
    "webkit-light-touch",
    "webkit-dark-touch",
];

/// Branch `diffs`: a commit with 26 changed files (more than load with the
/// page), a generated one, and one that only changes indentation.
fn seed_diffs(git: &GitClient, w: &Path, url: &str) {
    git.ok(w, &["switch", "-q", "-c", "diffs", "main"]);
    std::fs::write(w.join("ws.txt"), "alpha\nbeta\n").unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Add ws.txt"]);
    std::fs::write(w.join("ws.txt"), "  alpha\n  beta\n").unwrap();
    std::fs::write(w.join(".gitattributes"), "gen.txt linguist-generated\n").unwrap();
    std::fs::write(w.join("gen.txt"), "generated output\n").unwrap();
    for i in 0..23 {
        std::fs::write(w.join(format!("f{i:02}.txt")), format!("file {i}\n")).unwrap();
    }
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Change many files"]);
    git.ok(w, &["push", "-q", url, "diffs"]);
    git.ok(w, &["switch", "-q", "main"]);
}

/// Branches `e2e/<project>` to open merge requests from, the merge request
/// "Review fixture" from `review` with two revisions, and `pushed`, which
/// no test opens a merge request from (the "Recently pushed" banner).
fn seed_merge_requests(server: &TestServer, git: &GitClient, w: &Path, url: &str) {
    for p in PROJECTS {
        git.ok(w, &["switch", "-q", "-c", &format!("e2e/{p}"), "main"]);
        std::fs::write(w.join(format!("{p}.md")), format!("# Notes for {p}\n")).unwrap();
        git.ok(w, &["add", "-A"]);
        git.ok(w, &["commit", "-q", "-m", &format!("Add notes for {p}")]);
        git.ok(w, &["push", "-q", url, &format!("e2e/{p}")]);
    }
    git.ok(w, &["switch", "-q", "-c", "review", "main"]);
    std::fs::write(w.join("review.md"), "First draft.\n").unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Add review notes"]);
    git.ok(w, &["push", "-q", url, "review"]);
    let mut alice = server.login("alice", PASSWORD);
    let token = slath_tests::http::create_token(&mut alice, "e2e", &["repo:write", "mrs:write"]);
    let mut api = server.client();
    let r = api.api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&token),
        Some(&serde_json::json!({
            "source_branch": "review",
            "title": "Review fixture",
            "description": "Two revisions for the timeline tests.",
        })),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    std::fs::write(w.join("review.md"), "Second draft.\n").unwrap();
    git.ok(w, &["commit", "-qam", "Revise review notes"]);
    git.ok(w, &["push", "-q", url, "review"]);
    // The second revision is recorded by a background job.
    let path = format!("/api/v1/repos/alice/demo/merge-requests/{n}");
    let start = std::time::Instant::now();
    while api.api("GET", &path, Some(&token), None).json()["revisions"] != 2 {
        assert!(
            start.elapsed().as_secs() < 60,
            "no second revision\n{}",
            server.log()
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    // Pushed last, so the "Recently pushed" banner always shows it.
    git.ok(w, &["switch", "-q", "-c", "pushed", "main"]);
    std::fs::write(w.join("pushed.md"), "Pushed last.\n").unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Add a pushed note"]);
    git.ok(w, &["push", "-q", url, "pushed"]);
    git.ok(w, &["switch", "-q", "main"]);
}

fn main() {
    // alice/demo (private) with content; accounts as in tests/e2e/fixtures.ts.
    // A host name, not an IP address: passkeys (WebAuthn) need one.
    let inst = Instance::new("");
    let origin = inst.use_localhost();
    let mut server = inst.start(&[]);
    server.seed_repos(&[DEMO]);
    server.create_user("sysadmin", "e2e admin password", true);
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let w = work.path();
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(w.join("src")).unwrap();
    std::fs::write(
        w.join("README.md"),
        "# Demo\n\nA repository for browser tests.\n\n<div hx-get=\"/-/logout\" hx-trigger=\"load\">injected</div>\n\n- [x] item\n\n```rust\nfn main() {}\n```\n",
    )
    .unwrap();
    std::fs::write(
        w.join("src/main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Initial commit"]);
    std::fs::write(
        w.join("src/main.rs"),
        "fn main() {\n    println!(\"hello, world\");\n}\n",
    )
    .unwrap();
    git.ok(w, &["commit", "-qam", "Greet the world"]);
    let url = server.url(&DEMO);
    git.ok(w, &["push", "-q", &url, "main"]);
    seed_merge_requests(&server, &git, w, &url);
    seed_diffs(&git, w, &url);
    // The browser tests sign in as alice with this password.
    assert_eq!(PASSWORD, "slath test password");
    println!("SLATH_E2E_URL={origin}");
    std::io::stdout().flush().unwrap();
    let mut sink = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut sink);
    drop(server);
}
