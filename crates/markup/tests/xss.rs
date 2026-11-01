// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The XSS corpus: every `tests/xss/*.md` renders to its `.html` snapshot,
//! and every output satisfies the safety invariants. Review snapshot
//! changes like code; regenerate with `SLATH_UPDATE_SNAPSHOTS=1`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use slath_markup::{MarkdownOptions, render_markdown};

/// The tags of an HTML fragment as (name, [(attribute, value)]).
fn tags(html: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut out = Vec::new();
    let b = html.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' || b.get(i + 1).is_some_and(|c| *c == b'/' || *c == b'!') {
            i += 1;
            continue;
        }
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_alphanumeric() {
            i += 1;
        }
        let name = html[start..i].to_ascii_lowercase();
        let mut attrs = Vec::new();
        loop {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= b.len() || b[i] == b'>' || b[i] == b'/' {
                break;
            }
            let a = i;
            while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'>' {
                i += 1;
            }
            let attr = html[a..i].to_ascii_lowercase();
            let mut value = String::new();
            if i < b.len() && b[i] == b'=' {
                i += 1;
                let q = b[i];
                if q == b'"' || q == b'\'' {
                    i += 1;
                    let v = i;
                    while i < b.len() && b[i] != q {
                        i += 1;
                    }
                    value = html[v..i].to_owned();
                    i += 1;
                } else {
                    let v = i;
                    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                        i += 1;
                    }
                    value = html[v..i].to_owned();
                }
            }
            attrs.push((attr, value));
        }
        out.push((name, attrs));
    }
    out
}

/// Things that must never appear in rendered user content.
fn violations(html: &str) -> Vec<String> {
    let mut v = Vec::new();
    for (tag, attrs) in tags(html) {
        if [
            "script", "iframe", "object", "embed", "form", "svg", "math", "style", "button",
            "noscript", "meta", "link", "base",
        ]
        .contains(&tag.as_str())
        {
            v.push(format!("<{tag}>"));
        }
        for (name, value) in attrs {
            if name.starts_with("on")
                || name == "style"
                || name.starts_with("hx-")
                || name.starts_with("data-")
            {
                v.push(format!("<{tag} {name}>"));
            }
            if ["href", "src", "srcset", "action", "formaction"].contains(&name.as_str()) {
                let val = value
                    .to_ascii_lowercase()
                    .replace(['\t', '\n', '\r', ' '], "");
                if val.starts_with("javascript:")
                    || val.starts_with("vbscript:")
                    || val.starts_with("data:")
                {
                    v.push(format!("<{tag} {name}={value}>"));
                }
            }
        }
    }
    v
}

#[test]
fn corpus_matches_snapshots_and_is_safe() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/xss");
    let update = std::env::var_os("SLATH_UPDATE_SNAPSHOTS").is_some();
    let mut cases: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 9);
    let mut failures = Vec::new();
    for md in cases {
        let src = std::fs::read_to_string(&md).unwrap();
        for (suffix, opts) in [
            (
                "html",
                MarkdownOptions {
                    external_images: true,
                },
            ),
            (
                "noext.html",
                MarkdownOptions {
                    external_images: false,
                },
            ),
        ] {
            let html = render_markdown(&src, opts);
            let bad = violations(&html);
            if !bad.is_empty() {
                failures.push(format!("{}: unsafe output {bad:?}:\n{html}", md.display()));
            }
            let snap = md.with_extension(suffix);
            if update {
                std::fs::write(&snap, &html).unwrap();
            } else {
                let want = std::fs::read_to_string(&snap).unwrap_or_default();
                if want != html {
                    failures.push(format!(
                        "{}: differs from {}:\n{html}",
                        md.display(),
                        snap.display()
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn bidi_controls_never_survive() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/xss/07-bidi-and-confusables.md");
    let src = std::fs::read_to_string(path).unwrap();
    let html = render_markdown(&src, MarkdownOptions::default());
    assert!(!html.chars().any(slath_markup::is_bidi_control), "{html}");
    assert!(html.contains("[U+202E]"));
}
