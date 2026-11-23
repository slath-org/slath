// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Agent context records, schema `slath-context/1`: an agent's own account
//! of how it made a change (what it was asked, what it did, what's left),
//! uploaded next to the change for its reviewers. Everything in a record is
//! claimed by its uploader and shown as untrusted text.
//!
//! This module holds the schema and its checks; storing, binding, and
//! access are the service's.

use serde::{Deserialize, Serialize};

/// The only schema this release accepts.
pub const SCHEMA: &str = "slath-context/1";

/// Longest agent name, model, or tool.
pub const NAME_MAX: usize = 200;
/// Longest summary field and open item.
pub const SUMMARY_FIELD_MAX: usize = 16 * 1024;
pub const OPEN_ITEMS_MAX: usize = 50;
pub const ARTIFACTS_MAX: usize = 100;
pub const REDACTIONS_MAX: usize = 100;

/// A record as uploaded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upload {
    pub schema: String,
    pub agent: Agent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiated_by: Option<String>,
    pub summary: Summary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transcript: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<Redaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<Binding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub intent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_items: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Prompt,
    Response,
    ToolCall,
    ToolResult,
}

/// One transcript entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub kind: EntryKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// When the agent says it happened (RFC 3339, not checked further).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

/// A digest of a file the agent produced; never its content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub name: String,
    pub sha256: String,
}

/// How often the agent redacted something by one rule; never the values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Redaction {
    pub rule: String,
    pub count: u32,
}

/// What the record is about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_request: Option<i32>,
    /// A change's display ID in jj's format, within `merge_request`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// What is stored encrypted: everything but the agent, the initiating
/// user, and the binding, which are metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Payload {
    pub summary: Summary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transcript: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<Redaction>,
}

/// A repository's detail level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Summary,
    Full,
}

impl Detail {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Full => "full",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "summary" => Some(Self::Summary),
            "full" => Some(Self::Full),
            _ => None,
        }
    }
}

/// A rejected field: its JSON pointer and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    pub field: String,
    pub message: String,
}

fn invalid(field: impl Into<String>, message: impl Into<String>) -> Invalid {
    Invalid {
        field: field.into(),
        message: message.into(),
    }
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn text(out: &mut Vec<Invalid>, field: &str, value: &str, max: usize, required: bool) {
    if required && value.trim().is_empty() {
        out.push(invalid(field, "Fill this in."));
    } else if value.len() > max {
        out.push(invalid(
            field,
            format!("Use at most {} KiB.", max.div_ceil(1024)),
        ));
    } else if value.contains('\0') {
        out.push(invalid(field, "Remove the NUL character."));
    }
}

fn name(out: &mut Vec<Invalid>, field: &str, value: &str, required: bool) {
    if required && value.trim().is_empty() {
        out.push(invalid(field, "Fill this in."));
    } else if value.len() > NAME_MAX || value.chars().any(char::is_control) {
        out.push(invalid(
            field,
            format!("Use at most {NAME_MAX} bytes on one line."),
        ));
    }
}

impl Upload {
    /// Checks everything that doesn't need the server's state; returns
    /// every problem, not just the first. `transcript_max` is the limit on
    /// entries; `full` says whether the repository keeps transcripts.
    pub fn check(&self, full: bool, transcript_max: usize) -> Result<(), Vec<Invalid>> {
        let mut out = Vec::new();
        if self.schema != SCHEMA {
            out.push(invalid(
                "/schema",
                format!("Use the schema \"{SCHEMA}\"; this server knows no other."),
            ));
        }
        name(&mut out, "/agent/name", &self.agent.name, true);
        if let Some(m) = &self.agent.model {
            name(&mut out, "/agent/model", m, false);
        }
        if let Some(t) = &self.agent.tool {
            name(&mut out, "/agent/tool", t, false);
        }
        if let Some(u) = &self.initiated_by {
            name(&mut out, "/initiated_by", u, true);
        }
        let s = &self.summary;
        text(
            &mut out,
            "/summary/intent",
            &s.intent,
            SUMMARY_FIELD_MAX,
            true,
        );
        if let Some(o) = &s.outcome {
            text(&mut out, "/summary/outcome", o, SUMMARY_FIELD_MAX, false);
        }
        if s.open_items.len() > OPEN_ITEMS_MAX {
            out.push(invalid(
                "/summary/open_items",
                format!("List at most {OPEN_ITEMS_MAX} open items."),
            ));
        }
        for (i, item) in s.open_items.iter().enumerate() {
            text(
                &mut out,
                &format!("/summary/open_items/{i}"),
                item,
                SUMMARY_FIELD_MAX,
                true,
            );
        }
        if !self.transcript.is_empty() && !full {
            out.push(invalid(
                "/transcript",
                "This repository keeps summaries only; leave the transcript out.",
            ));
        } else if self.transcript.len() > transcript_max {
            out.push(invalid(
                "/transcript",
                format!("Send at most {transcript_max} transcript entries."),
            ));
        }
        for (i, e) in self.transcript.iter().enumerate() {
            if e.text.contains('\0') {
                out.push(invalid(
                    format!("/transcript/{i}/text"),
                    "Remove the NUL character.",
                ));
            }
            if let Some(t) = &e.tool {
                name(&mut out, &format!("/transcript/{i}/tool"), t, false);
            }
            if let Some(a) = &e.at {
                name(&mut out, &format!("/transcript/{i}/at"), a, false);
            }
        }
        if self.artifacts.len() > ARTIFACTS_MAX {
            out.push(invalid(
                "/artifacts",
                format!("List at most {ARTIFACTS_MAX} artifacts."),
            ));
        }
        for (i, a) in self.artifacts.iter().enumerate() {
            name(&mut out, &format!("/artifacts/{i}/name"), &a.name, true);
            if !is_hex(&a.sha256, 64) {
                out.push(invalid(
                    format!("/artifacts/{i}/sha256"),
                    "Use a lowercase hex SHA-256.",
                ));
            }
        }
        if self.redactions.len() > REDACTIONS_MAX {
            out.push(invalid(
                "/redactions",
                format!("List at most {REDACTIONS_MAX} rules."),
            ));
        }
        for (i, r) in self.redactions.iter().enumerate() {
            name(&mut out, &format!("/redactions/{i}/rule"), &r.rule, true);
        }
        if let Some(b) = &self.binding {
            match (b.merge_request, &b.change, &b.commit) {
                (Some(n), _, None) if n > 0 => {}
                (None, None, Some(_)) => {}
                (Some(_), _, None) => {
                    out.push(invalid("/binding/merge_request", "Use a positive number."));
                }
                _ => out.push(invalid(
                    "/binding",
                    "Name a merge request (and optionally its change), or a commit.",
                )),
            }
            if let Some(c) = &b.change
                && !(c.len() == 32 && c.bytes().all(|b| (b'k'..=b'z').contains(&b)))
            {
                out.push(invalid(
                    "/binding/change",
                    "Use the change's ID as jj shows it (32 letters k-z).",
                ));
            }
            if let Some(c) = &b.commit
                && !is_hex(c, 40)
            {
                out.push(invalid("/binding/commit", "Use a full commit ID."));
            }
        }
        if out.is_empty() { Ok(()) } else { Err(out) }
    }

    /// The part stored encrypted.
    #[must_use]
    pub fn payload(&self) -> Payload {
        Payload {
            summary: self.summary.clone(),
            transcript: self.transcript.clone(),
            artifacts: self.artifacts.clone(),
            redactions: self.redactions.clone(),
        }
    }

    /// Every text field with its JSON pointer, for the secret scanner.
    #[must_use]
    pub fn texts(&self) -> Vec<(String, &str)> {
        let mut v: Vec<(String, &str)> = vec![
            ("/agent/name".into(), &self.agent.name),
            ("/summary/intent".into(), &self.summary.intent),
        ];
        v.extend(
            self.agent
                .model
                .as_deref()
                .map(|m| ("/agent/model".into(), m)),
        );
        v.extend(
            self.agent
                .tool
                .as_deref()
                .map(|t| ("/agent/tool".into(), t)),
        );
        v.extend(
            self.summary
                .outcome
                .as_deref()
                .map(|o| ("/summary/outcome".into(), o)),
        );
        for (i, item) in self.summary.open_items.iter().enumerate() {
            v.push((format!("/summary/open_items/{i}"), item));
        }
        for (i, e) in self.transcript.iter().enumerate() {
            v.push((format!("/transcript/{i}/text"), &e.text));
            if let Some(t) = &e.tool {
                v.push((format!("/transcript/{i}/tool"), t));
            }
        }
        for (i, a) in self.artifacts.iter().enumerate() {
            v.push((format!("/artifacts/{i}/name"), &a.name));
        }
        for (i, r) in self.redactions.iter().enumerate() {
            v.push((format!("/redactions/{i}/rule"), &r.rule));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(json: &str) -> Upload {
        serde_json::from_str(json).unwrap()
    }

    const MINIMAL: &str = r#"{"schema":"slath-context/1","agent":{"name":"coder"},
        "summary":{"intent":"Fix the login redirect"}}"#;

    #[test]
    fn a_minimal_record_passes_and_unknown_keys_are_refused() {
        assert_eq!(upload(MINIMAL).check(false, 10), Ok(()));
        for bad in [
            r#"{"schema":"slath-context/1","agent":{"name":"a"},"summary":{"intent":"x"},"extra":1}"#,
            r#"{"schema":"slath-context/1","agent":{"name":"a","x":1},"summary":{"intent":"x"}}"#,
            r#"{"schema":"slath-context/1","agent":{"name":"a"},"summary":{"intent":"x"},
                "redactions":[{"rule":"r","count":1,"value":"hunter2"}]}"#,
        ] {
            assert!(serde_json::from_str::<Upload>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_problem_is_named_by_its_pointer() {
        let u = upload(
            r#"{"schema":"slath-context/2","agent":{"name":" ","model":"a\nb"},
            "summary":{"intent":"","open_items":["ok",""]},
            "transcript":[{"kind":"prompt","text":"hi"}],
            "artifacts":[{"name":"out.txt","sha256":"ABC"}],
            "binding":{"merge_request":1,"commit":"00"}}"#,
        );
        let fields: Vec<String> = u
            .check(false, 10)
            .unwrap_err()
            .into_iter()
            .map(|i| i.field)
            .collect();
        assert_eq!(
            fields,
            [
                "/schema",
                "/agent/name",
                "/agent/model",
                "/summary/intent",
                "/summary/open_items/1",
                "/transcript",
                "/artifacts/0/sha256",
                "/binding",
                "/binding/commit",
            ]
        );
    }

    #[test]
    fn transcripts_need_the_full_detail_level_and_fit_the_limit() {
        let u = upload(
            r#"{"schema":"slath-context/1","agent":{"name":"a"},"summary":{"intent":"x"},
            "transcript":[{"kind":"prompt","text":"a"},{"kind":"tool_call","text":"b","tool":"shell"}]}"#,
        );
        assert!(u.check(false, 10).is_err());
        assert_eq!(u.check(true, 10), Ok(()));
        assert_eq!(u.check(true, 1).unwrap_err()[0].field, "/transcript");
        let p = u.payload();
        assert_eq!(p.transcript.len(), 2);
        assert_eq!(p.transcript[1].kind, EntryKind::ToolCall);
    }

    #[test]
    fn bindings_name_a_merge_request_or_a_commit() {
        let ok = |b: &str| {
            upload(&MINIMAL.replace("}}", &format!("}},\"binding\":{b}}}")))
                .check(false, 1)
                .is_ok()
        };
        assert!(ok(r#"{"merge_request":3}"#));
        assert!(ok(&format!(
            r#"{{"merge_request":3,"change":"{}"}}"#,
            "k".repeat(32)
        )));
        assert!(ok(&format!(r#"{{"commit":"{}"}}"#, "a".repeat(40))));
        assert!(!ok(r#"{"merge_request":0}"#));
        assert!(!ok(r#"{}"#));
        assert!(!ok(&format!(r#"{{"change":"{}"}}"#, "k".repeat(32))));
        assert!(!ok(r#"{"merge_request":3,"change":"abc"}"#));
    }

    #[test]
    fn texts_cover_every_scanned_field() {
        let u = upload(
            r#"{"schema":"slath-context/1","agent":{"name":"a","model":"m"},
            "summary":{"intent":"i","outcome":"o","open_items":["x"]},
            "transcript":[{"kind":"tool_result","text":"t","tool":"shell"}]}"#,
        );
        let ptrs: Vec<String> = u.texts().into_iter().map(|(p, _)| p).collect();
        for p in [
            "/agent/name",
            "/agent/model",
            "/summary/intent",
            "/summary/outcome",
            "/summary/open_items/0",
            "/transcript/0/text",
            "/transcript/0/tool",
        ] {
            assert!(ptrs.iter().any(|x| x == p), "{p}");
        }
    }
}
