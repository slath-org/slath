// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merge request pages: list, compare and create, the conversation with its
//! timeline, commits, changes, and the actions on them.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::authz::{Action, Subject};
use slath_db::merge_requests::MergeRequest;
use slath_markup::refs::RefContext;
use slath_service::issues::TimelineItem;
use slath_service::labels::FilterInput;
use slath_service::merge_requests::{Compare, MrPerms, MrTimelineItem, NewMr};
use slath_service::mr_changes::{ChangeInfo, Interdiff, Rebind, StackEntry, VersionInfo};
use slath_service::mr_reviews::Mergeability;
use slath_service::repos::RepoAccess;
use slath_service::{FieldError, ServiceError};

use super::browse::time;
use super::diff::{DiffQuery, DiffView};
use super::repos::RepoHeader;
use super::settings::error_for;
use super::{MarkupHtml, PageCtx, Time, render};
use crate::AppState;
use crate::auth::{MaybeSession, RequireSession};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

const PER_PAGE: i64 = 50;
const TAB: &str = "merge-requests";

fn mr_url(ns: &str, repo: &str, n: i32, rest: &str) -> String {
    format!("/{ns}/{repo}/merge-requests/{n}{rest}")
}

fn number(s: &str) -> Result<i32, AppError> {
    s.parse::<i32>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(AppError::not_found)
}

use super::issues::enc;

/// Comma-separated names from a form field.
fn names(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().trim_start_matches('@').to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The icon and state shown for a merge request.
fn state_view(m: &MergeRequest) -> (&'static str, &'static str, &'static str) {
    match (m.state.as_str(), m.draft) {
        ("merged", _) => ("git-merge", "merged", "Merged"),
        ("closed", _) => ("git-pull-request-closed", "closed", "Closed"),
        (_, true) => ("git-pull-request-draft", "draft", "Draft"),
        _ => ("git-pull-request", "open", "Open"),
    }
}

/// A commit row on the compare, commits, and conversation pages.
struct CommitRow {
    oid: String,
    short: String,
    subject: String,
    author: String,
    time: Time,
}

fn commit_rows(commits: &[slath_git::browse::LogEntry]) -> Vec<CommitRow> {
    commits
        .iter()
        .map(|c| CommitRow {
            oid: c.oid.as_str().to_owned(),
            short: c.oid.as_str()[..12].to_owned(),
            subject: slath_markup::reveal_bidi(&c.subject).into_owned(),
            author: c.author_name.clone(),
            time: time(c.author_time),
        })
        .collect()
}

// ---------------------------------------------------------------- list

struct MrRow {
    labels: Vec<slath_db::labels::Label>,
    number: i32,
    title: String,
    state: &'static str,
    state_text: &'static str,
    icon: &'static str,
    author: Option<String>,
    created: Time,
    comments: i64,
    source: String,
    target: String,
}

#[derive(Template)]
#[template(path = "repo/merge_requests.html")]
struct ListPage {
    ctx: PageCtx,
    header: RepoHeader,
    items: Vec<MrRow>,
    state: String,
    open: i64,
    merged: i64,
    closed: i64,
    can_create: bool,
    next: Option<String>,
    q: ListQuery,
    query: String,
    milestones: Vec<String>,
    filtered: bool,
}

impl ListPage {
    fn label_href(&self, name: &str) -> String {
        format!(
            "/{}/{}/merge-requests?state=open&labels={}",
            self.header.namespace,
            self.header.name,
            enc(name)
        )
    }
}

#[derive(Deserialize, Default, Clone)]
struct ListQuery {
    state: Option<String>,
    page: Option<i64>,
    #[serde(default)]
    labels: String,
    #[serde(default)]
    assignee: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    milestone: String,
    #[serde(default)]
    sort: String,
}

impl ListQuery {
    fn input(&self, state: &str) -> FilterInput {
        FilterInput {
            state: Some(state.to_owned()),
            labels: self.labels.split(',').map(str::to_owned).collect(),
            assignee: Some(self.assignee.clone()),
            author: Some(self.author.clone()),
            milestone: Some(self.milestone.clone()),
            sort_updated: self.sort == "updated",
        }
    }

    fn query(&self) -> String {
        [
            ("labels", &self.labels),
            ("assignee", &self.assignee),
            ("author", &self.author),
            ("milestone", &self.milestone),
            ("sort", &self.sort),
        ]
        .iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .map(|(k, v)| format!("&{k}={}", enc(v.trim())))
        .collect()
    }
}

async fn list(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let st = match q.state.as_deref() {
        Some("merged") => "merged",
        Some("closed") => "closed",
        Some("all") => "all",
        _ => "open",
    };
    let page = q.page.unwrap_or(1).clamp(1, 10_000);
    let (a, items, (open, merged, closed)) = state
        .services
        .mr_list(
            &subject,
            &ns,
            &repo,
            &q.input(st),
            (page - 1) * PER_PAGE,
            PER_PAGE + 1,
        )
        .await?;
    let more = items.len() as i64 > PER_PAGE;
    let (_, ms) = state.services.milestones(&subject, &ns, &repo).await?;
    let mut rows = Vec::new();
    for m in items.into_iter().take(PER_PAGE as usize) {
        let (_, _, labels, _) = state.services.mr_details(&m).await?;
        let (icon, s, text) = state_view(&m);
        rows.push(MrRow {
            labels,
            number: m.number,
            icon,
            state: s,
            state_text: text,
            source: m.source_branch(),
            target: m.target_branch.clone(),
            title: m.title,
            author: m.author,
            created: m.created_at.into(),
            comments: m.comments,
        });
    }
    let query = q.query();
    Ok(render(&ListPage {
        milestones: ms.into_iter().map(|m| m.title).collect(),
        filtered: !query.is_empty(),
        header: RepoHeader::load(&state, &a, &subject, TAB)
            .await
            .with_recent_pushes(&state, &a, &subject)
            .await,
        can_create: a.can(&subject, Action::MrCreate),
        items: rows,
        state: st.to_owned(),
        open,
        merged,
        closed,
        next: more.then(|| format!("?state={st}{query}&page={}", page + 1)),
        query,
        q,
        ctx,
    }))
}

// ---------------------------------------------------------------- compare

#[derive(Template)]
#[template(path = "repo/compare.html")]
struct ComparePage {
    ctx: PageCtx,
    header: RepoHeader,
    branches: Vec<String>,
    source: String,
    target: String,
    /// The compare, once both branches are chosen and exist.
    cmp: Option<CompareView>,
    can_create: bool,
    form: CreateForm,
    milestones: Vec<String>,
    errors: Vec<FieldError>,
}

impl ComparePage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

struct CompareView {
    commits: Vec<CommitRow>,
    more_commits: bool,
    diff: DiffView,
    too_big: bool,
    /// No commits on the source that aren't on the target.
    nothing: bool,
    unrelated: bool,
    existing: Option<i32>,
}

#[derive(Deserialize, Default, Clone)]
struct CompareQuery {
    #[serde(default)]
    source: String,
    #[serde(default)]
    target: String,
}

#[derive(Deserialize, Default, Clone)]
struct CreateForm {
    #[serde(default)]
    source_branch: String,
    #[serde(default)]
    target_branch: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    draft: Option<String>,
    #[serde(default)]
    reviewers: String,
    #[serde(default)]
    assignees: String,
    #[serde(default)]
    labels: String,
    #[serde(default)]
    milestone: String,
}

/// The prefilled title and description: a single commit's message, or the
/// branch name; the repository's template wins for the description.
fn prefill(c: &Compare) -> (String, String) {
    let title = match c.commits.as_slice() {
        [one] => one.subject.clone(),
        _ => {
            let name = c
                .source_branch
                .rsplit('/')
                .next()
                .unwrap_or(&c.source_branch);
            let t = name.replace(['-', '_'], " ");
            let mut chars = t.chars();
            chars
                .next()
                .map(|f| f.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    };
    (title, c.template.clone().unwrap_or_default())
}

#[allow(clippy::too_many_arguments)]
async fn compare_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    ns: &str,
    repo: &str,
    source: &str,
    target: &str,
    form: Option<CreateForm>,
    mut errors: Vec<FieldError>,
    (dq, hx): (&DiffQuery, super::HxRequest),
) -> Result<Response, AppError> {
    let a = state
        .services
        .repo_access(subject, ns, repo, Action::RepoRead)
        .await?;
    if !state.services.mrs_enabled(&a).await {
        return Err(AppError::not_found());
    }
    let git_dir = state.services.repo_path(a.repo.id);
    let branches: Vec<String> = slath_git::browse::list_refs(
        &state.runner,
        &git_dir,
        slath_git::browse::RefKind::Branch,
        500,
    )
    .await
    .map_err(|e| AppError::internal(&e))?
    .into_iter()
    .filter_map(|r| String::from_utf8(r.name).ok())
    .collect();
    let target = if target.trim().is_empty() {
        a.repo.default_branch.clone()
    } else {
        target.trim().to_owned()
    };
    let source = source.trim().to_owned();
    let mut cmp = None;
    let mut form = form;
    if !source.is_empty() {
        match state
            .services
            .compare(
                subject,
                ns,
                repo,
                &source,
                Some(&target),
                dq.git_options(&ctx),
            )
            .await
        {
            Ok((_, c)) => {
                let limits = &state.config.limits;
                let generated = super::diff::generated(
                    &state.runner,
                    &git_dir,
                    &c.source_head,
                    &c.diff,
                    limits,
                )
                .await;
                let href = format!(
                    "/{ns}/{repo}/compare?source={}&target={}",
                    enc(&source),
                    enc(&target)
                );
                let diff = super::diff::view(&c.diff, limits, &generated, dq, &ctx, href);
                if form.is_none() {
                    let (title, description) = prefill(&c);
                    form = Some(CreateForm {
                        title,
                        description,
                        ..CreateForm::default()
                    });
                }
                cmp = Some(CompareView {
                    commits: commit_rows(&c.commits),
                    more_commits: c.more_commits,
                    nothing: c.commits.is_empty(),
                    unrelated: c.merge_base.is_none(),
                    diff,
                    too_big: c.diff_truncated,
                    existing: c.existing,
                });
            }
            Err(ServiceError::Invalid(e)) => errors.extend(e),
            Err(e) => return Err(e.into()),
        }
    }
    let (_, ms) = state.services.milestones(subject, ns, repo).await?;
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let page = ComparePage {
        header: RepoHeader::load(state, &a, subject, TAB).await,
        can_create: a.can(subject, Action::MrCreate) && a.can(subject, Action::RepoPush),
        milestones: ms
            .into_iter()
            .filter(|m| m.state == "open")
            .map(|m| m.title)
            .collect(),
        branches,
        source,
        target,
        cmp,
        form: form.unwrap_or_default(),
        errors,
        ctx,
    };
    let mut res = match &page.cmp {
        Some(c) => super::diff::respond(hx, &page.ctx, &c.diff, &page),
        None => render(&page),
    };
    *res.status_mut() = status;
    Ok(res)
}

async fn compare(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    hx: super::HxRequest,
    Path((ns, repo)): Path<(String, String)>,
    Query(q): Query<CompareQuery>,
    Query(dq): Query<DiffQuery>,
) -> Result<Response, AppError> {
    compare_page(
        &state,
        ctx,
        &session.subject(),
        &ns,
        &repo,
        &q.source,
        &q.target,
        None,
        Vec::new(),
        (&dq, hx),
    )
    .await
}

async fn create(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<CreateForm>,
) -> Result<Response, AppError> {
    let new = NewMr {
        source_branch: f.source_branch.clone(),
        target_branch: f.target_branch.clone(),
        title: f.title.clone(),
        description: f.description.clone(),
        draft: f.draft.is_some(),
        reviewers: names(&f.reviewers),
        assignees: names(&f.assignees),
        labels: names(&f.labels),
        milestone: Some(f.milestone.clone()),
        delete_source_on_merge: None,
    };
    match state.services.create_mr(&s.subject, &ns, &repo, &new).await {
        Ok(m) => Ok(Redirect::to(&mr_url(&ns, &repo, m.number, "")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            let (source, target) = (f.source_branch.clone(), f.target_branch.clone());
            compare_page(
                &state,
                ctx,
                &s.subject,
                &ns,
                &repo,
                &source,
                &target,
                Some(f),
                errors,
                (&DiffQuery::default(), super::HxRequest::default()),
            )
            .await
        }
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------- view

struct CommentView {
    id: String,
    author_text: String,
    /// Written by a bot: a claim, not a verdict (Weave: EvidenceChip).
    bot: bool,
    created: Time,
    edited: bool,
    deleted: bool,
    hidden: bool,
    own: bool,
    html: MarkupHtml,
    html_empty: bool,
}

struct EventView {
    icon: &'static str,
    actor: String,
    text: String,
    created: Time,
    /// Metadata (labels, assignees, milestones, review requests): folded
    /// with its neighbours (Weave: EventGroup).
    quiet: bool,
}

struct RevisionView {
    seq: i32,
    pusher: String,
    commits: i32,
    short: String,
    oid: String,
    created: Time,
    /// "Compare with previous" link.
    since_href: Option<String>,
    /// Why only the whole diff can be reviewed, if so.
    whole_reason: Option<&'static str>,
    /// "rebased onto main, 2 changes modified, 1 added".
    summary: Option<String>,
}

fn summary_text(s: &slath_service::mr_changes::RevisionSummary, target: &str) -> Option<String> {
    let mut parts = Vec::new();
    if s.rebased {
        parts.push(format!("rebased onto {target}"));
    }
    // "2 changes modified, 1 added": the noun only on the first count.
    let mut first = true;
    for (n, what) in [
        (s.modified, "modified"),
        (s.added, "added"),
        (s.removed, "removed"),
        (s.unresolved, "unresolved"),
    ] {
        if n > 0 {
            parts.push(if first {
                format!("{n} change{} {what}", if n == 1 { "" } else { "s" })
            } else {
                format!("{n} {what}")
            });
            first = false;
        }
    }
    if first && s.unchanged > 0 {
        parts.push("no change modified".to_owned());
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

struct ReviewItem {
    id: String,
    reviewer: String,
    /// "approved", "requested changes", or "reviewed".
    verdict: &'static str,
    icon: &'static str,
    revision: i32,
    body_html: MarkupHtml,
    body_empty: bool,
    created: Time,
    /// Made stale by later changes.
    stale: bool,
}

/// One line of a thread's code snippet: number, text, part of the anchor.
struct SnippetLine {
    no: String,
    text: String,
    anchored: bool,
}

struct ThreadCommentView {
    author: String,
    bot: bool,
    html: MarkupHtml,
    pending: bool,
    created: Time,
}

/// A review thread in the conversation.
struct ThreadItem {
    id: String,
    /// "g.txt, line 5"; `None` for a thread on the whole merge request.
    location: Option<String>,
    outdated: bool,
    /// Carried from this earlier revision.
    from: Option<i32>,
    revision: i32,
    snippet: Vec<SnippetLine>,
    resolved: bool,
    resolved_by: Option<String>,
    comments: Vec<ThreadCommentView>,
    can_resolve: bool,
    /// The viewer started it or wrote in it.
    mine: bool,
}

enum TimelineView {
    Thread(ThreadItem),
    Comment(CommentView),
    Event(EventView),
    Revision(RevisionView),
    Review(ReviewItem),
    /// Consecutive quiet events in one line: the summary and the events.
    Group(String, Vec<EventView>),
}

/// Folds runs of two or more quiet events into one expandable line,
/// moving the "new since your review" position along.
fn fold_events(items: Vec<TimelineView>, since_at: &mut Option<usize>) -> Vec<TimelineView> {
    let mut out: Vec<TimelineView> = Vec::new();
    let mut run: Vec<EventView> = Vec::new();
    let mut new_since = None;
    let flush = |run: &mut Vec<EventView>, out: &mut Vec<TimelineView>| match run.len() {
        0 => {}
        1 => out.push(TimelineView::Event(run.remove(0))),
        n => {
            let actors: std::collections::BTreeSet<&str> =
                run.iter().map(|e| e.actor.as_str()).collect();
            let who = if actors.len() == 1 {
                format!("{} changed", run[0].actor)
            } else {
                format!("{} people changed", actors.len())
            };
            let mut what: Vec<&str> = Vec::new();
            for e in run.iter() {
                let w = match e.icon {
                    "tag" => "labels",
                    "user" => "assignees",
                    "flag" => "the milestone",
                    _ => "reviewers",
                };
                if !what.contains(&w) {
                    what.push(w);
                }
            }
            let summary = format!("{who} {} · {n} events", what.join(" and "));
            out.push(TimelineView::Group(summary, std::mem::take(run)));
        }
    };
    for (i, item) in items.into_iter().enumerate() {
        if *since_at == Some(i) {
            flush(&mut run, &mut out);
            new_since = Some(out.len());
        }
        match item {
            TimelineView::Event(e) if e.quiet => run.push(e),
            other => {
                flush(&mut run, &mut out);
                out.push(other);
            }
        }
    }
    flush(&mut run, &mut out);
    if since_at.is_some() {
        *since_at = new_since;
    }
    out
}

fn event_view(e: slath_db::issues::Event) -> Option<EventView> {
    let str_of = |k: &str| {
        e.data
            .get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let (icon, text) = match e.kind.as_str() {
        "closed" => ("git-pull-request-closed", "closed this".to_owned()),
        "reopened" => ("git-pull-request", "reopened this".to_owned()),
        "merged" => ("git-merge", "merged this".to_owned()),
        "draft" => (
            "git-pull-request-draft",
            "marked this as a draft".to_owned(),
        ),
        "ready" => (
            "git-pull-request",
            "marked this as ready for review".to_owned(),
        ),
        "source_deleted" => (
            "git-branch",
            "deleted the source branch, which closed this".to_owned(),
        ),
        "source_restored" => ("git-branch", "restored the source branch".to_owned()),
        "review_requested" => ("eye", format!("requested a review from {}", str_of("user"))),
        "review_request_removed" => (
            "user",
            format!("removed the review request for {}", str_of("user")),
        ),
        "approvals_carried" => (
            "circle-check",
            format!(
                "kept earlier approvals: revision {} has the same patches",
                e.data
                    .get("revision")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0)
            ),
        ),
        "approvals_stale" => (
            "history",
            format!(
                "made earlier approvals stale: revision {} changed the patches",
                e.data
                    .get("revision")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0)
            ),
        ),
        "change_split" => (
            "git-commit-horizontal",
            format!(
                "split a commit into the new change {}",
                short(&str_of("change"))
            ),
        ),
        "change_joined" => (
            "git-commit-horizontal",
            format!(
                "attached a commit to the change {}",
                short(&str_of("change"))
            ),
        ),
        "change_resolved" => (
            "git-commit-horizontal",
            format!(
                "chose the change {} for a commit with conflicting IDs",
                short(&str_of("change"))
            ),
        ),
        "renamed" => (
            "pencil",
            format!(
                "changed the title from “{}” to “{}”",
                str_of("from"),
                str_of("to")
            ),
        ),
        "labeled" => ("tag", format!("added the label {}", str_of("label"))),
        "unlabeled" => ("tag", format!("removed the label {}", str_of("label"))),
        "assigned" => ("user", format!("assigned {}", str_of("user"))),
        "unassigned" => ("user", format!("unassigned {}", str_of("user"))),
        "milestoned" => (
            "flag",
            format!("added this to the milestone {}", str_of("milestone")),
        ),
        "demilestoned" => (
            "flag",
            format!("removed this from the milestone {}", str_of("milestone")),
        ),
        _ => return None,
    };
    let system = matches!(e.kind.as_str(), "approvals_carried" | "approvals_stale");
    let quiet = matches!(
        e.kind.as_str(),
        "labeled"
            | "unlabeled"
            | "assigned"
            | "unassigned"
            | "milestoned"
            | "demilestoned"
            | "review_requested"
            | "review_request_removed"
    );
    Some(EventView {
        quiet,
        icon,
        actor: e
            .actor
            .unwrap_or_else(|| if system { "Slath" } else { "A deleted user" }.to_owned()),
        text,
        created: e.created_at.into(),
    })
}

fn whole_reason(r: Option<&str>) -> Option<&'static str> {
    match r? {
        "unrelated-histories" => Some("the branches have no common history"),
        "empty" => Some("it has no commits of its own"),
        "too-many-commits" => Some("it has too many commits to show one by one"),
        "merge-commits" => Some("it contains merge commits"),
        _ => Some("of how its history looks"),
    }
}

/// What every merge request page shows above its tabs.
struct MrHeading {
    mr: MergeRequest,
    source: String,
    icon: &'static str,
    state: &'static str,
    state_text: &'static str,
    created: Time,
    perms: MrPerms,
    /// The source branch still exists.
    source_exists: bool,
    can_push: bool,
    /// The readiness card: what to decide, and the viewer's next step.
    card: Card,
}

/// The one primary action of a merge request page, for this viewer.
struct NextAction {
    label: &'static str,
    icon: &'static str,
    /// A link (`#anchor` or a page), or a form posting `field=value`.
    href: Option<String>,
    form: Option<(String, &'static str, &'static str)>,
}

/// The readiness card (Weave: ReadinessCard).
struct Card {
    headline: String,
    reason: String,
    /// `done`, `waiting`, or `blocking`.
    tone: &'static str,
    action: Option<NextAction>,
    rows: Vec<slath_service::mr_reviews::Requirement>,
    /// Timeline entries by others since the viewer's last review.
    since: Option<usize>,
    mergeable: bool,
}

/// Who the viewer is to this merge request, for choosing the next step.
struct Viewer {
    author: bool,
    edit: bool,
    merge: bool,
    review: bool,
    push: bool,
    /// The viewer's approval or request for changes counts now.
    reviewed: bool,
}

fn card(m: &MergeRequest, mb: Mergeability, v: &Viewer, since: Option<usize>, base: &str) -> Card {
    let link = |label, icon, anchor: &str| {
        Some(NextAction {
            label,
            icon,
            href: Some(format!("{base}{anchor}")),
            form: None,
        })
    };
    let done = |headline: String, reason: String, action| Card {
        headline,
        reason,
        tone: "done",
        action,
        rows: Vec::new(),
        since,
        mergeable: false,
    };
    match m.state.as_str() {
        "merged" => {
            return done(
                "Merged".to_owned(),
                format!("Merged into {}.", m.target_branch),
                None,
            );
        }
        "closed" => {
            return Card {
                tone: "waiting",
                ..done(
                    "Closed".to_owned(),
                    "It won't be merged unless it's reopened.".to_owned(),
                    None,
                )
            };
        }
        _ => {}
    }
    if mb.mergeable {
        let action = if v.merge {
            link("Merge", "git-merge", "#merge-form")
        } else {
            None
        };
        let reason = if v.merge {
            "Every requirement is met.".to_owned()
        } else {
            "Every requirement is met; someone with write access can merge it.".to_owned()
        };
        return Card {
            rows: mb.requirements,
            mergeable: true,
            ..done("Ready to merge".to_owned(), reason, action)
        };
    }
    let first = mb
        .requirements
        .iter()
        .find(|r| r.tone == "blocking")
        .or_else(|| mb.requirements.iter().find(|r| r.tone == "waiting"))
        .cloned();
    let Some(r) = first else {
        return Card {
            rows: mb.requirements,
            ..done("Not ready".to_owned(), String::new(), None)
        };
    };
    let headline = match r.key {
        "draft" => "Draft".to_owned(),
        "approvals" => "Approval required".to_owned(),
        "changes_requested" => "Changes requested".to_owned(),
        "statuses" if r.tone == "waiting" => "Checks running".to_owned(),
        "statuses" => "Checks failing".to_owned(),
        "conflicts" => format!("Conflicts with {}", m.target_branch),
        "identity" => "Commits need a change".to_owned(),
        "up_to_date" => format!("Behind {}", m.target_branch),
        "threads" => "Unresolved threads".to_owned(),
        _ => "Not ready".to_owned(),
    };
    let action = match r.key {
        "draft" if v.edit => Some(NextAction {
            label: "Mark as ready",
            icon: "git-pull-request",
            href: None,
            form: Some((format!("{base}/draft"), "draft", "false")),
        }),
        "approvals" | "changes_requested" if v.review && !v.reviewed => {
            link("Review", "eye", "#review-form")
        }
        "approvals" if v.edit => link("Ask for review", "users", "#reviewers"),
        "conflicts" if v.push || v.author => link("Resolve conflicts", "terminal", "#conflicts"),
        "identity" if v.push || v.author => {
            link("Choose changes", "git-commit-horizontal", "/commits")
        }
        _ => None,
    };
    Card {
        headline,
        reason: r.reason.clone(),
        tone: r.tone,
        action,
        rows: mb.requirements,
        since,
        mergeable: false,
    }
}

async fn heading(
    state: &AppState,
    subject: &Subject,
    a: &RepoAccess,
    m: MergeRequest,
) -> Result<MrHeading, AppError> {
    let (icon, st, text) = state_view(&m);
    let source_exists = slath_git::refs::read(
        &state.runner,
        &state.services.repo_path(a.repo.id),
        &slath_git::args::RefName::parse(m.source_ref.clone())
            .map_err(|_| AppError::not_found())?,
    )
    .await
    .map_err(|e| AppError::internal(&e))?
    .is_some();
    let perms = state.services.mr_perms(subject, a, &m);
    let me = match subject {
        Subject::User { id, .. } => Some(*id),
        _ => None,
    };
    let reviewed = me.is_some()
        && state
            .services
            .mr_reviews(&m)
            .await?
            .iter()
            .any(|r| r.counts && r.review.reviewer_id == me);
    let viewer = Viewer {
        author: me.is_some() && m.author_id == me,
        edit: perms.edit,
        merge: a.can(subject, Action::MrMerge),
        review: me.is_some() && a.can(subject, Action::MrReview) && m.author_id != me,
        push: a.can(subject, Action::RepoPush),
        reviewed,
    };
    let mb = state.services.mr_mergeability(a, &m).await?;
    let since = state
        .services
        .since_last_review(subject, a, &m)
        .await?
        .map(|(_, n)| n)
        .filter(|n| *n > 0);
    let base = mr_url(&a.repo.namespace, &a.repo.name, m.number, "");
    let card = card(&m, mb, &viewer, since, &base);
    Ok(MrHeading {
        card,
        perms,
        source: m.source_branch(),
        created: m.created_at.into(),
        icon,
        state: st,
        state_text: text,
        source_exists,
        can_push: a.can(subject, Action::RepoPush),
        mr: m,
    })
}

#[derive(Template)]
#[template(path = "repo/merge_request.html")]
struct MrPage {
    ctx: PageCtx,
    header: RepoHeader,
    h: MrHeading,
    body_html: MarkupHtml,
    body_empty: bool,
    timeline: Vec<TimelineView>,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    assignees: Vec<String>,
    reviewers: Vec<(String, String)>,
    labels: Vec<slath_db::labels::Label>,
    milestone: Option<String>,
    git_checkout: String,
    jj_checkout: String,
    /// Where the "New since your review" divider goes in the timeline.
    since_at: Option<usize>,
    /// `all`, `unresolved`, or `mine` (Weave: SegmentedFilter).
    show: &'static str,
    unresolved: usize,
    /// The viewer's comments waiting for their review.
    pending: usize,
    /// The viewer may review, and approve (not the author).
    can_review: bool,
    can_approve: bool,
    /// The viewer may change who's asked to review.
    can_request: bool,
    /// The viewer last reviewed this earlier revision.
    since_review: Option<i32>,
    /// The latest statuses of the head.
    statuses: Vec<super::StatusRow>,
    /// The viewer may merge: the strategies (default first), the head
    /// they're looking at, and whether the source goes after merging.
    merge_form: Option<MergeForm>,
    /// Commands for Git and jj that resolve conflicts with the target.
    conflict_commands: Option<(String, String)>,
}

struct MergeForm {
    strategies: Vec<(String, &'static str)>,
    head: String,
    delete_source: bool,
}

fn strategy_label(s: &str) -> &'static str {
    match s {
        "fast-forward" => "Fast-forward",
        "rebase" => "Rebase",
        "merge-commit" => "Merge commit",
        "squash" => "Squash",
        _ => "Other",
    }
}

impl MrPage {
    /// The "New since your review" divider goes before timeline entry `i`.
    fn since_here(&self, i: &usize) -> bool {
        self.since_at == Some(*i)
    }

    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }

    fn label_href(&self, name: &str) -> String {
        format!(
            "/{}/{}/merge-requests?state=open&labels={}",
            self.header.namespace,
            self.header.name,
            enc(name)
        )
    }
}

/// Quotes a branch name for a POSIX shell if it needs it.
fn sh(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./@".contains(&b))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

fn snippet(anchor: &slath_db::mr_threads::Anchor) -> Vec<SnippetLine> {
    let get = |k: &str| -> Vec<String> {
        anchor
            .context
            .get(k)
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (before, lines, after) = (get("before"), get("lines"), get("after"));
    let start = i64::from(anchor.line_start.unwrap_or(1));
    let first = start - i64::try_from(before.len()).unwrap_or(0);
    before
        .into_iter()
        .map(|t| (t, false))
        .chain(lines.into_iter().map(|t| (t, true)))
        .chain(after.into_iter().map(|t| (t, false)))
        .enumerate()
        .map(|(i, (text, anchored))| SnippetLine {
            no: (first + i64::try_from(i).unwrap_or(0)).to_string(),
            text: slath_markup::reveal_bidi(&text).into_owned(),
            anchored,
        })
        .collect()
}

async fn thread_item(
    state: &AppState,
    subject: &Subject,
    a: &RepoAccess,
    m: &MergeRequest,
    rc: &RefContext,
    t: slath_service::mr_threads::ThreadView,
) -> Result<ThreadItem, AppError> {
    use slath_service::mr_threads::Position;
    let me = match subject {
        Subject::User { id, .. } => Some(*id),
        _ => None,
    };
    let anchor = &t.thread.anchor;
    let location = anchor.path.as_ref().map(|p| {
        let side = if anchor.side.as_deref() == Some("old") {
            " (removed)"
        } else {
            ""
        };
        match (anchor.line_start, anchor.line_end) {
            (Some(s), Some(e)) if e > s => format!("{p}, lines {s}–{e}{side}"),
            (Some(s), _) => format!("{p}, line {s}{side}"),
            _ => p.clone(),
        }
    });
    let mut comments = Vec::new();
    for c in &t.comments {
        let html = state
            .services
            .render_markdown(subject, Some(rc), &c.body)
            .await?;
        comments.push(ThreadCommentView {
            author: c
                .author
                .clone()
                .unwrap_or_else(|| "A deleted user".to_owned()),
            bot: c.author_bot,
            html: MarkupHtml::from_markup(html),
            pending: c.pending,
            created: c.created_at.into(),
        });
    }
    let mine =
        me.is_some() && (t.thread.author_id == me || t.comments.iter().any(|c| c.author_id == me));
    let can_resolve = me.is_some()
        && a.can(subject, Action::MrComment)
        && (m.author_id == me || t.thread.author_id == me || a.can(subject, Action::RepoPush));
    Ok(ThreadItem {
        id: t.thread.id.to_string(),
        outdated: t.position == Position::Outdated,
        from: match &t.position {
            Position::Current { from, .. } => *from,
            _ => None,
        },
        revision: t.thread.revision_seq,
        snippet: snippet(anchor),
        location,
        resolved: t.thread.resolved_at.is_some(),
        resolved_by: t.thread.resolved_by.clone(),
        comments,
        can_resolve,
        mine,
    })
}

async fn mr_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    mr: (&str, &str, i32),
    message: Option<&'static str>,
    errors: Vec<FieldError>,
) -> Result<Response, AppError> {
    mr_page_showing(state, ctx, subject, mr, message, errors, "all").await
}

/// The conversation, narrowed by `show` (`all`, `unresolved`, `mine`).
async fn mr_page_showing(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    (ns, repo, number): (&str, &str, i32),
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    show: &'static str,
) -> Result<Response, AppError> {
    let (a, m) = state.services.mr(subject, ns, repo, number).await?;
    let rc = RefContext {
        ns: a.repo.namespace.clone(),
        repo: a.repo.name.clone(),
    };
    let body_html = state
        .services
        .render_markdown(subject, Some(&rc), &m.description)
        .await?;
    let summaries = state.services.revision_summaries(&m).await?;
    let last_review = state
        .services
        .since_last_review(subject, &a, &m)
        .await?
        .map(|(t, _)| t);
    let mut since_at = None;
    let mut timeline = Vec::new();
    for item in state.services.mr_timeline(subject, &a, &m).await? {
        let at = slath_service::mr_reviews::item_time(&item).map(|(t, _)| t);
        if since_at.is_none()
            && let (Some(t), Some(last)) = (at, last_review)
            && t > last
        {
            since_at = Some(timeline.len());
        }
        match item {
            MrTimelineItem::Comment(TimelineItem::Comment {
                comment,
                html,
                hidden,
                own,
            }) => timeline.push(TimelineView::Comment(CommentView {
                bot: comment.author_bot,
                id: comment.id.to_string(),
                author_text: comment
                    .author
                    .clone()
                    .unwrap_or_else(|| "A deleted user".to_owned()),
                created: comment.created_at.into(),
                edited: comment.edited_at.is_some(),
                deleted: comment.deleted_at.is_some(),
                hidden,
                own,
                html_empty: html.is_empty(),
                html: MarkupHtml::from_markup(html),
            })),
            MrTimelineItem::Comment(TimelineItem::Event(..)) => {}
            MrTimelineItem::Thread(t) => {
                timeline.push(TimelineView::Thread(
                    thread_item(state, subject, &a, &m, &rc, t).await?,
                ));
            }
            MrTimelineItem::Review(v) => {
                let body_html = state
                    .services
                    .render_markdown(subject, Some(&rc), &v.review.body)
                    .await?;
                let (verdict, icon) = match v.review.verdict.as_str() {
                    "approve" => ("approved", "circle-check"),
                    "request_changes" => ("requested changes", "circle-x"),
                    _ => ("reviewed", "message-square"),
                };
                timeline.push(TimelineView::Review(ReviewItem {
                    id: v.review.id.to_string(),
                    reviewer: v
                        .review
                        .reviewer
                        .unwrap_or_else(|| "A deleted user".to_owned()),
                    verdict,
                    icon,
                    revision: v.review.revision_seq,
                    body_empty: body_html.trim().is_empty(),
                    body_html: MarkupHtml::from_markup(body_html),
                    created: v.review.created_at.into(),
                    stale: v.stale,
                }));
            }
            MrTimelineItem::Event(e) => {
                if let Some(v) = event_view(e) {
                    timeline.push(TimelineView::Event(v));
                }
            }
            MrTimelineItem::Revision {
                revision: r,
                previous_head,
            } => timeline.push(TimelineView::Revision(RevisionView {
                seq: r.seq,
                pusher: r.pushed_by.unwrap_or_else(|| "Someone".to_owned()),
                commits: r.commit_count,
                short: r.head_oid.chars().take(12).collect(),
                since_href: previous_head.is_some().then(|| {
                    mr_url(
                        ns,
                        repo,
                        m.number,
                        &format!("/interdiff?from={}&to={}", r.seq - 1, r.seq),
                    )
                }),
                summary: summaries
                    .get(&r.seq)
                    .and_then(|x| summary_text(x, &m.target_branch)),
                whole_reason: if r.mode == "whole-only" {
                    whole_reason(r.mode_reason.as_deref())
                } else {
                    None
                },
                oid: r.head_oid,
                created: r.created_at.into(),
            })),
        }
    }
    let unresolved = timeline
        .iter()
        .filter(|t| matches!(t, TimelineView::Thread(x) if !x.resolved))
        .count();
    let my_name = ctx.user.as_ref().map(|u| u.name.clone());
    if show != "all" {
        since_at = None;
        timeline.retain(|t| match (show, t) {
            ("unresolved", TimelineView::Thread(x)) => !x.resolved,
            ("mine", TimelineView::Thread(x)) => x.mine,
            ("mine", TimelineView::Comment(c)) => Some(&c.author_text) == my_name.as_ref(),
            ("mine", TimelineView::Review(r)) => Some(&r.reviewer) == my_name.as_ref(),
            _ => false,
        });
    }
    let pending = state.services.pending_comments(subject, &m).await?;
    let timeline = fold_events(timeline, &mut since_at);
    let (assignees, requested, labels, milestone) = state.services.mr_details(&m).await?;
    // Each reviewer's state: their current verdict, else the request.
    let mut reviewers: Vec<(String, String)> = requested
        .into_iter()
        .map(|(n, st)| {
            (
                n,
                if st == "requested" {
                    "requested".to_owned()
                } else {
                    String::new()
                },
            )
        })
        .collect();
    for t in &timeline {
        if let TimelineView::Review(r) = t {
            let st = match (r.verdict, r.stale) {
                ("reviewed", _) => None,
                (_, true) => Some(format!("{}, stale", r.verdict)),
                (v, false) => Some(v.to_owned()),
            };
            match (reviewers.iter_mut().find(|(n, _)| *n == r.reviewer), st) {
                (Some(e), Some(st)) => e.1 = st,
                (None, Some(st)) => reviewers.push((r.reviewer.clone(), st)),
                (None, None) => reviewers.push((r.reviewer.clone(), "commented".to_owned())),
                (Some(_), None) => {}
            }
        }
    }
    let h = heading(state, subject, &a, m.clone()).await?;
    let me = match subject {
        Subject::User { id, .. } => Some(*id),
        _ => None,
    };
    let open = m.state == "open";
    let can_review = open && a.can(subject, Action::MrReview) && me.is_some();
    let can_approve = can_review && m.author_id != me;
    let since_review = state.services.last_reviewed_revision(subject, &m).await?;
    let conflicted = h
        .card
        .rows
        .iter()
        .any(|r| r.key == "conflicts" && r.state != "met");
    let conflict_commands = conflicted.then(|| {
        slath_service::merge::conflict_commands(&sh(&m.source_branch()), &sh(&m.target_branch))
    });
    let merge_form = if open && a.can(subject, Action::MrMerge) && h.card.mergeable {
        let (allowed, default) = state.services.mr_merge_settings(&a).await?;
        let mut strategies: Vec<(String, &'static str)> = allowed
            .iter()
            .map(|x| (x.clone(), strategy_label(x)))
            .collect();
        strategies.sort_by_key(|(x, _)| *x != default);
        Some(MergeForm {
            strategies,
            head: m.head_oid.clone().unwrap_or_default(),
            delete_source: m.delete_source_on_merge,
        })
    } else {
        None
    };
    let statuses = state
        .services
        .mr_statuses(&m)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let n = m.number;
    let source = m.source_branch();
    let mut res = render(&MrPage {
        header: RepoHeader::load(state, &a, subject, TAB).await,
        git_checkout: format!(
            "git fetch origin refs/merge-requests/{n}/head && git switch --detach FETCH_HEAD"
        ),
        jj_checkout: format!(
            "jj git fetch --remote origin --branch {b} && jj new {r}",
            b = sh(&source),
            r = sh(&format!("{source}@origin"))
        ),
        can_request: open && state.services.mr_perms(subject, &a, &m).edit,
        h,
        body_empty: body_html.trim().is_empty(),
        body_html: MarkupHtml::from_markup(body_html),
        timeline,
        since_at,
        show,
        unresolved,
        pending,
        can_review,
        can_approve,
        since_review,
        statuses,
        merge_form,
        conflict_commands,
        message,
        errors,
        assignees,
        reviewers,
        labels,
        milestone,
        ctx,
    });
    *res.status_mut() = status;
    Ok(res)
}

#[derive(Deserialize, Default)]
struct StatusQuery {
    status: Option<String>,
    /// `unresolved` or `mine` narrow the conversation.
    show: Option<String>,
}

async fn view(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let message = match q.status.as_deref() {
        Some("saved") => Some("Merge request saved."),
        Some("restored") => Some("Branch restored."),
        Some("reviewers") => Some("Reviewers saved."),
        Some("merged") => Some("Merged."),
        _ => None,
    };
    let show = match q.show.as_deref() {
        Some("unresolved") => "unresolved",
        Some("mine") => "mine",
        _ => "all",
    };
    mr_page_showing(
        &state,
        ctx,
        &session.subject(),
        (&ns, &repo, number(&n)?),
        message,
        Vec::new(),
        show,
    )
    .await
}

/// A commit with the change it belongs to.
struct ChangeRow {
    commit: CommitRow,
    /// The commit's statuses in one: tone and words ("2 passed").
    checks: Option<(&'static str, String)>,
    entry: Option<StackEntry>,
    /// What the commit can be rebound to: (value, label); `""` is a new
    /// change.
    options: Vec<(String, String)>,
}

#[derive(Template)]
#[template(path = "repo/merge_request_commits.html")]
struct CommitsPage {
    ctx: PageCtx,
    header: RepoHeader,
    h: MrHeading,
    rows: Vec<ChangeRow>,
    removed: Vec<String>,
    /// The revision can only be reviewed as a whole.
    whole: bool,
    blocked: bool,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

async fn commits_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    (ns, repo, n): (&str, &str, i32),
    message: Option<&'static str>,
    errors: Vec<FieldError>,
) -> Result<Response, AppError> {
    let (a, m) = state.services.mr(subject, ns, repo, n).await?;
    let commits = commit_rows(&state.services.mr_commits(&m).await?);
    let stack = state.services.mr_stack(&m).await?;
    let can = m.state == "open" && slath_service::Services::can_rebind(subject, &a, &m);
    let oids: Vec<String> = commits.iter().map(|c| c.oid.clone()).collect();
    let statuses = state.services.statuses_of_commits(&m, &oids).await?;
    let rows = commits
        .into_iter()
        .map(|c| {
            let entry = stack
                .entries
                .iter()
                .find(|e| e.commit_oid == c.oid)
                .cloned();
            let options = match &entry {
                Some(e) if can && e.binding == "unresolved" => e
                    .candidates
                    .iter()
                    .map(|d| (d.clone(), format!("Continue {}", short(d))))
                    .chain([(String::new(), "Start a new change".to_owned())])
                    .collect(),
                Some(e) if can && e.earlier_versions > 0 => {
                    vec![(String::new(), "Split into a new change".to_owned())]
                }
                Some(_) if can => stack
                    .removed
                    .iter()
                    .map(|d| (d.clone(), format!("Continue {}", short(d))))
                    .collect(),
                _ => Vec::new(),
            };
            let checks = statuses.get(&c.oid).map(|list| {
                let failing = list
                    .iter()
                    .filter(|s| matches!(s.state.as_str(), "failure" | "error"))
                    .count();
                let pending = list.iter().filter(|s| s.state == "pending").count();
                if failing > 0 {
                    (
                        "blocking",
                        format!("{failing} of {} checks failing", list.len()),
                    )
                } else if pending > 0 {
                    (
                        "waiting",
                        format!("{pending} of {} checks running", list.len()),
                    )
                } else {
                    ("done", format!("{} checks passed", list.len()))
                }
            });
            ChangeRow {
                checks,
                commit: c,
                entry,
                options,
            }
        })
        .collect();
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&CommitsPage {
        header: RepoHeader::load(state, &a, subject, TAB).await,
        whole: stack.entries.is_empty() && m.revisions > 0,
        blocked: stack.blocks_merge(),
        removed: stack.removed,
        h: heading(state, subject, &a, m).await?,
        rows,
        message,
        errors,
        ctx,
    });
    *res.status_mut() = status;
    Ok(res)
}

async fn commits(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let message = match q.status.as_deref() {
        Some("rebound") => Some("Saved. The next push keeps this choice."),
        _ => None,
    };
    commits_page(
        &state,
        ctx,
        &session.subject(),
        (&ns, &repo, number(&n)?),
        message,
        Vec::new(),
    )
    .await
}

#[derive(Deserialize)]
struct BindForm {
    #[serde(default)]
    change: String,
}

async fn bind(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n, pos)): Path<(String, String, String, String)>,
    Form(f): Form<BindForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let pos: i32 = pos.parse().map_err(|_| AppError::not_found())?;
    let to = if f.change.is_empty() {
        Rebind::New
    } else {
        Rebind::Change(f.change)
    };
    match state
        .services
        .rebind_change(&s.subject, (&ns, &repo, n), pos, &to)
        .await
    {
        Ok(()) => {
            Ok(Redirect::to(&mr_url(&ns, &repo, n, "/commits?status=rebound")).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            commits_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Template)]
#[template(path = "repo/merge_request_changes.html")]
struct ChangesPage {
    ctx: PageCtx,
    header: RepoHeader,
    h: MrHeading,
    diff: DiffView,
    too_big: bool,
    /// Showing what changed since this revision.
    since: Option<i32>,
    current: i32,
}

#[derive(Deserialize, Default)]
struct ChangesQuery {
    since: Option<i32>,
}

async fn changes(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    hx: super::HxRequest,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Query(q): Query<ChangesQuery>,
    Query(dq): Query<DiffQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let (a, m) = state.services.mr(&subject, &ns, &repo, number(&n)?).await?;
    let limits = &state.config.limits;
    let (raw, too_big, generated) = match state
        .services
        .mr_diff(&m, q.since, dq.git_options(&ctx))
        .await?
    {
        Some((raw, cut, head)) => {
            let dir = state.services.repo_path(m.repo_id);
            let g = super::diff::generated(&state.runner, &dir, &head, &raw, limits).await;
            (raw, cut, g)
        }
        None if q.since.is_some() => return Err(AppError::not_found()),
        None => (Vec::new(), false, std::collections::HashSet::new()),
    };
    let href = match q.since {
        Some(k) => mr_url(&ns, &repo, m.number, &format!("/changes?since={k}")),
        None => mr_url(&ns, &repo, m.number, "/changes"),
    };
    let diff = super::diff::view(&raw, limits, &generated, &dq, &ctx, href);
    let page = ChangesPage {
        header: RepoHeader::load(&state, &a, &subject, TAB).await,
        current: m.revisions,
        h: heading(&state, &subject, &a, m).await?,
        diff,
        too_big,
        since: q.since,
        ctx,
    };
    Ok(super::diff::respond(hx, &page.ctx, &page.diff, &page))
}

#[derive(Template)]
#[template(path = "repo/merge_request_change.html")]
struct ChangePage {
    ctx: PageCtx,
    header: RepoHeader,
    h: MrHeading,
    change: ChangeInfo,
    /// Per version (same order): how it was bound, from the ledger.
    matched: Vec<String>,
    /// The change's history from the ledger, oldest first.
    history: Vec<(Time, String)>,
    /// The version shown (or compared to).
    shown: VersionInfo,
    /// The version compared from, for an interdiff.
    from: Option<VersionInfo>,
    /// `exact`, `patches`, `tree`, or empty for a plain version diff.
    kind: &'static str,
    /// The diff, unless the comparison is a range-diff.
    diff: Option<DiffView>,
    /// `git range-diff` output.
    text: Option<String>,
    too_big: bool,
    /// The same comparison the other way (patches or tree).
    other_href: Option<String>,
    base: String,
}

impl ChangePage {
    fn version_href(&self, v: &VersionInfo) -> String {
        format!("{}?rev={}", self.base, v.revision)
    }

    fn compare_href(&self, v: &VersionInfo) -> String {
        format!(
            "{}?from={}&rev={}",
            self.base, v.revision, self.shown.revision
        )
    }
}

/// How a binding entry reads ("by its jj ID", "by similar content").
fn binding_text(e: &slath_db::mr_ledger::Entry) -> String {
    let who = e.actor.clone().unwrap_or_else(|| "someone".to_owned());
    let by = e
        .detail
        .get("by")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match (e.state.as_deref(), e.reason.as_deref()) {
        (Some("new"), Some("split")) => format!("split off into a new change by {who}"),
        (Some("new"), Some("resolved")) => format!("made a new change by {who}"),
        (Some("new"), _) => "first version".to_owned(),
        (Some("confirmed"), Some("join")) => format!("attached to this change by {who}"),
        (Some("confirmed"), Some("resolved")) => format!("chosen for this change by {who}"),
        (Some("confirmed"), Some("patch")) => "matched by an identical patch".to_owned(),
        (Some("confirmed"), _) if by.starts_with("gerrit:") => {
            "matched by its Change-Id trailer".to_owned()
        }
        (Some("confirmed"), _) => "matched by its change ID".to_owned(),
        (Some("inferred"), _) => {
            "matched by similar content (inferred: earlier approvals don't carry over)".to_owned()
        }
        (Some("unresolved"), _) => "unresolved: its IDs name more than one change".to_owned(),
        _ => "recorded".to_owned(),
    }
}

#[derive(Deserialize, Default)]
struct ChangeQuery {
    /// The revision whose version is shown.
    rev: Option<i32>,
    /// Compare with the version in this revision.
    from: Option<i32>,
    /// `1`: a tree diff instead of the diff of patches.
    tree: Option<String>,
}

async fn change(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    hx: super::HxRequest,
    Path((ns, repo, n, id)): Path<(String, String, String, String)>,
    Query(q): Query<ChangeQuery>,
    Query(dq): Query<DiffQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let (a, m) = state.services.mr(&subject, &ns, &repo, number(&n)?).await?;
    let change = state.services.mr_change(&m, &id).await?;
    let pick = |rev: Option<i32>| match rev {
        Some(r) => change.versions.iter().find(|v| v.revision == r).cloned(),
        None => change.versions.last().cloned(),
    };
    let shown = pick(q.rev).ok_or_else(AppError::not_found)?;
    let from = match q.from {
        Some(_) => Some(pick(q.from).ok_or_else(AppError::not_found)?),
        None => None,
    };
    let base = mr_url(
        &ns,
        &repo,
        m.number,
        &format!("/changes/{}", change.display_id),
    );
    let limits = &state.config.limits;
    let dir = state.services.repo_path(m.repo_id);
    let tree = q.tree.as_deref() == Some("1");
    let mut href = format!("{base}?rev={}", shown.revision);
    let (kind, raw, text, too_big, other_href) = match &from {
        None => {
            let (raw, cut) = state
                .services
                .mr_version_diff(&m, &shown, dq.git_options(&ctx))
                .await?;
            ("", Some(raw), None, cut, None)
        }
        Some(f) => {
            href.push_str(&format!("&from={}", f.revision));
            let pair = format!("{base}?from={}&rev={}", f.revision, shown.revision);
            match state
                .services
                .mr_version_interdiff(&m, f, &shown, tree, dq.git_options(&ctx))
                .await?
            {
                Interdiff::Exact(raw, cut) => ("exact", Some(raw), None, cut, None),
                Interdiff::Tree(raw, cut) => {
                    href.push_str("&tree=1");
                    ("tree", Some(raw), None, cut, Some(pair))
                }
                Interdiff::Patches(out, cut) => (
                    "patches",
                    None,
                    Some(slath_markup::reveal_bidi(&String::from_utf8_lossy(&out)).into_owned()),
                    cut,
                    Some(format!("{pair}&tree=1")),
                ),
            }
        }
    };
    let diff = match raw {
        Some(raw) => {
            let head = slath_git::ObjectId::parse(&shown.commit_oid)
                .map_err(|e| AppError::internal(&e))?;
            let generated = super::diff::generated(&state.runner, &dir, &head, &raw, limits).await;
            Some(super::diff::view(&raw, limits, &generated, &dq, &ctx, href))
        }
        None => None,
    };
    let ledger = state.services.mr_ledger(&m).await?;
    let mine: Vec<&slath_db::mr_ledger::Entry> = ledger
        .iter()
        .filter(|e| e.change_id.as_deref() == Some(change.display_id.as_str()))
        .collect();
    let matched = change
        .versions
        .iter()
        .map(|v| {
            mine.iter()
                .rev()
                .find(|e| {
                    e.kind == "binding"
                        && e.revision_seq == Some(v.revision)
                        && e.detail.get("commit").and_then(serde_json::Value::as_str)
                            == Some(v.commit_oid.as_str())
                })
                .map_or_else(|| "recorded".to_owned(), |e| binding_text(e))
        })
        .collect();
    let history = mine
        .iter()
        .map(|e| {
            let rev = e
                .revision_seq
                .map(|r| format!("Revision {r}: "))
                .unwrap_or_default();
            let text = match e.kind.as_str() {
                "binding" => binding_text(e),
                "signature_removed" => {
                    "its signature was removed when the merge rewrote it".to_owned()
                }
                k => k.replace('_', " "),
            };
            (e.created_at.into(), format!("{rev}{text}"))
        })
        .collect();
    let page = ChangePage {
        header: RepoHeader::load(&state, &a, &subject, TAB).await,
        h: heading(&state, &subject, &a, m).await?,
        matched,
        history,
        change,
        shown,
        from,
        kind,
        diff,
        text,
        too_big,
        other_href,
        base,
        ctx,
    };
    match &page.diff {
        Some(d) => Ok(super::diff::respond(hx, &page.ctx, d, &page)),
        None => Ok(render(&page)),
    }
}

#[derive(Template)]
#[template(path = "repo/merge_request_interdiff.html")]
struct InterdiffPage {
    ctx: PageCtx,
    header: RepoHeader,
    h: MrHeading,
    from: i32,
    to: i32,
    text: String,
    too_big: bool,
}

#[derive(Deserialize)]
struct InterdiffQuery {
    from: i32,
    to: Option<i32>,
}

async fn interdiff(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Query(q): Query<InterdiffQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let (a, m) = state.services.mr(&subject, &ns, &repo, number(&n)?).await?;
    let to = q.to.unwrap_or(m.revisions);
    let (out, too_big) = state
        .services
        .mr_revision_interdiff(&m, q.from, to)
        .await?
        .ok_or_else(AppError::not_found)?;
    Ok(render(&InterdiffPage {
        header: RepoHeader::load(&state, &a, &subject, TAB).await,
        h: heading(&state, &subject, &a, m).await?,
        from: q.from,
        to,
        text: slath_markup::reveal_bidi(&String::from_utf8_lossy(&out)).into_owned(),
        too_big,
        ctx,
    }))
}

/// `/{ns}/{repo}/issues/{n}` of a merge request leads here.
pub(super) async fn issue_redirect(
    state: &AppState,
    subject: &Subject,
    ns: &str,
    repo: &str,
    n: i32,
) -> Option<Response> {
    state
        .services
        .mr(subject, ns, repo, n)
        .await
        .ok()
        .map(|_| Redirect::to(&mr_url(ns, repo, n, "")).into_response())
}

// ---------------------------------------------------------------- actions

#[derive(Template)]
#[template(path = "repo/issue_edit.html")]
struct EditPage {
    ctx: PageCtx,
    header: RepoHeader,
    heading: String,
    action: String,
    back: String,
    title: Option<String>,
    body: String,
    errors: Vec<FieldError>,
}

impl EditPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

/// The edit form calls the description `body`.
fn body_errors(errors: Vec<FieldError>) -> Vec<FieldError> {
    errors
        .into_iter()
        .map(|mut e| {
            if e.field == "description" {
                e.field = "body".into();
            }
            e
        })
        .collect()
}

#[derive(Deserialize)]
struct EditForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
}

async fn edit_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let (a, m) = state
        .services
        .mr(&s.subject, &ns, &repo, number(&n)?)
        .await?;
    if !state.services.mr_perms(&s.subject, &a, &m).edit {
        return Err(AppError::forbidden());
    }
    Ok(render(&EditPage {
        header: RepoHeader::load(&state, &a, &s.subject, TAB).await,
        heading: format!("Edit merge request !{}", m.number),
        action: mr_url(&ns, &repo, m.number, "/edit"),
        back: mr_url(&ns, &repo, m.number, ""),
        title: Some(m.title),
        body: m.description,
        errors: Vec::new(),
        ctx,
    }))
}

async fn edit(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<EditForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    match state
        .services
        .edit_mr(&s.subject, &ns, &repo, n, &f.title, &f.body)
        .await
    {
        Ok(()) => Ok(Redirect::to(&mr_url(&ns, &repo, n, "?status=saved")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            let (a, _) = state.services.mr(&s.subject, &ns, &repo, n).await?;
            let mut res = render(&EditPage {
                header: RepoHeader::load(&state, &a, &s.subject, TAB).await,
                heading: format!("Edit merge request !{n}"),
                action: mr_url(&ns, &repo, n, "/edit"),
                back: mr_url(&ns, &repo, n, ""),
                title: Some(f.title),
                body: f.body,
                errors: body_errors(errors),
                ctx,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct StateForm {
    #[serde(default)]
    state: String,
}

async fn set_state(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<StateForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    match state
        .services
        .set_mr_open(&s.subject, &ns, &repo, n, f.state == "open")
        .await
    {
        Ok(()) => Ok(Redirect::to(&mr_url(&ns, &repo, n, "")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct DraftForm {
    #[serde(default)]
    draft: String,
}

async fn set_draft(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<DraftForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    state
        .services
        .set_mr_draft(&s.subject, &ns, &repo, n, f.draft == "true")
        .await?;
    Ok(Redirect::to(&mr_url(&ns, &repo, n, "")).into_response())
}

async fn restore(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    match state
        .services
        .restore_mr_branch(&s.subject, &ns, &repo, n)
        .await
    {
        Ok(()) => Ok(Redirect::to(&mr_url(&ns, &repo, n, "?status=restored")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct CommentForm {
    #[serde(default)]
    body: String,
    /// "Comment and close" (`closed`) or "Comment and reopen" (`open`).
    #[serde(default)]
    state: Option<String>,
}

async fn comment(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<CommentForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let change = f
        .state
        .as_deref()
        .filter(|s| matches!(*s, "closed" | "open"));
    let added = if change.is_some() && f.body.trim().is_empty() {
        Ok(None)
    } else {
        state
            .services
            .add_mr_comment(&s.subject, &ns, &repo, n, &f.body)
            .await
            .map(Some)
    };
    let added = match (added, change) {
        (Ok(c), Some(to)) => state
            .services
            .set_mr_open(&s.subject, &ns, &repo, n, to == "open")
            .await
            .map(|()| c),
        (r, None) => r,
        (Err(e), Some(_)) => Err(e),
    };
    match added {
        Ok(Some(c)) => {
            Ok(Redirect::to(&mr_url(&ns, &repo, n, &format!("#comment-{}", c.id))).into_response())
        }
        Ok(None) => Ok(Redirect::to(&mr_url(&ns, &repo, n, "")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct ReviewForm {
    #[serde(default)]
    verdict: String,
    #[serde(default)]
    body: String,
}

async fn review(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<ReviewForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    match state
        .services
        .submit_review(&s.subject, (&ns, &repo, n), &f.verdict, &f.body)
        .await
    {
        Ok(r) => {
            Ok(Redirect::to(&mr_url(&ns, &repo, n, &format!("#review-{}", r.id))).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct MergeFormInput {
    #[serde(default)]
    strategy: String,
    #[serde(default)]
    expected_head_oid: String,
    #[serde(default)]
    commit_message: String,
    delete_source: Option<String>,
}

async fn merge(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<MergeFormInput>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let input = slath_service::merge::MergeInput {
        strategy: Some(f.strategy).filter(|x| !x.is_empty()),
        expected_head_oid: f.expected_head_oid,
        commit_message: Some(f.commit_message).filter(|x| !x.trim().is_empty()),
        delete_source: Some(f.delete_source.is_some()),
    };
    match state
        .services
        .merge_mr(&s.subject, (&ns, &repo, n), &input)
        .await
    {
        Ok(_) => Ok(Redirect::to(&mr_url(&ns, &repo, n, "?status=merged")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(ServiceError::Conflict(why)) => {
            let errors = vec![FieldError {
                field: "merge".into(),
                code: "conflict".into(),
                message: why,
            }];
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct ReviewersForm {
    #[serde(default)]
    reviewers: String,
}

async fn set_reviewers(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<ReviewersForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    match state
        .services
        .set_mr_reviewers(&s.subject, (&ns, &repo, n), &names(&f.reviewers))
        .await
    {
        Ok(()) => Ok(Redirect::to(&mr_url(&ns, &repo, n, "?status=reviewers")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

struct WaitingRow {
    href: String,
    title: String,
    repo: String,
    number: i32,
    author: Option<String>,
    updated: Time,
}

#[derive(Template)]
#[template(path = "review_requests.html")]
struct ReviewRequestsPage {
    ctx: PageCtx,
    rows: Vec<WaitingRow>,
}

/// `GET /-/review-requests`: merge requests waiting for the viewer's review.
async fn review_requests(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
) -> Result<Response, AppError> {
    let rows = state
        .services
        .review_requests(&s.subject)
        .await?
        .into_iter()
        .map(|w| WaitingRow {
            href: mr_url(&w.namespace, &w.repo_name, w.number, ""),
            repo: format!("{}/{}", w.namespace, w.repo_name),
            title: w.title,
            number: w.number,
            author: w.author,
            updated: w.updated_at.into(),
        })
        .collect();
    Ok(render(&ReviewRequestsPage { ctx, rows }))
}

#[derive(Deserialize)]
struct ReplyForm {
    #[serde(default)]
    body: String,
    /// `pending` adds it to the review instead of sending it now.
    #[serde(default)]
    mode: String,
    /// Where to go afterwards (a page of this merge request).
    #[serde(default)]
    back: String,
}

/// `back` if it's a page of this merge request, else its conversation.
fn back_to(ns: &str, repo: &str, n: i32, back: &str, anchor: &str) -> String {
    let base = mr_url(ns, repo, n, "");
    let target = if back.starts_with(&base) && !back.contains("//") && !back.contains('#') {
        back.to_owned()
    } else {
        base
    };
    format!("{target}{anchor}")
}

async fn thread_reply(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n, id)): Path<(String, String, String, String)>,
    Form(f): Form<ReplyForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let id = comment_id(&id)?;
    match state
        .services
        .reply(
            &s.subject,
            (&ns, &repo, n),
            id,
            &f.body,
            f.mode == "pending",
        )
        .await
    {
        Ok(_) => Ok(
            Redirect::to(&back_to(&ns, &repo, n, &f.back, &format!("#thread-{id}")))
                .into_response(),
        ),
        Err(ServiceError::Invalid(errors)) => {
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct ResolveForm {
    #[serde(default)]
    resolved: String,
    #[serde(default)]
    back: String,
}

async fn thread_resolve(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, n, id)): Path<(String, String, String, String)>,
    Form(f): Form<ResolveForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let id = comment_id(&id)?;
    state
        .services
        .set_thread_resolved(&s.subject, (&ns, &repo, n), id, f.resolved == "true")
        .await?;
    Ok(Redirect::to(&back_to(&ns, &repo, n, &f.back, &format!("#thread-{id}"))).into_response())
}

fn comment_id(s: &str) -> Result<uuid::Uuid, AppError> {
    s.parse().map_err(|_| AppError::not_found())
}

async fn comment_edit_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n, id)): Path<(String, String, String, String)>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let id = comment_id(&id)?;
    let (a, m) = state.services.mr(&s.subject, &ns, &repo, n).await?;
    let c = state
        .services
        .mr_timeline(&s.subject, &a, &m)
        .await?
        .into_iter()
        .find_map(|t| match t {
            MrTimelineItem::Comment(TimelineItem::Comment {
                comment, own: true, ..
            }) if comment.id == id && comment.deleted_at.is_none() => Some(comment),
            _ => None,
        })
        .ok_or_else(AppError::not_found)?;
    Ok(render(&EditPage {
        header: RepoHeader::load(&state, &a, &s.subject, TAB).await,
        heading: "Edit comment".to_owned(),
        action: mr_url(&ns, &repo, n, &format!("/comments/{id}/edit")),
        back: mr_url(&ns, &repo, n, &format!("#comment-{id}")),
        title: None,
        body: c.body,
        errors: Vec::new(),
        ctx,
    }))
}

async fn comment_edit(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n, id)): Path<(String, String, String, String)>,
    Form(f): Form<CommentForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let id = comment_id(&id)?;
    // The comment must belong to this merge request.
    let (a, m) = state.services.mr(&s.subject, &ns, &repo, n).await?;
    let c = state.services.comment_subject(id).await?;
    if c != *m.id.as_uuid() {
        return Err(AppError::not_found());
    }
    match state
        .services
        .edit_issue_comment(&s.subject, &ns, &repo, id, &f.body)
        .await
    {
        Ok(()) => {
            Ok(Redirect::to(&mr_url(&ns, &repo, n, &format!("#comment-{id}"))).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            let mut res = render(&EditPage {
                header: RepoHeader::load(&state, &a, &s.subject, TAB).await,
                heading: "Edit comment".to_owned(),
                action: mr_url(&ns, &repo, n, &format!("/comments/{id}/edit")),
                back: mr_url(&ns, &repo, n, &format!("#comment-{id}")),
                title: None,
                body: f.body,
                errors,
                ctx,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        Err(e) => Err(e.into()),
    }
}

async fn comment_op(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, n, id, op)): Path<(String, String, String, String, String)>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let id = comment_id(&id)?;
    let (_, m) = state.services.mr(&s.subject, &ns, &repo, n).await?;
    if state.services.comment_subject(id).await? != *m.id.as_uuid() {
        return Err(AppError::not_found());
    }
    match op.as_str() {
        "delete" => {
            state
                .services
                .delete_issue_comment(&s.subject, &ns, &repo, id)
                .await?;
        }
        "hide" | "unhide" => {
            state
                .services
                .hide_issue_comment(&s.subject, &ns, &repo, id, op == "hide")
                .await?;
        }
        _ => return Err(AppError::not_found()),
    }
    Ok(Redirect::to(&mr_url(&ns, &repo, n, "")).into_response())
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as(
            "/{ns}/{repo}/merge-requests",
            Access::Resource,
            get(list).post(create),
        )
        .route_as("/{ns}/{repo}/compare", Access::Resource, get(compare))
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}",
            Access::Resource,
            get(view),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/commits",
            Access::Resource,
            get(commits),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/stack/{pos}/bind",
            Access::Resource,
            post(bind),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/changes/{id}",
            Access::Resource,
            get(change),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/interdiff",
            Access::Resource,
            get(interdiff),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/changes",
            Access::Resource,
            get(changes),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/edit",
            Access::Resource,
            get(edit_page).post(edit),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/state",
            Access::Resource,
            post(set_state),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/draft",
            Access::Resource,
            post(set_draft),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/restore",
            Access::Resource,
            post(restore),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/comments",
            Access::Resource,
            post(comment),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/reviews",
            Access::Resource,
            post(review),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/threads/{id}/comments",
            Access::Resource,
            post(thread_reply),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/threads/{id}/resolve",
            Access::Resource,
            post(thread_resolve),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/merge",
            Access::Resource,
            post(merge),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/reviewers",
            Access::Resource,
            post(set_reviewers),
        )
        .route_as("/-/review-requests", Access::Account, get(review_requests))
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/comments/{id}/edit",
            Access::Resource,
            get(comment_edit_page).post(comment_edit),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/comments/{id}/{op}",
            Access::Resource,
            post(comment_op),
        )
}
