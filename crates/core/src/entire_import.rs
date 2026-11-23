// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The experimental import of one Entire CLI checkpoint session into an
//! agent context record. It reads what the Entire CLI 0.13 writes for a
//! session: its `metadata.json`, its agent-neutral `transcript.jsonl`
//! (lines of version 1), and its `prompt.txt`. Entire's files carry no
//! schema version and may change; unknown fields are ignored, and anything
//! that doesn't fit is refused by the record's own checks afterwards.
//!
//! Everything in a checkpoint is the uploader's claim.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::context_record::{
    Agent, Binding, Entry, EntryKind, OPEN_ITEMS_MAX, REDACTIONS_MAX, Redaction, SCHEMA,
    SUMMARY_FIELD_MAX, Summary, Upload,
};

/// The import request: one session of a checkpoint.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Import {
    /// The session's `metadata.json`.
    pub session: Session,
    /// The session's `transcript.jsonl`, as text.
    #[serde(default)]
    pub transcript: Option<String>,
    /// The session's `prompt.txt`.
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub initiated_by: Option<String>,
    #[serde(default)]
    pub binding: Option<Binding>,
}

/// The fields of a session's `metadata.json` the import uses.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub checkpoint_id: String,
    #[serde(default)]
    pub cli_version: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub compact_transcript_start: Option<usize>,
    #[serde(default)]
    pub summary: Option<EntireSummary>,
    #[serde(default)]
    pub redaction: Option<Counts>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EntireSummary {
    #[serde(default)]
    pub intent: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub open_items: Vec<String>,
}

/// Entire's redaction counts: per layer and per kind of personal data.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Counts {
    #[serde(default)]
    pub layers: BTreeMap<String, u32>,
    #[serde(default)]
    pub pii: BTreeMap<String, u32>,
}

/// Separator of the prompts in `prompt.txt`.
const PROMPT_SEPARATOR: &str = "\n\n---\n\n";

fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

fn nonempty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The transcript entries of the checkpoint's part of `jsonl`: user text
/// as prompts, assistant text as responses, and each tool use as a call
/// and, if it has one, its result. Lines that aren't version 1 objects,
/// and blocks of other kinds, are skipped.
fn entries(jsonl: &str, start: usize) -> Vec<Entry> {
    let mut out = Vec::new();
    for line in jsonl.lines().skip(start) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v["v"] != 1 {
            continue;
        }
        let at = v["ts"].as_str().map(str::to_owned);
        let user = v["type"] == "user";
        if !user && v["type"] != "assistant" {
            continue;
        }
        let blocks = match &v["content"] {
            serde_json::Value::String(s) => vec![serde_json::json!({"text": s})],
            serde_json::Value::Array(a) => a.clone(),
            _ => Vec::new(),
        };
        for b in blocks {
            let kind = b["type"].as_str();
            match (user, kind) {
                (true, None | Some("text")) | (false, Some("text")) => {
                    if let Some(t) = b["text"].as_str().filter(|t| !t.trim().is_empty()) {
                        out.push(Entry {
                            kind: if user {
                                EntryKind::Prompt
                            } else {
                                EntryKind::Response
                            },
                            text: t.to_owned(),
                            tool: None,
                            at: at.clone(),
                        });
                    }
                }
                (false, Some("tool_use")) => {
                    let tool = b["name"].as_str().map(str::to_owned);
                    let input = match &b["input"] {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Null => String::new(),
                        other => other.to_string(),
                    };
                    out.push(Entry {
                        kind: EntryKind::ToolCall,
                        text: input,
                        tool: tool.clone(),
                        at: at.clone(),
                    });
                    if let Some(o) = b["result"]["output"].as_str() {
                        out.push(Entry {
                            kind: EntryKind::ToolResult,
                            text: o.to_owned(),
                            tool,
                            at: at.clone(),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    out
}

impl Import {
    /// The record this session becomes. `full` says whether the
    /// repository keeps transcripts; without, the transcript is left out.
    #[must_use]
    pub fn to_upload(&self, full: bool) -> Upload {
        let s = &self.session;
        let prompts: Vec<&str> = self
            .prompt
            .as_deref()
            .map(|p| {
                p.split(PROMPT_SEPARATOR)
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let summary = s.summary.clone().unwrap_or_default();
        let intent = nonempty(Some(&summary.intent))
            .or_else(|| prompts.first().map(|p| (*p).to_owned()))
            .unwrap_or_else(|| {
                format!("Entire checkpoint {}", s.checkpoint_id.trim())
                    .trim_end()
                    .to_owned()
            });
        let mut redactions: Vec<Redaction> = s
            .redaction
            .iter()
            .flat_map(|c| {
                c.layers
                    .iter()
                    .map(|(k, n)| (format!("entire-{k}"), *n))
                    .chain(
                        c.pii
                            .iter()
                            .map(|(k, n)| (format!("entire-pii-{}", k.to_lowercase()), *n)),
                    )
                    .collect::<Vec<_>>()
            })
            .filter(|(_, n)| *n > 0)
            .map(|(rule, count)| Redaction {
                rule: cut(&rule, crate::context_record::NAME_MAX),
                count,
            })
            .collect();
        redactions.truncate(REDACTIONS_MAX);
        let transcript = match (&self.transcript, full) {
            (Some(t), true) => entries(t, s.compact_transcript_start.unwrap_or(0)),
            _ => Vec::new(),
        };
        let tool = match nonempty(s.cli_version.as_deref()) {
            Some(v) => format!("entire {v}"),
            None => "entire".to_owned(),
        };
        Upload {
            schema: SCHEMA.to_owned(),
            agent: Agent {
                name: nonempty(s.agent.as_deref()).unwrap_or_else(|| "Unknown".to_owned()),
                model: nonempty(s.model.as_deref()),
                tool: Some(tool),
            },
            initiated_by: self.initiated_by.clone(),
            summary: Summary {
                intent: cut(&intent, SUMMARY_FIELD_MAX),
                outcome: nonempty(Some(&summary.outcome)).map(|o| cut(&o, SUMMARY_FIELD_MAX)),
                open_items: summary
                    .open_items
                    .iter()
                    .filter_map(|i| nonempty(Some(i)))
                    .map(|i| cut(&i, SUMMARY_FIELD_MAX))
                    .take(OPEN_ITEMS_MAX)
                    .collect(),
            },
            transcript,
            artifacts: Vec::new(),
            redactions,
            binding: self.binding.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = r#"{
        "cli_version": "0.13.4", "checkpoint_id": "a1b2c3d4e5f6", "session_id": "s-1",
        "strategy": "manual-commit", "created_at": "2026-11-20T10:00:00Z",
        "agent": "Claude Code", "model": "some-model", "checkpoints_count": 2,
        "files_touched": ["src/lib.rs"], "compact_transcript_start": 1,
        "summary": {"intent": "Fix the parser", "outcome": "Fixed; added tests",
            "friction": ["slow tests"], "open_items": ["Docs", ""], "learnings": {"repo": []}},
        "redaction": {"replacements": 3, "layers": {"entropy": 2, "provider_token": 1, "custom": 0},
            "pii": {"EMAIL": 1}},
        "token_usage": {"input_tokens": 10}
    }"#;

    const TRANSCRIPT: &str = concat!(
        r#"{"v":1,"type":"user","ts":"t0","content":[{"text":"Earlier checkpoint"}]}"#,
        "\n",
        r#"{"v":1,"type":"user","ts":"t1","content":[{"text":"Fix the parser"}]}"#,
        "\n",
        r#"{"v":1,"type":"assistant","ts":"t2","content":[{"type":"thinking","text":"hidden"},{"type":"text","text":"Looking."},{"type":"tool_use","id":"x","name":"Bash","input":{"command":"cargo test"},"result":{"output":"ok","status":"success"}}]}"#,
        "\n",
        r#"{"v":2,"type":"user","content":[{"text":"future format"}]}"#,
        "\n",
        "not json\n",
    );

    fn import(transcript: bool, prompt: Option<&str>) -> Import {
        Import {
            session: serde_json::from_str(SESSION).unwrap(),
            transcript: transcript.then(|| TRANSCRIPT.to_owned()),
            prompt: prompt.map(str::to_owned),
            initiated_by: Some("alice".into()),
            binding: None,
        }
    }

    #[test]
    fn a_session_becomes_a_record() {
        let u = import(true, None).to_upload(true);
        assert_eq!(u.agent.name, "Claude Code");
        assert_eq!(u.agent.model.as_deref(), Some("some-model"));
        assert_eq!(u.agent.tool.as_deref(), Some("entire 0.13.4"));
        assert_eq!(u.summary.intent, "Fix the parser");
        assert_eq!(u.summary.open_items, ["Docs"]);
        let rules: Vec<(&str, u32)> = u
            .redactions
            .iter()
            .map(|r| (r.rule.as_str(), r.count))
            .collect();
        assert_eq!(
            rules,
            [
                ("entire-entropy", 2),
                ("entire-provider_token", 1),
                ("entire-pii-email", 1)
            ]
        );
        // Only this checkpoint's part, without thinking or other versions.
        let kinds: Vec<(EntryKind, &str)> = u
            .transcript
            .iter()
            .map(|e| (e.kind, e.text.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                (EntryKind::Prompt, "Fix the parser"),
                (EntryKind::Response, "Looking."),
                (EntryKind::ToolCall, r#"{"command":"cargo test"}"#),
                (EntryKind::ToolResult, "ok"),
            ]
        );
        assert_eq!(u.transcript[2].tool.as_deref(), Some("Bash"));
        assert_eq!(u.check(true, 100), Ok(()));
    }

    #[test]
    fn without_full_detail_the_transcript_is_left_out() {
        let u = import(true, None).to_upload(false);
        assert!(u.transcript.is_empty());
        assert_eq!(u.check(false, 100), Ok(()));
    }

    #[test]
    fn without_a_summary_the_first_prompt_or_the_id_says_what_it_was() {
        let mut i = import(false, Some("Make it fast\n\n---\n\nAnd tested"));
        i.session.summary = None;
        assert_eq!(i.to_upload(false).summary.intent, "Make it fast");
        i.prompt = None;
        assert_eq!(
            i.to_upload(false).summary.intent,
            "Entire checkpoint a1b2c3d4e5f6"
        );
        i.session.agent = None;
        assert_eq!(i.to_upload(false).agent.name, "Unknown");
    }

    #[test]
    fn unknown_request_keys_are_refused_but_unknown_entire_fields_are_not() {
        assert!(serde_json::from_str::<Import>(r#"{"session":{},"extra":1}"#).is_err());
        let i: Import = serde_json::from_str(r#"{"session":{"new_field":true}}"#).unwrap();
        assert_eq!(i.to_upload(false).summary.intent, "Entire checkpoint");
    }
}
