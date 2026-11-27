// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The brief schema `slath-brief/2`: a brief is a list of items, each of
//! one kind (what the change is for, what behaves differently, risks,
//! known gaps, rollout steps, how it was checked), in the author's order.
//! Checking a brief here is pure: its shape, kinds, and lengths, with the
//! JSON pointer of whatever is wrong. Where items point in the diff is
//! checked against the diff by the caller.

use serde::{Deserialize, Serialize};

use crate::error::{FieldError, Result, ServiceError};

/// The one schema this version accepts.
pub const SCHEMA: &str = "slath-brief/2";
/// The longest text of an item (its text, before, or after), in bytes.
pub const TEXT_MAX_BYTES: usize = 16 * 1024;

/// What an item says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Motivation,
    Effect,
    Risk,
    Gap,
    Rollout,
    Verification,
}

impl Kind {
    /// Every kind, in the order a brief shows them.
    pub const ALL: [Self; 6] = [
        Self::Motivation,
        Self::Effect,
        Self::Risk,
        Self::Gap,
        Self::Rollout,
        Self::Verification,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Motivation => "motivation",
            Self::Effect => "effect",
            Self::Risk => "risk",
            Self::Gap => "gap",
            Self::Rollout => "rollout",
            Self::Verification => "verification",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// One item's name ("Effect 2" numbers it).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Motivation => "Motivation",
            Self::Effect => "Effect",
            Self::Risk => "Risk",
            Self::Gap => "Known gap",
            Self::Rollout => "Rollout step",
            Self::Verification => "Verification",
        }
    }

    /// The heading over a kind's items.
    #[must_use]
    pub fn heading(self) -> &'static str {
        match self {
            Self::Motivation => "Motivation",
            Self::Effect => "Effects",
            Self::Risk => "Risks",
            Self::Gap => "Known gaps",
            Self::Rollout => "Rollout",
            Self::Verification => "Verification",
        }
    }

    /// What items of the kind say, for forms.
    #[must_use]
    pub fn hint(self) -> &'static str {
        match self {
            Self::Motivation => "What the change is for.",
            Self::Effect => "What behaves differently, with what happened before and after.",
            Self::Risk => "What could go wrong, and where.",
            Self::Gap => "What's known to be missing or wrong.",
            Self::Rollout => "Steps for deploying it, in order.",
            Self::Verification => "How it was checked: tests run, what was tried.",
        }
    }
}

/// One item of a brief.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub kind: Kind,
    pub text: String,
    /// For effects: what happened before the change, and after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Where in the diff it points.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<Anchor>,
}

/// The side of a diff lines are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    New,
    Old,
}

/// Where an item points: a whole file of the diff, or lines of one of its
/// hunks on one side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    /// The file's new path, or its old path when it's deleted.
    pub path: String,
    /// `new` (the default) or `old`, for removed lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<u32>,
    /// The SHA-256 of the lines, computed by Slath when it's saved (never
    /// sent by the author, never shown in the API).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_hash: Option<String>,
}

/// The most lines one anchor covers.
pub const ANCHOR_MAX_LINES: u32 = 500;

impl Anchor {
    #[must_use]
    pub fn side(&self) -> Side {
        self.side.unwrap_or(Side::New)
    }

    /// Its lines, first and last, if it points at lines.
    #[must_use]
    pub fn lines(&self) -> Option<(u32, u32)> {
        self.start.map(|s| (s, self.end.unwrap_or(s)))
    }

    /// As the brief shows it, and its form takes it: `src/a.rs`,
    /// `src/a.rs:10`, `src/a.rs:10-20`, `src/a.rs:old 10-20`.
    #[must_use]
    pub fn notation(&self) -> String {
        let old = if self.side() == Side::Old { "old " } else { "" };
        match self.lines() {
            None => self.path.clone(),
            Some((s, e)) if s == e => format!("{}:{old}{s}", self.path),
            Some((s, e)) => format!("{}:{old}{s}-{e}", self.path),
        }
    }

    /// Reads [`Anchor::notation`]; `None` for what isn't one. A path
    /// whose last colon isn't followed by line numbers is a whole file.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let whole = || Self {
            path: s.to_owned(),
            side: None,
            start: None,
            end: None,
            line_hash: None,
        };
        let Some((path, lines)) = s.rsplit_once(':') else {
            return Some(whole());
        };
        let (side, lines) = match lines.trim().strip_prefix("old ") {
            Some(rest) => (Some(Side::Old), rest.trim()),
            None => (None, lines.trim()),
        };
        let (a, b) = lines.split_once('-').unwrap_or((lines, lines));
        let (Ok(a), Ok(b)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>()) else {
            // `old x` must be lines; anything else is a file name.
            return side.is_none().then(whole);
        };
        Some(Self {
            path: path.trim().to_owned(),
            side,
            start: Some(a),
            end: (b != a).then_some(b),
            line_hash: None,
        })
    }
}

impl Item {
    /// Its text as lines (Before and After under it), for quoting it and
    /// for the SHA-256 a thread on it keeps.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut out: Vec<String> = self.text.lines().map(str::to_owned).collect();
        for (name, v) in [("Before", &self.before), ("After", &self.after)] {
            if let Some(v) = v {
                out.push(String::new());
                out.push(format!("{name}:"));
                out.extend(v.lines().map(str::to_owned));
            }
        }
        out
    }

    /// The SHA-256 of [`Item::lines`].
    #[must_use]
    pub fn hash(&self) -> String {
        let lines: Vec<Vec<u8>> = self.lines().into_iter().map(String::into_bytes).collect();
        crate::mr_threads::hash_lines(&lines)
    }
}

/// A brief as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upload {
    pub schema: String,
    pub items: Vec<Item>,
    /// Where it belongs (a version's commit for change briefs).
    #[serde(default)]
    pub commit: Option<String>,
}

/// Text with empty counting as absent, trailing whitespace dropped.
fn tidy(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim_end().to_owned()).filter(|s| !s.is_empty())
}

impl Upload {
    /// Parses and checks `body`, tidying its texts (trailing whitespace
    /// dropped, empty before and after removed).
    pub fn parse(body: &[u8]) -> Result<Self> {
        let mut u: Self = serde_json::from_slice(body).map_err(|e| {
            ServiceError::invalid("body", "invalid", format!("This isn't a valid brief: {e}."))
        })?;
        u.check()?;
        Ok(u)
    }

    fn check(&mut self) -> Result<()> {
        if self.schema != SCHEMA {
            return Err(ServiceError::invalid(
                "/schema",
                "schema",
                format!("Send \"schema\": \"{SCHEMA}\"."),
            ));
        }
        if self.items.is_empty() {
            return Err(ServiceError::invalid(
                "/items",
                "invalid",
                "Add at least one item, such as what the change is for.",
            ));
        }
        let mut errors = Vec::new();
        for (i, item) in self.items.iter_mut().enumerate() {
            item.text = item.text.trim_end().to_owned();
            item.before = tidy(item.before.take());
            item.after = tidy(item.after.take());
            if item.text.trim().is_empty() {
                errors.push(FieldError::new(
                    &format!("/items/{i}/text"),
                    "invalid",
                    "Write the item's text.",
                ));
            }
            if item.kind != Kind::Effect {
                for (name, v) in [("before", &item.before), ("after", &item.after)] {
                    if v.is_some() {
                        errors.push(FieldError::new(
                            &format!("/items/{i}/{name}"),
                            "invalid",
                            "Only effects have before and after.",
                        ));
                    }
                }
            }
            for (j, a) in item.anchors.iter().enumerate() {
                let at = format!("/items/{i}/anchors/{j}");
                let msg = if a.path.trim().is_empty() {
                    Some("Name a file of the diff.".to_owned())
                } else if a.line_hash.is_some() {
                    Some("Leave out line_hash: Slath computes it.".to_owned())
                } else if a.start.is_none() && (a.end.is_some() || a.side.is_some()) {
                    Some("Give start with end or side, or neither for the whole file.".to_owned())
                } else {
                    match a.lines() {
                        Some((0, _)) => Some("Lines start at 1.".to_owned()),
                        Some((s, e)) if e < s => Some("End before start.".to_owned()),
                        Some((s, e)) if e - s >= ANCHOR_MAX_LINES => {
                            Some(format!("Point at {ANCHOR_MAX_LINES} lines at most."))
                        }
                        _ => None,
                    }
                };
                if let Some(msg) = msg {
                    errors.push(FieldError::new(&at, "anchor", msg));
                }
            }
        }
        for (pointer, text) in self.texts() {
            if text.len() > TEXT_MAX_BYTES {
                errors.push(FieldError::new(
                    &pointer,
                    "too_long",
                    format!("Keep it to {} KiB.", TEXT_MAX_BYTES / 1024),
                ));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ServiceError::Invalid(errors))
        }
    }

    /// Checks every anchor against `diff`, the diff the brief describes,
    /// and records each line anchor's SHA-256: the file must be in it, and
    /// lines must be shown in one of its hunks on their side.
    pub fn locate(&mut self, diff: &slath_git::diff::Diff) -> Result<()> {
        let mut errors = Vec::new();
        for (i, item) in self.items.iter_mut().enumerate() {
            for (j, a) in item.anchors.iter_mut().enumerate() {
                if let Err(msg) = locate(diff, a) {
                    errors.push(FieldError::new(
                        &format!("/items/{i}/anchors/{j}"),
                        "anchor",
                        msg,
                    ));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ServiceError::Invalid(errors))
        }
    }

    /// Every text by JSON pointer, for length and secret checks.
    #[must_use]
    pub fn texts(&self) -> Vec<(String, &str)> {
        let mut out = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            out.push((format!("/items/{i}/text"), item.text.as_str()));
            for (name, v) in [("before", &item.before), ("after", &item.after)] {
                if let Some(v) = v {
                    out.push((format!("/items/{i}/{name}"), v.as_str()));
                }
            }
            for (j, a) in item.anchors.iter().enumerate() {
                out.push((format!("/items/{i}/anchors/{j}/path"), a.path.as_str()));
            }
        }
        out
    }
}

/// The lines of one hunk on one side: first, last, and each line's text.
fn hunk_lines(h: &slath_git::diff::Hunk, side: Side) -> Vec<(u32, &[u8])> {
    use slath_git::diff::LineKind;
    h.lines
        .iter()
        .filter_map(|l| match (side, l.kind) {
            (_, LineKind::NoNewline) => None,
            (Side::New, LineKind::Context | LineKind::Added) => l.new_no.map(|n| (n, &l.text[..])),
            (Side::Old, LineKind::Context | LineKind::Removed) => {
                l.old_no.map(|n| (n, &l.text[..]))
            }
            _ => None,
        })
        .collect()
}

/// "1–8 and 40–52".
fn ranges(rs: &[(u32, u32)]) -> String {
    let one = |(s, e): &(u32, u32)| {
        if s == e {
            s.to_string()
        } else {
            format!("{s}–{e}")
        }
    };
    match rs {
        [] => String::new(),
        [only] => one(only),
        [rest @ .., last] => format!(
            "{} and {}",
            rest.iter().map(one).collect::<Vec<_>>().join(", "),
            one(last)
        ),
    }
}

/// Checks `a` against `diff` and records its lines' SHA-256, or says
/// what's there instead.
fn locate(diff: &slath_git::diff::Diff, a: &mut Anchor) -> std::result::Result<(), String> {
    let Some(f) = diff.files.iter().find(|f| f.path() == a.path.as_bytes()) else {
        return Err(format!("{} isn't in this diff.", a.path));
    };
    let Some((s, e)) = a.lines() else {
        return Ok(());
    };
    let side = a.side();
    let hunks: Vec<Vec<(u32, &[u8])>> = f
        .hunks
        .iter()
        .map(|h| hunk_lines(h, side))
        .filter(|l| !l.is_empty())
        .collect();
    let side_name = match side {
        Side::New => "new",
        Side::Old => "old",
    };
    for lines in &hunks {
        let (first, last) = (lines[0].0, lines[lines.len() - 1].0);
        if first <= s && e <= last {
            let picked: Vec<Vec<u8>> = lines
                .iter()
                .filter(|(n, _)| (s..=e).contains(n))
                .map(|(_, t)| t.to_vec())
                .collect();
            a.line_hash = Some(crate::mr_threads::hash_lines(&picked));
            return Ok(());
        }
    }
    let (what, are) = if s == e {
        (format!("Line {s}"), "isn't")
    } else {
        (format!("Lines {s}–{e}"), "aren't")
    };
    if hunks.is_empty() {
        return Err(format!(
            "{what} of {} ({side_name}) {are} in this diff: it shows no {side_name} lines of the file; point at the whole file.",
            a.path
        ));
    }
    let shown: Vec<(u32, u32)> = hunks.iter().map(|l| (l[0].0, l[l.len() - 1].0)).collect();
    Err(format!(
        "{what} of {} ({side_name}) {are} in this diff; its hunks there show lines {}.",
        a.path,
        ranges(&shown)
    ))
}

/// The items of a stored brief (checked when they were written).
#[must_use]
pub fn stored_items(v: &serde_json::Value) -> Vec<Item> {
    serde_json::from_value(v.clone()).unwrap_or_default()
}

/// Each item with its name on the page ("Effect 2": numbered within its
/// kind, in the author's order).
#[must_use]
pub fn numbered(items: &[Item]) -> Vec<(usize, String)> {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let n = items[..=i].iter().filter(|x| x.kind == item.kind).count();
            let others = items.iter().filter(|x| x.kind == item.kind).count();
            if others > 1 {
                (i, format!("{} {n}", item.kind.label()))
            } else {
                (i, item.kind.label().to_owned())
            }
        })
        .collect()
}

/// How an item of one brief compares with another brief's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compared {
    /// The same item in both (indexes in the old and the new brief).
    Same(usize, usize),
    /// Changed: an item of the same kind between the same unchanged ones.
    Updated(usize, usize),
    New(usize),
    Removed(usize),
}

/// Compares two briefs' items: unchanged items by a longest common
/// subsequence, and between two unchanged ones, a removed and an added
/// item of the same kind (in order) as one updated item. In the new
/// brief's order, removed items where they were.
#[must_use]
pub fn compare(old: &[Item], new: &[Item]) -> Vec<Compared> {
    let (n, m) = (old.len(), new.len());
    // lcs[i][j]: the longest common subsequence of old[i..] and new[j..].
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    let flush = |out: &mut Vec<Compared>, removed: &mut Vec<usize>, added: &mut Vec<usize>| {
        let mut left: Vec<Option<usize>> = removed.drain(..).map(Some).collect();
        for j in added.drain(..) {
            match left
                .iter_mut()
                .find(|r| r.is_some_and(|i| old[i].kind == new[j].kind))
                .and_then(Option::take)
            {
                Some(i) => out.push(Compared::Updated(i, j)),
                None => out.push(Compared::New(j)),
            }
        }
        out.extend(left.into_iter().flatten().map(Compared::Removed));
    };
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && old[i] == new[j] {
            flush(&mut out, &mut removed, &mut added);
            out.push(Compared::Same(i, j));
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            added.push(j);
            j += 1;
        } else {
            removed.push(i);
            i += 1;
        }
    }
    flush(&mut out, &mut removed, &mut added);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<Upload> {
        Upload::parse(json.as_bytes())
    }

    fn pointers(e: ServiceError) -> Vec<String> {
        match e {
            ServiceError::Invalid(v) => v.into_iter().map(|f| f.field).collect(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn briefs_need_the_schema_and_an_item() {
        assert!(parse(r#"{"schema":"slath-brief/2","items":[],"extra":1}"#).is_err());
        let old = parse(r#"{"schema":"slath-brief/1","intent":"x"}"#).unwrap_err();
        assert!(matches!(old, ServiceError::Invalid(_)));
        let wrong = parse(r#"{"schema":"slath-brief/3","items":[]}"#).unwrap_err();
        assert_eq!(pointers(wrong), ["/schema"]);
        let none = parse(r#"{"schema":"slath-brief/2","items":[]}"#).unwrap_err();
        assert_eq!(pointers(none), ["/items"]);
        assert!(
            parse(r#"{"schema":"slath-brief/2","items":[{"kind":"wish","text":"x"}]}"#).is_err()
        );
        assert!(
            parse(r#"{"schema":"slath-brief/2","items":[{"kind":"risk","text":"x","y":1}]}"#)
                .is_err()
        );
    }

    #[test]
    fn items_are_checked_by_pointer_and_tidied() {
        let e = parse(
            r#"{"schema":"slath-brief/2","items":[
                {"kind":"motivation","text":"why"},
                {"kind":"risk","text":"  ","before":"b"},
                {"kind":"effect","text":"faster  \n","before":"slow","after":""}]}"#,
        )
        .unwrap_err();
        assert_eq!(pointers(e), ["/items/1/text", "/items/1/before"]);
        let ok = parse(
            r#"{"schema":"slath-brief/2","items":[
                {"kind":"effect","text":"faster  \n","before":"slow","after":""}]}"#,
        )
        .unwrap();
        assert_eq!(ok.items[0].text, "faster");
        assert_eq!(ok.items[0].after, None);
        assert_eq!(ok.texts().len(), 2);
        let long = format!(
            r#"{{"schema":"slath-brief/2","items":[{{"kind":"gap","text":"{}"}}]}}"#,
            "a".repeat(TEXT_MAX_BYTES + 1)
        );
        assert_eq!(pointers(parse(&long).unwrap_err()), ["/items/0/text"]);
    }

    #[test]
    fn items_are_numbered_within_their_kind() {
        let item = |kind, text: &str| Item {
            kind,
            text: text.to_owned(),
            before: None,
            after: None,
            anchors: Vec::new(),
        };
        let items = [
            item(Kind::Effect, "a"),
            item(Kind::Motivation, "m"),
            item(Kind::Effect, "b"),
        ];
        let names: Vec<String> = numbered(&items).into_iter().map(|(_, n)| n).collect();
        assert_eq!(names, ["Effect 1", "Motivation", "Effect 2"]);
        // A thread's hash covers before and after.
        let mut e = item(Kind::Effect, "a");
        let plain = e.hash();
        e.before = Some("slow".into());
        assert_ne!(e.hash(), plain);
        assert_eq!(e.lines(), ["a", "", "Before:", "slow"]);
    }

    #[test]
    fn briefs_compare_item_by_item() {
        let item = |kind, text: &str| Item {
            kind,
            text: text.to_owned(),
            before: None,
            after: None,
            anchors: Vec::new(),
        };
        let old = [
            item(Kind::Motivation, "why"),
            item(Kind::Effect, "slow"),
            item(Kind::Risk, "gone"),
            item(Kind::Verification, "tests"),
        ];
        let new = [
            item(Kind::Motivation, "why"),
            item(Kind::Effect, "fast"),
            item(Kind::Gap, "docs"),
            item(Kind::Verification, "tests"),
        ];
        assert_eq!(
            compare(&old, &new),
            [
                Compared::Same(0, 0),
                Compared::Updated(1, 1),
                Compared::New(2),
                Compared::Removed(2),
                Compared::Same(3, 3),
            ]
        );
        assert_eq!(compare(&old, &old).len(), 4);
        assert_eq!(compare(&[], &new[..1]), [Compared::New(0)]);
        assert_eq!(compare(&old[..1], &[]), [Compared::Removed(0)]);
    }

    #[test]
    fn anchors_are_written_and_read_in_one_notation() {
        for text in [
            "src/a.rs",
            "src/a.rs:10",
            "src/a.rs:10-20",
            "src/a.rs:old 3-4",
            "a:b.rs",
        ] {
            assert_eq!(Anchor::parse(text).unwrap().notation(), text);
        }
        let a = Anchor::parse(" dir/x y.txt : old 7 - 9 ").unwrap();
        assert_eq!(
            (a.path.as_str(), a.side(), a.lines()),
            ("dir/x y.txt", Side::Old, Some((7, 9)))
        );
        assert_eq!(Anchor::parse("x:old y"), None);
        assert_eq!(Anchor::parse("  "), None);
    }

    #[test]
    fn anchor_shapes_are_checked_by_pointer() {
        let e = parse(
            r#"{"schema":"slath-brief/2","items":[{"kind":"risk","text":"x","anchors":[
                {"path":"a"},{"path":" "},{"path":"a","start":0},{"path":"a","start":5,"end":4},
                {"path":"a","start":1,"end":600},{"path":"a","end":3},{"path":"a","line_hash":"x"}]}]}"#,
        )
        .unwrap_err();
        assert_eq!(
            pointers(e),
            (1..=6)
                .map(|j| format!("/items/0/anchors/{j}"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn anchors_must_be_in_the_diff_and_keep_their_lines_hash() {
        let raw = b"diff --git a/a.txt b/a.txt
index 1111111..2222222 100644
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,4 @@
 one
-two
+TWO
+three
 four
@@ -10,2 +11,2 @@ ctx
 ten
-eleven
+ELEVEN
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 3333333..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
";
        let diff = slath_git::diff::parse(raw, 100, 10_000);
        let brief = |anchors: &str| {
            Upload::parse(
                format!(
                    r#"{{"schema":"slath-brief/2","items":[{{"kind":"effect","text":"x","anchors":[{anchors}]}}]}}"#
                )
                .as_bytes(),
            )
            .unwrap()
        };
        let mut ok = brief(
            r#"{"path":"a.txt","start":2,"end":3},{"path":"a.txt"},{"path":"a.txt","side":"old","start":2},{"path":"gone.txt","side":"old","start":1},{"path":"gone.txt"}"#,
        );
        ok.locate(&diff).unwrap();
        let a = &ok.items[0].anchors;
        assert_eq!(
            a[0].line_hash.as_deref(),
            Some(crate::mr_threads::hash_lines(&[b"TWO".to_vec(), b"three".to_vec()]).as_str())
        );
        assert_eq!(a[1].line_hash, None);
        assert!(a[2].line_hash.is_some());
        let mut bad = brief(
            r#"{"path":"nope.txt"},{"path":"a.txt","start":5,"end":12},{"path":"gone.txt","start":1}"#,
        );
        let ServiceError::Invalid(errors) = bad.locate(&diff).unwrap_err() else {
            panic!()
        };
        let msgs: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(
            msgs,
            [
                "nope.txt isn't in this diff.",
                "Lines 5–12 of a.txt (new) aren't in this diff; its hunks there show lines 1–4 and 11–12.",
                "Line 1 of gone.txt (new) isn't in this diff: it shows no new lines of the file; point at the whole file.",
            ]
        );
        assert_eq!(errors[1].field, "/items/0/anchors/1");
    }
}
