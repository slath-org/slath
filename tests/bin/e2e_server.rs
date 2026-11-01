// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Starts a `slath serve` with a fresh database and data directory for the
//! browser tests, prints `SLATH_E2E_URL=<base url>`, and runs until stdin
//! closes (the Playwright launcher holds the pipe open).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};

fn main() {
    let server = slath_tests::server::TestServer::start_with(&[]);
    println!("SLATH_E2E_URL={}", server.base());
    std::io::stdout().flush().unwrap();
    let mut sink = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut sink);
    drop(server);
}
