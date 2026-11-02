// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Renders Markdown from stdin with the server renderer and prints the HTML.
//!
//! Used to compare editor round-trip output with the authoritative renderer:
//! `cargo run -q -p slath-markup --example render < doc.md`
use std::io::{Read, Write};

use slath_markup::{MarkdownOptions, render_markdown};

fn main() -> std::io::Result<()> {
    let mut src = String::new();
    std::io::stdin().read_to_string(&mut src)?;
    let html = render_markdown(
        &src,
        MarkdownOptions {
            external_images: true,
        },
    );
    std::io::stdout().write_all(html.as_bytes())
}
