// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rebuilds the crate when a migration is added or changed: `sqlx::migrate!`
//! embeds the directory, but incremental builds don't notice new files.

fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
