// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Suggestions in review comments: a fenced block with the info string
//! `suggestion` holds the text that replaces the commented lines.
//!
//! ````text
//! ```suggestion
//! let total = items.len();
//! ```
//! ````
//!
//! An empty block suggests deleting the lines. Only the first block counts,
//! and one without a closing fence is no suggestion.

/// The replacement text of the first suggestion block in `body`: its
/// lines, each ending with `\n`.
#[must_use]
pub fn suggestion(body: &str) -> Option<String> {
    split(body).map(|(_, text, _)| text)
}

/// `body` without its first suggestion block (for showing the comment
/// next to the suggestion), or `None` if it has none.
#[must_use]
pub fn without_suggestion(body: &str) -> Option<String> {
    split(body).map(|(before, _, after)| format!("{before}{after}"))
}

/// The text before the first suggestion block, its content, and the text
/// after it.
fn split(body: &str) -> Option<(&str, String, &str)> {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(body.match_indices('\n').map(|(i, _)| i + 1))
        .filter(|i| *i <= body.len())
        .collect();
    let line_at = |k: usize| -> &str {
        let s = starts[k];
        let e = starts.get(k + 1).map_or(body.len(), |n| *n);
        body[s..e].trim_end_matches(['\n', '\r'])
    };
    let lines = starts.len();
    let (open, indent, fence) = (0..lines).find_map(|k| {
        let l = line_at(k);
        let indent = l.len() - l.trim_start_matches(' ').len();
        let rest = &l[indent..];
        let ch = rest.chars().next()?;
        if indent > 3 || !(ch == '`' || ch == '~') {
            return None;
        }
        let n = rest.chars().take_while(|c| *c == ch).count();
        (n >= 3 && rest[n..].trim() == "suggestion").then(|| (k, indent, rest[..n].to_owned()))
    })?;
    let mut out = String::new();
    for k in open + 1..lines {
        let l = line_at(k);
        let t = l.trim_start_matches(' ');
        if l.len() - t.len() <= 3
            && t.starts_with(&fence)
            && t.trim_end().chars().all(|c| fence.starts_with(c))
        {
            let after = starts.get(k + 1).map_or("", |n| &body[*n..]);
            return Some((&body[..starts[open]], out, after));
        }
        let strip = (l.len() - l.trim_start_matches(' ').len()).min(indent);
        out.push_str(&l[strip..]);
        out.push('\n');
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{suggestion, without_suggestion};

    #[test]
    fn suggestion_blocks() {
        assert_eq!(
            suggestion("Better:\n\n```suggestion\nlet x = 1;\n  y();\n```\nthanks"),
            Some("let x = 1;\n  y();\n".to_owned())
        );
        assert_eq!(suggestion("```suggestion\n```"), Some(String::new()));
        assert_eq!(
            suggestion("~~~~ suggestion \nkeep ``` inside\n~~~~"),
            Some("keep ``` inside\n".to_owned())
        );
        assert_eq!(
            suggestion("  ```suggestion\n  a\n b\n  ```"),
            Some("a\nb\n".to_owned())
        );
        // Not a suggestion: other languages, no closing fence, too indented.
        assert_eq!(suggestion("```rust\nx\n```"), None);
        assert_eq!(suggestion("```suggestion\nx"), None);
        assert_eq!(suggestion("    ```suggestion\nx\n    ```"), None);
        assert_eq!(
            without_suggestion("Better:\n\n```suggestion\nx\n```\nThanks."),
            Some("Better:\n\nThanks.".to_owned())
        );
        assert_eq!(without_suggestion("No block."), None);
        // Only the first block counts.
        assert_eq!(
            suggestion("```suggestion\na\n```\n```suggestion\nb\n```"),
            Some("a\n".to_owned())
        );
    }
}
