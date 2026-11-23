// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Context panel of a merge request's pages: agents' own records of
//! how they made its changes, for the people who may see them. Everything
//! in a record but its uploader and time is the agent's claim, shown as
//! plain text.

use slath_core::authz::Subject;
use slath_core::context_record::EntryKind;
use slath_core::ids::MergeRequestId;
use slath_service::repos::RepoAccess;

use super::{Time, Who};
use crate::AppState;
use crate::error::AppError;

/// One transcript entry.
pub struct EntryRow {
    pub label: &'static str,
    pub tool: Option<String>,
    pub text: String,
}

/// One record as the panel shows it.
pub struct RecordRow {
    pub uploader: Who,
    pub created: Time,
    /// "coder · some-model · cli".
    pub agent: String,
    pub initiated_by: Option<String>,
    /// The change it's bound to: its short display ID and page.
    pub change: Option<(String, String)>,
    pub intent: String,
    pub outcome: Option<String>,
    pub open_items: Vec<String>,
    pub transcript: Vec<EntryRow>,
}

const fn label(k: EntryKind) -> &'static str {
    match k {
        EntryKind::Prompt => "Prompt",
        EntryKind::Response => "Response",
        EntryKind::ToolCall => "Tool call",
        EntryKind::ToolResult => "Tool result",
    }
}

/// The records of merge request `m` the subject may see, oldest first;
/// with `change`, only those bound to that change (its display ID).
pub async fn panel(
    state: &AppState,
    subject: &Subject,
    a: &RepoAccess,
    (m, number): (MergeRequestId, i32),
    change: Option<&str>,
) -> Result<Vec<RecordRow>, AppError> {
    let records = state.services.mr_context_records(subject, a, m).await?;
    let base = format!(
        "/{}/{}/merge-requests/{number}/changes",
        a.repo.namespace, a.repo.name
    );
    Ok(records
        .into_iter()
        .filter(|v| change.is_none_or(|c| v.change.as_deref() == Some(c)))
        .map(|v| {
            let r = v.record;
            let agent = [Some(r.agent_name), r.agent_model, r.agent_tool]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            let p = v
                .payload
                .unwrap_or_else(|| slath_core::context_record::Payload {
                    summary: slath_core::context_record::Summary {
                        intent: String::new(),
                        outcome: None,
                        open_items: Vec::new(),
                    },
                    transcript: Vec::new(),
                    artifacts: Vec::new(),
                    redactions: Vec::new(),
                });
            RecordRow {
                uploader: Who::account(v.uploader),
                created: r.created_at.into(),
                agent,
                initiated_by: v.initiated_by,
                change: v.change.filter(|_| change.is_none()).map(|d| {
                    let short = d.chars().take(8).collect();
                    (short, format!("{base}/{d}"))
                }),
                intent: p.summary.intent,
                outcome: p.summary.outcome,
                open_items: p.summary.open_items,
                transcript: p
                    .transcript
                    .into_iter()
                    .map(|e| EntryRow {
                        label: label(e.kind),
                        tool: e.tool,
                        text: e.text,
                    })
                    .collect(),
            }
        })
        .collect())
}
