// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Starts a `slath serve` with a fresh database and data directory for the
//! browser tests, seeds test accounts, prints `SLATH_E2E_URL=<base url>`, and runs until stdin
//! closes (the Playwright launcher holds the pipe open).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};

fn main() {
    let server = slath_tests::server::TestServer::start_with(&[]);
    // Accounts the browser tests sign in with (passwords in tests/e2e/fixtures.ts).
    server.create_user("sysadmin", "e2e admin password", true);
    server.create_user("alice", "e2e alice password", false);
    println!("SLATH_E2E_URL={}", server.base());
    std::io::stdout().flush().unwrap();
    let mut sink = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut sink);
    drop(server);
}
