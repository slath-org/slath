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
        }
        out
    }
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
}
