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
    let mut lines = body.lines();
    // The opening fence: up to three spaces of indentation, three or more
    // backticks or tildes, and the info string `suggestion`.
    let (indent, fence) = lines.by_ref().find_map(|l| {
        let indent = l.len() - l.trim_start_matches(' ').len();
        let rest = &l[indent..];
        let ch = rest.chars().next()?;
        if indent > 3 || !(ch == '`' || ch == '~') {
            return None;
        }
        let n = rest.chars().take_while(|c| *c == ch).count();
        (n >= 3 && rest[n..].trim() == "suggestion").then(|| (indent, rest[..n].to_owned()))
    })?;
    let mut out = String::new();
    for l in lines {
        let t = l.trim_start_matches(' ');
        if l.len() - t.len() <= 3
            && t.starts_with(&fence)
            && t.trim_end().chars().all(|c| fence.starts_with(c))
        {
            return Some(out);
        }
        // Content lines lose up to the fence's indentation.
        let strip = (l.len() - l.trim_start_matches(' ').len()).min(indent);
        out.push_str(&l[strip..]);
        out.push('\n');
    }
    None
}

#[cfg(test)]
mod tests {
    use super::suggestion;

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
        // Only the first block counts.
        assert_eq!(
            suggestion("```suggestion\na\n```\n```suggestion\nb\n```"),
            Some("a\n".to_owned())
        );
    }
}
