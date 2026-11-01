// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The UI bundle in `web/dist` is embedded into the binary. It's built by
//! `just web` (Node.js is needed at build time only).

fn main() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    println!("cargo:rerun-if-changed=templates");
    if !dist.join("app.js").is_file() {
        println!(
            "cargo:warning=web/dist is missing; run `just web` to build the UI bundle (the server will serve no assets)"
        );
        let _ = std::fs::create_dir_all(&dist);
    }
}
