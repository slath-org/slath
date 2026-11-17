// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Syntax highlighting with syntect: classed HTML (no inline styles), one
//! string per source line. Callers cap the input size and run this on a
//! blocking thread with a timeout.

use std::sync::OnceLock;

use syntect::html::{ClassStyle, line_tokens_to_classed_spans};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// Class prefix of highlighted spans (`syn-keyword`, `syn-string`, …).
pub const CLASS_PREFIX: &str = "syn-";

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Highlighted code: one HTML fragment per line (without the newline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Highlighted {
    pub lines: Vec<String>,
    /// The syntax used, e.g. "Rust"; "Plain Text" when unknown.
    pub language: String,
}

fn syntax_for(path: &str, first_line: &str) -> &'static SyntaxReference {
    let set = syntaxes();
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e);
    ext.and_then(|e| set.find_syntax_by_extension(e))
        .or_else(|| set.find_syntax_by_extension(name))
        .or_else(|| set.find_syntax_by_first_line(first_line))
        .unwrap_or_else(|| set.find_syntax_plain_text())
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if crate::is_bidi_control(c) => {
                out.push_str(&format!("<span class=\"bidi-control\" title=\"Text direction control U+{0:04X}\">[U+{0:04X}]</span>", u32::from(c)));
            }
            c => out.push(c),
        }
    }
}

/// Plain (escaped, unhighlighted) lines, e.g. after a highlighting timeout.
#[must_use]
pub fn plain(text: &str) -> Highlighted {
    Highlighted {
        lines: text
            .lines()
            .map(|l| {
                let mut s = String::new();
                escape(l, &mut s);
                s
            })
            .collect(),
        language: "Plain Text".to_owned(),
    }
}

/// Highlights `text` for the file at `path`. Falls back to plain text when
/// the syntax is unknown or a line can't be parsed.
#[must_use]
pub fn highlight(path: &str, text: &str) -> Highlighted {
    let syntax = syntax_for(path, text.lines().next().unwrap_or(""));
    if syntax.name == "Plain Text" {
        return plain(text);
    }
    let set = syntaxes();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut lines = Vec::new();
    for line in LinesWithEndings::from(text) {
        let Ok(ops) = state.parse_line(line, set) else {
            return plain(text);
        };
        // Escape the text pieces ourselves (also revealing bidi controls),
        // then let syntect wrap them in classed spans.
        let Ok((html, _)) = line_tokens_to_classed_spans(
            line,
            &ops,
            ClassStyle::SpacedPrefixed {
                prefix: CLASS_PREFIX,
            },
            &mut stack,
        ) else {
            return plain(text);
        };
        let html = reveal_bidi_html(&html);
        lines.push(html.trim_end_matches(['\n', '\r']).to_owned());
    }
    // Close spans left open at the end of a line on the next line's start:
    // each line must be well-formed on its own.
    let lines = balance(lines);
    Highlighted {
        lines,
        language: syntax.name.clone(),
    }
}

/// syntect escapes `&<>"'`; this additionally makes bidi controls visible.
fn reveal_bidi_html(html: &str) -> String {
    if !html.chars().any(crate::is_bidi_control) {
        return html.to_owned();
    }
    let mut out = String::new();
    for c in html.chars() {
        if crate::is_bidi_control(c) {
            out.push_str(&format!("<span class=\"bidi-control\" title=\"Text direction control U+{0:04X}\">[U+{0:04X}]</span>", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

/// Makes every line self-contained: spans still open at the end of a line
/// are closed there and reopened at the start of the next.
fn balance(lines: Vec<String>) -> Vec<String> {
    let mut open: Vec<String> = Vec::new();
    lines
        .into_iter()
        .map(|line| {
            let mut out: String = open.concat();
            let mut rest = line.as_str();
            while let Some(i) = rest.find('<') {
                out.push_str(&rest[..i]);
                rest = &rest[i..];
                let end = rest.find('>').map_or(rest.len(), |e| e + 1);
                let tag = &rest[..end];
                if tag.starts_with("</span") {
                    open.pop();
                } else if tag.starts_with("<span") && !tag.contains("bidi-control") {
                    open.push(tag.to_owned());
                }
                out.push_str(tag);
                rest = &rest[end..];
            }
            out.push_str(rest);
            for _ in &open {
                out.push_str("</span>");
            }
            out
        })
        .collect()
}

/// Marks character ranges of a highlighted line: `ranges` are
/// `(start, end)` positions in the line's text (characters, before
/// escaping), sorted and apart. Each range is wrapped in `open` …
/// `close`; where a range crosses a highlighting span, it's closed before
/// the span's tag and opened again after it with `reopen`, so the HTML
/// stays well-formed. A revealed bidi control counts as one character.
#[must_use]
pub fn emphasize(
    html: &str,
    ranges: &[(usize, usize)],
    open: &str,
    reopen: &str,
    close: &str,
) -> String {
    const BIDI: &str = "<span class=\"bidi-control\"";
    let mut out = String::with_capacity(html.len() + ranges.len() * 32);
    let mut pos = 0usize;
    let mut next = 0usize;
    let mut inside = false;
    let mut started = false;
    let mut rest = html;
    while !rest.is_empty() {
        // One unit: a tag, a revealed bidi control, an entity, or a char.
        let (unit, is_text) = if rest.starts_with(BIDI) {
            let end = rest
                .find("</span>")
                .map_or(rest.len(), |e| e + "</span>".len());
            (&rest[..end], true)
        } else if rest.starts_with('<') {
            let end = rest.find('>').map_or(rest.len(), |e| e + 1);
            (&rest[..end], false)
        } else if rest.starts_with('&') {
            let end = rest.find(';').map_or(1, |e| e + 1);
            (&rest[..end], true)
        } else {
            let len = rest.chars().next().map_or(1, char::len_utf8);
            (&rest[..len], true)
        };
        rest = &rest[unit.len()..];
        if !is_text {
            if inside {
                out.push_str(close);
                inside = false;
            }
            out.push_str(unit);
            continue;
        }
        while next < ranges.len() && ranges[next].1 <= pos {
            next += 1;
            started = false;
        }
        let in_range = next < ranges.len() && ranges[next].0 <= pos && pos < ranges[next].1;
        if in_range && !inside {
            out.push_str(if started { reopen } else { open });
            inside = true;
            started = true;
        }
        out.push_str(unit);
        pos += 1;
        if inside && next < ranges.len() && pos >= ranges[next].1 {
            out.push_str(close);
            inside = false;
        }
    }
    if inside {
        out.push_str(close);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_with_classes_only() {
        let h = highlight("src/main.rs", "fn main() {\n    let s = \"<b>\";\n}\n");
        assert_eq!(h.language, "Rust");
        assert_eq!(h.lines.len(), 3);
        assert!(h.lines[0].contains("class=\"syn-"), "{:?}", h.lines);
        assert!(h.lines[1].contains("&lt;b&gt;"), "{:?}", h.lines);
        assert!(!h.lines.concat().contains("style="));
        for l in &h.lines {
            assert_eq!(
                l.matches("<span").count(),
                l.matches("</span>").count(),
                "{l}"
            );
        }
    }

    #[test]
    fn unknown_files_are_plain_and_escaped() {
        let h = highlight("notes.unknownext", "<script>x</script>\n");
        assert_eq!(h.language, "Plain Text");
        assert_eq!(h.lines, vec!["&lt;script&gt;x&lt;/script&gt;"]);
    }

    #[test]
    fn trojan_source_is_visible() {
        let h = highlight("a.rs", "let x = \"\u{202e}abc\";\n");
        assert!(h.lines[0].contains("[U+202E]"), "{:?}", h.lines);
        let p = highlight("a.txt", "a\u{2066}b\n");
        assert!(p.lines[0].contains("[U+2066]"), "{:?}", p.lines);
    }

    #[test]
    fn emphasis_wraps_text_and_splits_at_spans() {
        use super::emphasize;
        let m = |h: &str, r: &[(usize, usize)]| emphasize(h, r, "<ins>", "<ins>", "</ins>");
        assert_eq!(m("abcdef", &[(1, 3)]), "a<ins>bc</ins>def");
        assert_eq!(
            m("abcdef", &[(0, 1), (5, 6)]),
            "<ins>a</ins>bcde<ins>f</ins>"
        );
        // Across a span boundary: closed before the tag, reopened after.
        assert_eq!(
            m(r#"ab<span class="syn-x">cd</span>ef"#, &[(1, 5)]),
            r#"a<ins>b</ins><span class="syn-x"><ins>cd</ins></span><ins>e</ins>f"#
        );
        // Entities count as one character.
        assert_eq!(m("a&lt;b", &[(1, 2)]), "a<ins>&lt;</ins>b");
        // The first piece may differ (a screen-reader prefix).
        assert_eq!(
            emphasize("a<b>c</b>", &[(0, 2)], "<ins>P:", "<ins>", "</ins>"),
            "<ins>P:a</ins><b><ins>c</ins></b>"
        );
        // A revealed bidi control is one character.
        let h = super::highlight("x.txt", "a\u{202E}b").lines[0].clone();
        let e = m(&h, &[(2, 3)]);
        assert!(e.ends_with("<ins>b</ins>"), "{e}");
        assert_eq!(m("", &[(0, 1)]), "");
    }

    #[test]
    fn emphasis_keeps_highlighted_text() {
        let h = super::highlight("x.rs", "let x = \"a<b\";").lines[0].clone();
        let e = super::emphasize(&h, &[(8, 13)], "<ins>", "<ins>", "</ins>");
        let strip = |s: &str| {
            let mut out = String::new();
            let mut tag = false;
            for c in s.chars() {
                match c {
                    '<' => tag = true,
                    '>' => tag = false,
                    c if !tag => out.push(c),
                    _ => {}
                }
            }
            out
        };
        assert_eq!(strip(&e), strip(&h));
        assert_eq!(e.matches("<ins>").count(), e.matches("</ins>").count());
    }
}
