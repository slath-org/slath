// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Titles of issues and merge requests: plain text, except that text in
//! backticks shows as code (`Upgrade `vite` to v8`), as in Markdown code
//! spans. Nothing else is interpreted.

fn escape_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// The length of the backtick run starting at byte `i`.
fn run_at(s: &str, i: usize) -> usize {
    s[i..].bytes().take_while(|&b| b == b'`').count()
}

/// A title as HTML: escaped, with code spans as `<code>`. A run of N
/// backticks opens a span that the next run of exactly N closes; a run
/// without a match stays as written. As in CommonMark, one space is
/// stripped from both ends of a span that has a space at each end.
#[must_use]
pub fn title_html(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 16);
    let mut i = 0;
    let mut text_start = 0;
    while i < src.len() {
        if src.as_bytes()[i] != b'`' {
            i += src[i..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let n = run_at(src, i);
        // Look for a closing run of exactly n backticks.
        let mut j = i + n;
        let mut close = None;
        while j < src.len() {
            if src.as_bytes()[j] == b'`' {
                let m = run_at(src, j);
                if m == n {
                    close = Some(j);
                    break;
                }
                j += m;
            } else {
                j += src[j..].chars().next().map_or(1, char::len_utf8);
            }
        }
        match close {
            Some(c) if c > i + n => {
                escape_into(&src[text_start..i], &mut out);
                let mut code = &src[i + n..c];
                if code.len() >= 2
                    && code.starts_with(' ')
                    && code.ends_with(' ')
                    && !code.trim().is_empty()
                {
                    code = &code[1..code.len() - 1];
                }
                out.push_str("<code>");
                escape_into(code, &mut out);
                out.push_str("</code>");
                i = c + n;
                text_start = i;
            }
            _ => i += n,
        }
    }
    escape_into(&src[text_start..], &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::title_html;

    #[test]
    fn code_spans_and_nothing_else() {
        assert_eq!(
            title_html("Upgrade `vite` from v5 to v8"),
            "Upgrade <code>vite</code> from v5 to v8"
        );
        assert_eq!(
            title_html("Implement `/repos/{org}/{repo}/forks` for <now>"),
            "Implement <code>/repos/{org}/{repo}/forks</code> for &lt;now&gt;"
        );
        // Markdown other than code spans stays text.
        assert_eq!(title_html("**bold** _x_ [l](u)"), "**bold** _x_ [l](u)");
        // Longer fences hold backticks; one space trims at both ends.
        assert_eq!(
            title_html("Use `` `a` `` here"),
            "Use <code>`a`</code> here"
        );
        // Unmatched or empty runs stay as written.
        assert_eq!(title_html("Don't `close"), "Don&#39;t `close");
        assert_eq!(title_html("Empty `` pair"), "Empty `` pair");
        assert_eq!(title_html("a `b` c `d"), "a <code>b</code> c `d");
        // Escaped inside code too.
        assert_eq!(title_html("`<script>`"), "<code>&lt;script&gt;</code>");
        assert_eq!(title_html("Ünïcode `ö`"), "Ünïcode <code>ö</code>");
    }
}
