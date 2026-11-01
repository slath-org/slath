// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Starts a `slath serve` with a fresh database and data directory for the
//! browser tests, seeds test accounts and a repository, prints
//! `SLATH_E2E_URL=<base url>`, and runs until stdin closes (the Playwright
//! launcher holds the pipe open).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};

use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

fn main() {
    // alice/demo (private) with content; accounts as in tests/e2e/fixtures.ts.
    let server = TestServer::start_with(&[DEMO]);
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
    git.ok(w, &["push", "-q", &server.url(&DEMO), "main"]);
    // The browser tests sign in as alice with this password.
    assert_eq!(PASSWORD, "slath test password");
    println!("SLATH_E2E_URL={}", server.base());
    std::io::stdout().flush().unwrap();
    let mut sink = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut sink);
    drop(server);
}
