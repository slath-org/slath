// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Agents' context records in the UI: the Context panel of a merge
//! request's pages (each record's summary and the start of its
//! transcript) and a record's own page in the repository
//! (`/{ns}/{repo}/-/context/{id}`), with the whole transcript in pages.
//! Everything in a record but its uploader and time is the agent's claim,
//! shown as plain text.

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use serde::Deserialize;
use slath_core::authz::Subject;
use slath_core::context_record::{EntryKind, Payload};
use slath_core::ids::MergeRequestId;
use slath_service::context_records::ContextRecord;
use slath_service::repos::RepoAccess;

use super::repos::RepoHeader;
use super::{PageCtx, Time, Who, render};
use crate::AppState;
use crate::auth::MaybeSession;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// The longest excerpt of a transcript the panel shows.
const EXCERPT_CHARS: usize = 200;
/// Transcript entries per page of a record.
const ENTRIES_PER_PAGE: usize = 200;
/// Lines of an entry shown before the rest folds away.
const ENTRY_LINES: usize = 20;

const fn label(k: EntryKind) -> &'static str {
    match k {
        EntryKind::Prompt => "Prompt",
        EntryKind::Response => "Response",
        EntryKind::ToolCall => "Tool call",
        EntryKind::ToolResult => "Tool result",
    }
}

/// The agent's name, model, and tool: "coder · some-model · cli".
fn agent_line(r: &slath_db::context_records::Record) -> String {
    [
        Some(r.agent_name.clone()),
        r.agent_model.clone(),
        r.agent_tool.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

fn record_href(a: &RepoAccess, id: uuid::Uuid) -> String {
    format!("/{}/{}/-/context/{id}", a.repo.namespace, a.repo.name)
}

/// The start of `text`, at most [`EXCERPT_CHARS`] characters on one line.
fn excerpt(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= EXCERPT_CHARS {
        return flat;
    }
    let cut: String = flat.chars().take(EXCERPT_CHARS).collect();
    format!("{}…", cut.trim_end())
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
    /// The record's own page.
    pub href: String,
    /// The transcript's first prompt, shortened, and its number of entries.
    pub excerpt: Option<String>,
    pub entries: usize,
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
            let p = v.payload.unwrap_or_else(empty_payload);
            let excerpt = p
                .transcript
                .iter()
                .find(|e| e.kind == EntryKind::Prompt)
                .map(|e| excerpt(&e.text));
            RecordRow {
                uploader: Who::account(v.uploader),
                created: r.created_at.into(),
                agent: agent_line(&r),
                initiated_by: v.initiated_by,
                change: v.change.filter(|_| change.is_none()).map(|d| {
                    let short = d.chars().take(8).collect();
                    (short, format!("{base}/{d}"))
                }),
                href: record_href(a, r.id),
                excerpt,
                entries: p.transcript.len(),
                intent: p.summary.intent,
                outcome: p.summary.outcome,
                open_items: p.summary.open_items,
            }
        })
        .collect())
}

fn empty_payload() -> Payload {
    Payload {
        summary: slath_core::context_record::Summary {
            intent: String::new(),
            outcome: None,
            open_items: Vec::new(),
        },
        transcript: Vec::new(),
        artifacts: Vec::new(),
        redactions: Vec::new(),
    }
}

/// One transcript entry on a record's page: its first lines, and the rest
/// folded away when it's long.
pub struct EntryView {
    pub number: usize,
    pub label: &'static str,
    pub tool: Option<String>,
    pub at: Option<String>,
    pub head: String,
    pub rest: Option<(usize, String)>,
}

/// What the record is about, as its page says it.
pub enum BindingView {
    None,
    /// The merge request (`!n`, page) and the change (short ID, page).
    Bound((String, String), Option<(String, String)>),
    Unlinked,
}

#[derive(Template)]
#[template(path = "repo/context_record.html")]
struct RecordPage {
    ctx: PageCtx,
    header: RepoHeader,
    uploader: Who,
    created: Time,
    agent: String,
    initiated_by: Option<String>,
    detail: String,
    binding: BindingView,
    intent: String,
    outcome: Option<String>,
    open_items: Vec<String>,
    artifacts: Vec<(String, String)>,
    redactions: Vec<(String, u32)>,
    entries: Vec<EntryView>,
    /// "Entries 201–400 of 2,000", when there are several pages.
    range: Option<String>,
    total: usize,
    prev: String,
    next: String,
}

#[derive(Deserialize)]
struct PageQuery {
    page: Option<usize>,
}

fn entry_view(number: usize, e: &slath_core::context_record::Entry) -> EntryView {
    let lines: Vec<&str> = e.text.lines().collect();
    let (head, rest) = if lines.len() > ENTRY_LINES + 5 {
        (
            lines[..ENTRY_LINES].join("\n"),
            Some((lines.len() - ENTRY_LINES, lines[ENTRY_LINES..].join("\n"))),
        )
    } else {
        (e.text.clone(), None)
    };
    EntryView {
        number,
        label: label(e.kind),
        tool: e.tool.clone(),
        at: e.at.clone(),
        head,
        rest,
    }
}

fn binding_view(a: &RepoAccess, v: &ContextRecord) -> BindingView {
    match (v.record.binding_state.as_str(), v.merge_request) {
        ("bound", Some(n)) => {
            let mr = format!("/{}/{}/merge-requests/{n}", a.repo.namespace, a.repo.name);
            let change = v
                .change
                .as_ref()
                .map(|d| (d.chars().take(8).collect(), format!("{mr}/changes/{d}")));
            BindingView::Bound((format!("!{n}"), mr), change)
        }
        ("unlinked", _) => BindingView::Unlinked,
        _ => BindingView::None,
    }
}

async fn show_record(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo, id)): Path<(String, String, String)>,
    Query(q): Query<PageQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let id: uuid::Uuid = id.parse().map_err(|_| AppError::not_found())?;
    let v = state
        .services
        .context_record(&subject, &ns, &repo, id)
        .await?;
    let a = state
        .services
        .repo_access(&subject, &ns, &repo, slath_core::authz::Action::RepoRead)
        .await?;
    let binding = binding_view(&a, &v);
    let p = v.payload.clone().unwrap_or_else(empty_payload);
    let total = p.transcript.len();
    let pages = total.div_ceil(ENTRIES_PER_PAGE).max(1);
    let page = q.page.unwrap_or(1).clamp(1, pages);
    let start = (page - 1) * ENTRIES_PER_PAGE;
    let entries = p
        .transcript
        .iter()
        .enumerate()
        .skip(start)
        .take(ENTRIES_PER_PAGE)
        .map(|(i, e)| entry_view(i + 1, e))
        .collect::<Vec<_>>();
    let here = record_href(&a, id);
    let range = (pages > 1).then(|| {
        format!(
            "Entries {}–{} of {total}.",
            start + 1,
            start + entries.len()
        )
    });
    let r = &v.record;
    let page = RecordPage {
        header: RepoHeader::load(&state, &a, &subject, "merge-requests").await,
        uploader: Who::account(v.uploader.clone()),
        created: r.created_at.into(),
        agent: agent_line(r),
        initiated_by: v.initiated_by.clone(),
        detail: r.detail.clone(),
        binding,
        intent: p.summary.intent,
        outcome: p.summary.outcome,
        open_items: p.summary.open_items,
        artifacts: p
            .artifacts
            .into_iter()
            .map(|x| (x.name, x.sha256))
            .collect(),
        redactions: p
            .redactions
            .into_iter()
            .map(|x| (x.rule, x.count))
            .collect(),
        entries,
        range,
        total,
        prev: if page > 1 {
            format!("{here}?page={}", page - 1)
        } else {
            String::new()
        },
        next: if page < pages {
            format!("{here}?page={}", page + 1)
        } else {
            String::new()
        },
        ctx,
    };
    Ok(render(&page))
}

pub fn router() -> Router<AppState> {
    Router::new().route_as(
        "/{ns}/{repo}/-/context/{id}",
        Access::Resource,
        axum::routing::get(show_record),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpts_are_one_short_line() {
        assert_eq!(excerpt("Fix\n  the  bug"), "Fix the bug");
        let long = "word ".repeat(100);
        let e = excerpt(&long);
        assert!(e.ends_with('…'));
        assert_eq!(e.chars().count(), EXCERPT_CHARS);
    }

    #[test]
    fn long_entries_fold_after_their_first_lines() {
        let e = |text: String| slath_core::context_record::Entry {
            kind: EntryKind::ToolResult,
            text,
            tool: None,
            at: None,
        };
        let short = entry_view(1, &e("a\n".repeat(ENTRY_LINES + 5)));
        assert!(short.rest.is_none());
        let long = entry_view(2, &e((0..100).map(|i| format!("{i}\n")).collect()));
        assert_eq!(long.head.lines().count(), ENTRY_LINES);
        assert_eq!(long.rest.as_ref().map(|r| r.0), Some(100 - ENTRY_LINES));
    }
}
