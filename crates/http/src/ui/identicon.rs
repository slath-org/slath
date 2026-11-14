// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Locally generated avatars: a symmetric 5×5 pattern derived from a hash
//! of the name. No remote avatar service is ever contacted.

use sha2::{Digest, Sha256};

/// Muted foreground colors that read on both light and dark backgrounds.
const COLORS: [&str; 8] = [
    "#4A6B5A", "#6B5A3A", "#5A5A80", "#7A4A4A", "#3F6575", "#6A507A", "#5E6B3A", "#7A5A30",
];

/// The pattern for `name`, drawn in the page: which of the eight Weave
/// avatar tones it takes, and the SVG path of its cells (a 7×7 box with a
/// one-cell margin, symmetric about the middle column).
#[must_use]
pub fn pattern(name: &str) -> (usize, String) {
    let h = Sha256::digest(name.to_lowercase().as_bytes());
    let tone = usize::from(h[0]) % COLORS.len();
    let mut path = String::new();
    for row in 0..5u8 {
        for col in 0..3u8 {
            if (h[1 + usize::from(row)] >> col) & 1 == 0 {
                continue;
            }
            let cols: &[u8] = if col == 2 { &[2] } else { &[col, 4 - col] };
            for c in cols {
                path.push_str(&format!("M{} {}h1v1h-1z", c + 1, row + 1));
            }
        }
    }
    (tone, path)
}

/// The SVG document for `name`.
#[must_use]
pub fn svg(name: &str) -> String {
    let h = Sha256::digest(name.as_bytes());
    let color = COLORS[usize::from(h[0]) % COLORS.len()];
    let mut cells = String::new();
    for row in 0..5 {
        for col in 0..3 {
            let bit = (h[1 + row] >> col) & 1 == 1;
            if !bit {
                continue;
            }
            for c in if col == 2 {
                vec![2]
            } else {
                vec![col, 4 - col]
            } {
                cells.push_str(&format!(
                    r#"<rect x="{}" y="{}" width="1" height="1"/>"#,
                    c + 1,
                    row + 1
                ));
            }
        }
    }
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 7 7" width="64" height="64"><rect width="7" height="7" fill="#EAECE7"/><g fill="{color}">{cells}</g></svg>"##
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn deterministic_and_inert() {
        let a = super::svg("alice");
        assert_eq!(a, super::svg("alice"));
        assert_ne!(a, super::svg("bob"));
        assert!(!a.contains("script") && !a.contains("href"));
    }

    #[test]
    fn pattern_matches_the_document_and_ignores_case() {
        let (tone, path) = super::pattern("Alice");
        assert_eq!((tone, path.clone()), super::pattern("alice"));
        assert!(tone < 8);
        // Same cells as the image: one square per rect.
        assert_eq!(
            path.matches('M').count(),
            super::svg("alice").matches("<rect x").count()
        );
        assert!(
            path.chars()
                .all(|c| c.is_ascii_digit() || "Mhvz -".contains(c))
        );
    }
}
