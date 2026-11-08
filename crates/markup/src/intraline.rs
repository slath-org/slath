// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Character-aware diffs: within a hunk, removed and added lines are paired
//! by similarity, and each pair is diffed on extended grapheme clusters, so
//! the changed characters can be marked. Hunks, line numbers, and anchors
//! stay line-based; this only refines how paired lines are shown.
//!
//! [`Refiner`] is the pluggable step: [`CharacterRefiner`] is the default,
//! and a syntax-aware refiner can replace it later.

use unicode_segmentation::UnicodeSegmentation;

/// A run of a line's text, marked if it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub changed: bool,
}

/// The marked runs of a removed and an added line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refined {
    pub old: Vec<Segment>,
    pub new: Vec<Segment>,
}

/// How much refinement may do.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    /// Longer lines (in bytes) get no emphasis.
    pub max_line_bytes: usize,
    /// More pairs per file get no emphasis at all.
    pub max_pairs: usize,
    /// Changes that are only whitespace aren't marked.
    pub ignore_whitespace: bool,
}

/// The refinement step of the diff renderer.
pub trait Refiner {
    /// For a run of removed lines followed by added lines: which of them
    /// pair up (indices into `old` and `new`, in order, never crossing) and
    /// their marked runs.
    fn refine(&self, old: &[&str], new: &[&str], bounds: &Bounds) -> Vec<(usize, usize, Refined)>;
}

/// Grapheme-level refinement (spec: character-aware diffs).
#[derive(Debug, Clone, Copy, Default)]
pub struct CharacterRefiner;

/// Pairs are only made between lines at least this similar (Dice
/// coefficient of grapheme bigrams, in percent).
const PAIR_MIN_SIMILARITY: u32 = 50;

impl Refiner for CharacterRefiner {
    fn refine(&self, old: &[&str], new: &[&str], bounds: &Bounds) -> Vec<(usize, usize, Refined)> {
        if old.is_empty()
            || new.is_empty()
            || old.len().saturating_mul(new.len()) > bounds.max_pairs.saturating_mul(4)
        {
            return Vec::new();
        }
        let fits = |s: &str| s.len() <= bounds.max_line_bytes;
        pairs(old, new)
            .into_iter()
            .filter(|(i, j)| fits(old[*i]) && fits(new[*j]))
            .filter_map(|(i, j)| {
                refine_pair(old[i], new[j], bounds.ignore_whitespace).map(|r| (i, j, r))
            })
            .collect()
    }
}

fn bigrams(s: &str) -> Vec<(&str, &str)> {
    let g: Vec<&str> = s.graphemes(true).collect();
    g.windows(2).map(|w| (w[0], w[1])).collect()
}

/// Similarity in percent (Dice coefficient over grapheme bigrams).
fn similarity(a: &str, b: &str) -> u32 {
    if a == b {
        return 100;
    }
    let (x, mut y) = (bigrams(a), bigrams(b));
    if x.is_empty() || y.is_empty() {
        return 0;
    }
    let total = x.len() + y.len();
    let mut common = 0;
    for g in &x {
        if let Some(k) = y.iter().position(|h| h == g) {
            y.swap_remove(k);
            common += 1;
        }
    }
    u32::try_from(200 * common / total).unwrap_or(0)
}

/// In-order pairing that maximizes total similarity, with pairs only above
/// the threshold (dynamic programming, no crossing pairs).
fn pairs(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    let (m, n) = (old.len(), new.len());
    let sim: Vec<Vec<u32>> = old
        .iter()
        .map(|a| new.iter().map(|b| similarity(a, b)).collect())
        .collect();
    // best[i][j]: best total for old[i..] and new[j..].
    let mut best = vec![vec![0u32; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            let skip = best[i + 1][j].max(best[i][j + 1]);
            let take = if sim[i][j] >= PAIR_MIN_SIMILARITY {
                sim[i][j] + best[i + 1][j + 1]
            } else {
                0
            };
            best[i][j] = skip.max(take);
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < m && j < n {
        if sim[i][j] >= PAIR_MIN_SIMILARITY && best[i][j] == sim[i][j] + best[i + 1][j + 1] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if best[i + 1][j] >= best[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// The changed graphemes of `a` and `b` (true where changed), by a longest
/// common subsequence of grapheme clusters.
fn changed(a: &[&str], b: &[&str]) -> (Vec<bool>, Vec<bool>) {
    let (m, n) = (a.len(), b.len());
    let mut lcs = vec![vec![0u16; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut ca, mut cb) = (vec![true; m], vec![true; n]);
    let (mut i, mut j) = (0, 0);
    while i < m && j < n {
        if a[i] == b[j] {
            ca[i] = false;
            cb[j] = false;
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (ca, cb)
}

/// Widens changes to whole words where they cover most of a word.
fn snap_to_words(line: &str, graphemes: &[&str], marks: &mut [bool]) {
    let mut pos = 0;
    for word in line.split_word_bounds() {
        let len = word.graphemes(true).count();
        let end = (pos + len).min(marks.len());
        let is_word = word.chars().any(char::is_alphanumeric);
        if is_word && len > 1 {
            let hit = marks[pos..end].iter().filter(|m| **m).count();
            if hit * 2 > len {
                marks[pos..end].iter_mut().for_each(|m| *m = true);
            }
        }
        pos = end;
    }
    debug_assert_eq!(pos, graphemes.len());
}

fn segments(graphemes: &[&str], marks: &[bool], ignore_whitespace: bool) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    for (g, m) in graphemes.iter().zip(marks) {
        match out.last_mut() {
            Some(s) if s.changed == *m => s.text.push_str(g),
            _ => out.push(Segment {
                text: (*g).to_owned(),
                changed: *m,
            }),
        }
    }
    if ignore_whitespace {
        for s in &mut out {
            if s.changed && s.text.chars().all(char::is_whitespace) {
                s.changed = false;
            }
        }
        // Merge runs that became neighbours of the same kind.
        let mut merged: Vec<Segment> = Vec::new();
        for s in out {
            match merged.last_mut() {
                Some(l) if l.changed == s.changed => l.text.push_str(&s.text),
                _ => merged.push(s),
            }
        }
        out = merged;
    }
    out
}

/// The marked runs of one pair; `None` when more than half of either line
/// changed (no emphasis rather than noise) or nothing did.
#[must_use]
pub fn refine_pair(old: &str, new: &str, ignore_whitespace: bool) -> Option<Refined> {
    let a: Vec<&str> = old.graphemes(true).collect();
    let b: Vec<&str> = new.graphemes(true).collect();
    // The table is quadratic; callers bound line lengths, this is a backstop.
    if a.len() > 4096 || b.len() > 4096 {
        return None;
    }
    let (mut ca, mut cb) = changed(&a, &b);
    let share = |c: &[bool]| c.iter().filter(|m| **m).count() * 2 > c.len();
    if share(&ca) || share(&cb) {
        return None;
    }
    snap_to_words(old, &a, &mut ca);
    snap_to_words(new, &b, &mut cb);
    let r = Refined {
        old: segments(&a, &ca, ignore_whitespace),
        new: segments(&b, &cb, ignore_whitespace),
    };
    let any = |s: &[Segment]| s.iter().any(|x| x.changed);
    (any(&r.old) || any(&r.new)).then_some(r)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const B: Bounds = Bounds {
        max_line_bytes: 1000,
        max_pairs: 2000,
        ignore_whitespace: false,
    };

    fn marked(s: &[Segment]) -> Vec<&str> {
        s.iter()
            .filter(|x| x.changed)
            .map(|x| x.text.as_str())
            .collect()
    }

    fn join(s: &[Segment]) -> String {
        s.iter().map(|x| x.text.as_str()).collect()
    }

    #[test]
    fn marks_the_changed_word() {
        // Most of the word changed: the whole word is marked.
        let r = refine_pair("let total = count + 1;", "let total = items + 1;", false).unwrap();
        assert_eq!(marked(&r.old), vec!["count"]);
        assert_eq!(marked(&r.new), vec!["items"]);
        // A small part of it: only that part.
        let r = refine_pair("let total = count + 1;", "let total = amount + 1;", false).unwrap();
        assert_eq!(marked(&r.old), vec!["c"]);
        assert_eq!(marked(&r.new), vec!["am"]);
    }

    #[test]
    fn mostly_changed_lines_get_no_emphasis() {
        assert_eq!(refine_pair("abc", "xyz", false), None);
        assert_eq!(refine_pair("same", "same", false), None);
    }

    #[test]
    fn pairs_in_order_by_similarity() {
        let old = ["fn one() {}", "completely different", "fn three() {}"];
        let new = ["fn one(x: u8) {}", "fn three(y: u8) {}"];
        let got: Vec<(usize, usize)> = CharacterRefiner
            .refine(&old, &new, &B)
            .iter()
            .map(|(i, j, _)| (*i, *j))
            .collect();
        assert_eq!(got, vec![(0, 0), (2, 1)]);
    }

    #[test]
    fn graphemes_stay_whole() {
        // é as e + combining acute; a family emoji (ZWJ sequence); a flag.
        for (a, b) in [
            ("cafe\u{301} au lait", "cafe\u{300} au lait"),
            ("hello 👨‍👩‍👧 world", "hello 👨‍👩‍👦 world"),
            ("flag 🇩🇪 here", "flag 🇫🇷 here"),
        ] {
            let r = refine_pair(a, b, false).unwrap();
            for s in r.old.iter().chain(&r.new) {
                assert!(!s.text.is_empty());
            }
            assert_eq!(join(&r.old), a);
            assert_eq!(join(&r.new), b);
            for m in marked(&r.old).iter().chain(marked(&r.new).iter()) {
                assert!(m.graphemes(true).count() >= 1);
            }
        }
        let r = refine_pair("hello 👨‍👩‍👧 world", "hello 👨‍👩‍👦 world", false).unwrap();
        assert_eq!(marked(&r.old), vec!["👨‍👩‍👧"]);
    }

    #[test]
    fn cjk_and_rtl_text() {
        let r = refine_pair("今日は良い天気です", "今日は悪い天気です", false).unwrap();
        assert_eq!(marked(&r.old), vec!["良"]);
        let r = refine_pair("שלום עולם יפה", "שלום עולם טוב", false).unwrap();
        assert_eq!(marked(&r.new), vec!["טוב"]);
        let r = refine_pair("مرحبا بالعالم الجميل", "مرحبا بالعالم الكبير", false).unwrap();
        assert_eq!(join(&r.new), "مرحبا بالعالم الكبير");
    }

    #[test]
    fn whitespace_runs() {
        let r = refine_pair("\tif x {", "    if x {", false);
        assert!(r.is_some());
        // Ignored, a whitespace-only change marks nothing.
        assert_eq!(refine_pair("a  b  c d e f", "a b c d e f", true), None);
    }

    #[test]
    fn bounds_fall_back_to_line_diffs() {
        let long_a = format!("{}a", "x".repeat(2000));
        let long_b = format!("{}b", "x".repeat(2000));
        assert!(
            CharacterRefiner
                .refine(&[&long_a], &[&long_b], &B)
                .is_empty()
        );
        let tight = Bounds { max_pairs: 0, ..B };
        assert!(
            CharacterRefiner
                .refine(&["ab cd"], &["ab ce"], &tight)
                .is_empty()
        );
    }

    proptest! {
        #[test]
        fn refinement_reproduces_both_lines(a in "\\PC{0,40}", b in "\\PC{0,40}") {
            if let Some(r) = refine_pair(&a, &b, false) {
                prop_assert_eq!(join(&r.old), a.clone());
                prop_assert_eq!(join(&r.new), b.clone());
                // Every segment boundary is a grapheme boundary.
                let mut pos = 0;
                let bounds: Vec<usize> = a.grapheme_indices(true).map(|(i, _)| i).chain([a.len()]).collect();
                for s in &r.old {
                    prop_assert!(bounds.contains(&pos));
                    pos += s.text.len();
                }
                prop_assert!(bounds.contains(&pos));
            }
        }
    }
}
