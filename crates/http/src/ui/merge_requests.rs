// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merge request pages: list, compare and create, the conversation with its
//! timeline, commits, changes, and the actions on them.

use askama::Template;
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::authz::{Action, Subject};
use slath_db::merge_requests::MergeRequest;
use slath_markup::refs::RefContext;
use slath_service::issues::TimelineItem;
use slath_service::merge_requests::{Compare, MrPerms, MrTimelineItem, NewMr};
use slath_service::mr_changes::{
    ChangeInfo, Interdiff, Outcome, Rebind, RemovedChange, StackEntry, VersionInfo,
};
use slath_service::mr_reviews::Mergeability;
use slath_service::repos::RepoAccess;
use slath_service::{FieldError, ServiceError};

use super::browse::time;
use super::diff::{DiffQuery, DiffView};
use super::repos::RepoHeader;
use super::settings::error_for;
use super::{MarkupHtml, PageCtx, Time, Who, render};
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

use super::issues::{ListQuery, enc};

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
        _ if m.attention == "iterating" => ("bot", "iterating", "Iterating"),
        _ => ("git-pull-request", "open", "Open"),
    }
}

/// A commit row on the compare, commits, and conversation pages.
struct CommitRow {
    oid: String,
    short: String,
    subject: String,
    author: String,
    /// The account the author is shown as, or empty (see
    /// `Services::commit_people`).
    author_account: String,
    time: Time,
}

/// The rows of `commits` of a repository the subject reads, with the
/// accounts their authors are shown as.
async fn commit_rows(
    state: &AppState,
    subject: &Subject,
    repo: slath_core::ids::RepoId,
    commits: &[slath_git::browse::LogEntry],
) -> Result<Vec<CommitRow>, AppError> {
    let idents: Vec<slath_service::git_identities::CommitIdent<'_>> = commits
        .iter()
        .map(|c| slath_service::git_identities::CommitIdent {
            oid: c.oid.as_str(),
            author_email: &c.author_email,
            committer_email: "",
        })
        .collect();
    let people = state.services.commit_people(subject, repo, &idents).await?;
    Ok(commits
        .iter()
        .zip(people)
        .map(|(c, p)| CommitRow {
            oid: c.oid.as_str().to_owned(),
            short: c.oid.as_str()[..12].to_owned(),
            subject: slath_markup::reveal_bidi(&c.subject).into_owned(),
            author: c.author_name.clone(),
            author_account: p.author,
            time: time(c.author_time),
        })
        .collect())
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
    /// The head's checks; `None` without statuses or when not open.
    checks: Option<Checks>,
    /// Who is asked to review it, or reviewed it.
    reviewers: Vec<String>,
}

/// A head's checks in one tone (`done`, `waiting`, `blocking`) and words.
struct Checks {
    tone: &'static str,
    text: String,
}

/// The checks of a head in one tone and words: failing if any failed,
/// running if any is pending, passed if all passed.
fn checks_summary(statuses: &[slath_db::statuses::Status]) -> Option<Checks> {
    let total = statuses.len();
    if total == 0 {
        return None;
    }
    let count = |states: &[&str]| {
        statuses
            .iter()
            .filter(|s| states.contains(&s.state.as_str()))
            .count()
    };
    let failed = count(&["failure", "error"]);
    let pending = count(&["pending"]);
    let checks = |n: usize| if n == 1 { "check" } else { "checks" };
    let (tone, text) = if failed > 0 {
        (
            "blocking",
            format!("{failed} of {total} {} failed", checks(total)),
        )
    } else if pending > 0 {
        (
            "waiting",
            format!("{pending} of {total} {} running", checks(total)),
        )
    } else if total == 1 {
        ("done", "The check passed".to_owned())
    } else {
        ("done", format!("All {total} checks passed"))
    };
    Some(Checks { tone, text })
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
    /// The filter and order as a query string without the state.
    query: String,
    /// The filter alone, without the order.
    filters: String,
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
        let (_, reviewers, labels, _) = state.services.mr_details(&m).await?;
        let (icon, s, text) = state_view(&m);
        let checks = if m.state == "open" {
            checks_summary(&state.services.mr_statuses(&m).await?)
        } else {
            None
        };
        rows.push(MrRow {
            checks,
            labels,
            reviewers: reviewers.into_iter().map(|(n, _)| n).collect(),
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
    let filters = q.filters();
    Ok(render(&ListPage {
        milestones: ms.into_iter().map(|m| m.title).collect(),
        filtered: !filters.is_empty(),
        filters,
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
    /// This repository (`ns/name`).
    here: String,
    /// Other repositories of the fork network to merge from.
    source_repos: Vec<String>,
    /// The chosen one; empty: this repository.
    source_repo: String,
    source_branches: Vec<String>,
    /// This repository's branches (targets).
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
    /// A less visible fork: creating needs the confirmation.
    publishes_source: bool,
}

#[derive(Deserialize, Default, Clone)]
struct CompareQuery {
    #[serde(default)]
    source_repo: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    target: String,
}

#[derive(Deserialize, Default, Clone)]
struct CreateForm {
    #[serde(default)]
    source_repo: String,
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
    #[serde(default)]
    allow_maintainer_edits: Option<String>,
    #[serde(default)]
    publish_source: Option<String>,
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

/// What the compare page compares: a branch of the source repository
/// (`ns/name`, empty: this one) into a branch of this one.
#[derive(Default)]
struct CompareInput<'a> {
    source_repo: &'a str,
    source: &'a str,
    target: &'a str,
}

async fn branch_names(state: &AppState, dir: &std::path::Path) -> Result<Vec<String>, AppError> {
    Ok(
        slath_git::browse::list_refs(&state.runner, dir, slath_git::browse::RefKind::Branch, 500)
            .await
            .map_err(|e| AppError::internal(&e))?
            .into_iter()
            .filter_map(|r| String::from_utf8(r.name).ok())
            .collect(),
    )
}

#[allow(clippy::too_many_arguments)]
async fn compare_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    ns: &str,
    repo: &str,
    input: CompareInput<'_>,
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
    let here = format!("{}/{}", a.repo.namespace, a.repo.name);
    let mut source_repos = state.services.mr_source_choices(subject, &a).await?;
    // A fork source the viewer can read (it may not be theirs) is offered too.
    let wanted = input.source_repo.trim();
    let source_repo = if wanted.is_empty() || wanted == here {
        String::new()
    } else {
        wanted.to_owned()
    };
    let mut bad_source = false;
    let src = if source_repo.is_empty() {
        None
    } else {
        match (state.services)
            .mr_source(subject, &a, Some(&source_repo))
            .await
        {
            Ok(src) => src,
            Err(ServiceError::Invalid(e)) => {
                errors.extend(e);
                bad_source = true;
                None
            }
            Err(e) => return Err(e.into()),
        }
    };
    if let Some(s) = &src {
        let name = format!("{}/{}", s.repo.namespace, s.repo.name);
        if !source_repos.contains(&name) {
            source_repos.push(name);
        }
    }
    let source_repo = src
        .as_ref()
        .map(|s| format!("{}/{}", s.repo.namespace, s.repo.name))
        .unwrap_or_default();
    let branches = branch_names(state, &git_dir).await?;
    let src_dir = src.as_ref().map(|s| state.services.repo_path(s.repo.id));
    let source_branches = match &src_dir {
        Some(d) => branch_names(state, d).await?,
        None => branches.clone(),
    };
    let target = if input.target.trim().is_empty() {
        a.repo.default_branch.clone()
    } else {
        input.target.trim().to_owned()
    };
    let source = input.source.trim().to_owned();
    let mut cmp = None;
    let mut form = form;
    if !source.is_empty() && !bad_source {
        match state
            .services
            .compare(
                subject,
                ns,
                repo,
                (Some(&source_repo), &source),
                Some(&target),
                dq.git_options(&ctx),
            )
            .await
        {
            Ok((_, c)) => {
                let limits = &state.config.limits;
                // A fork's commits are read in place.
                let runner = match &src_dir {
                    Some(d) => state
                        .runner
                        .reading_objects_of(d)
                        .map_err(|e| AppError::internal(&e))?,
                    None => state.runner.clone(),
                };
                let generated =
                    super::diff::generated(&runner, &git_dir, &c.source_head, &c.diff, limits)
                        .await;
                let highlights =
                    super::diff::highlights(&runner, &git_dir, &c.diff, limits, dq, &ctx).await;
                let repo_param = if source_repo.is_empty() {
                    String::new()
                } else {
                    format!("source_repo={}&", enc(&source_repo))
                };
                let href = format!(
                    "/{ns}/{repo}/compare?{repo_param}source={}&target={}",
                    enc(&source),
                    enc(&target)
                );
                let diff =
                    super::diff::view(&c.diff, limits, &generated, &highlights, dq, &ctx, href);
                if form.is_none() {
                    let (title, description) = prefill(&c);
                    form = Some(CreateForm {
                        title,
                        description,
                        allow_maintainer_edits: Some("1".to_owned()),
                        ..CreateForm::default()
                    });
                }
                cmp = Some(CompareView {
                    commits: commit_rows(state, subject, a.repo.id, &c.commits).await?,
                    more_commits: c.more_commits,
                    nothing: c.commits.is_empty(),
                    unrelated: c.merge_base.is_none(),
                    diff,
                    too_big: c.diff_truncated,
                    existing: c.existing,
                    publishes_source: c.publishes_source,
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
    let can_create = match &src {
        Some(s) => a.can(subject, Action::MrCreateFork) && s.can(subject, Action::RepoPush),
        None => a.can(subject, Action::MrCreate) && a.can(subject, Action::RepoPush),
    };
    let page = ComparePage {
        header: RepoHeader::load(state, &a, subject, TAB).await,
        can_create,
        milestones: ms
            .into_iter()
            .filter(|m| m.state == "open")
            .map(|m| m.title)
            .collect(),
        here,
        source_repos,
        source_repo,
        source_branches,
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
        CompareInput {
            source_repo: &q.source_repo,
            source: &q.source,
            target: &q.target,
        },
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
        source_repo: f.source_repo.clone(),
        allow_maintainer_edits: (!f.source_repo.trim().is_empty())
            .then_some(f.allow_maintainer_edits.is_some()),
        publish_source: f.publish_source.is_some(),
        source_branch: f.source_branch.clone(),
        target_branch: f.target_branch.clone(),
        title: f.title.clone(),
        description: f.description.clone(),
        draft: f.draft.is_some(),
        reviewers: names(&f.reviewers),
        assignees: names(&f.assignees),
        labels: names(&f.labels),
        milestone: Some(f.milestone.clone()),
        ..NewMr::default()
    };
    match state.services.create_mr(&s.subject, &ns, &repo, &new).await {
        Ok(m) => Ok(Redirect::to(&mr_url(&ns, &repo, m.number, "")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            let (source_repo, source, target) = (
                f.source_repo.clone(),
                f.source_branch.clone(),
                f.target_branch.clone(),
            );
            compare_page(
                &state,
                ctx,
                &s.subject,
                &ns,
                &repo,
                CompareInput {
                    source_repo: &source_repo,
                    source: &source,
                    target: &target,
                },
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
    author: Who,
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
    actor: Who,
    text: String,
    /// What the event points at (a reference's source).
    link: Option<(String, String)>,
    created: Time,
    /// Metadata (labels, assignees, milestones, review requests): folded
    /// with its neighbours (Weave: EventGroup).
    quiet: bool,
}

struct RevisionView {
    seq: i32,
    pusher: Who,
    /// Pushed by a bot: consecutive ones fold into an iteration group.
    bot: bool,
    commits: i32,
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
        (s.returned, "returned"),
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
    reviewer: Who,
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
    id: String,
    author: Who,
    /// A pending reply that resolves its thread when the review is
    /// submitted.
    resolves: bool,
    bot: bool,
    html: MarkupHtml,
    /// Nothing besides the suggestion.
    html_empty: bool,
    pending: bool,
    created: Time,
    suggestion: Option<SuggestionView>,
    /// A finding's message: plain text, never rendered as Markdown.
    text: Option<String>,
}

/// A suggestion block of a line comment: the lines it replaces and its
/// text, and whether the viewer can apply it now.
struct SuggestionView {
    old: Vec<String>,
    new: Vec<String>,
    /// Why it can't be applied (outdated, removed lines); `None` if it can.
    blocked: Option<&'static str>,
    can_apply: bool,
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
    /// Who marked it addressed while it's open (bots do; a person
    /// resolves it).
    addressed_by: Option<String>,
    comments: Vec<ThreadCommentView>,
    can_resolve: bool,
    /// The viewer started it or wrote in it.
    mine: bool,
    /// "Finding from lint (warning)" for a status's annotation.
    finding: Option<String>,
}

/// How a finding thread names where it came from.
fn finding_label(f: &slath_db::mr_findings::Finding) -> String {
    format!("Finding from {} ({})", f.context, f.severity)
}

enum TimelineView {
    Thread(ThreadItem),
    Comment(CommentView),
    Event(EventView),
    Revision(RevisionView),
    Review(ReviewItem),
    /// Consecutive quiet events in one line: the one actor (if one), the
    /// summary after their name, and the events.
    Group(Option<Who>, String, Vec<EventView>),
    /// Consecutive revisions pushed by the same bot, nothing between them.
    Iterations(IterationGroup),
}

struct IterationGroup {
    pusher: Who,
    first: i32,
    last: i32,
    /// Compares the revision before the group with its last one.
    compare_href: Option<String>,
    revisions: Vec<RevisionView>,
}

/// Folds runs of two or more consecutive revisions by the same bot into
/// one entry, moving the "new since your review" position along.
fn fold_iterations(
    items: Vec<TimelineView>,
    since_at: &mut Option<usize>,
    compare: impl Fn(i32, i32) -> String,
) -> Vec<TimelineView> {
    let mut out: Vec<TimelineView> = Vec::new();
    let mut run: Vec<RevisionView> = Vec::new();
    let mut new_since = None;
    let flush = |run: &mut Vec<RevisionView>, out: &mut Vec<TimelineView>| match run.len() {
        0 => {}
        1 => out.push(TimelineView::Revision(run.remove(0))),
        _ => {
            let revisions = std::mem::take(run);
            let (first, last) = (revisions[0].seq, revisions[revisions.len() - 1].seq);
            out.push(TimelineView::Iterations(IterationGroup {
                pusher: revisions[0].pusher.clone(),
                first,
                last,
                compare_href: (first > 1).then(|| compare(first - 1, last)),
                revisions,
            }));
        }
    };
    for (i, item) in items.into_iter().enumerate() {
        if *since_at == Some(i) {
            flush(&mut run, &mut out);
            new_since = Some(out.len());
        }
        match item {
            TimelineView::Revision(r)
                if r.bot && run.last().is_none_or(|p| p.pusher == r.pusher) =>
            {
                run.push(r);
            }
            other => {
                flush(&mut run, &mut out);
                match other {
                    TimelineView::Revision(r) if r.bot => run.push(r),
                    other => out.push(other),
                }
            }
        }
    }
    flush(&mut run, &mut out);
    if since_at.is_some() {
        *since_at = new_since;
    }
    out
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
                run.iter().map(|e| e.actor.text()).collect();
            // One actor leads with their name; several are counted.
            let (lead, who) = if actors.len() == 1 {
                (Some(run[0].actor.clone()), "changed".to_owned())
            } else {
                (None, format!("{} people changed", actors.len()))
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
            out.push(TimelineView::Group(lead, summary, std::mem::take(run)));
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
        "merged" if str_of("strategy") == "external" => (
            "git-merge",
            format!("merged this by pushing to {}", str_of("target")),
        ),
        "merged" => ("git-merge", "merged this".to_owned()),
        "changes_landed" => {
            let ids: Vec<String> = e
                .data
                .get("changes")
                .and_then(serde_json::Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(|id| id.chars().take(8).collect())
                        .collect()
                })
                .unwrap_or_default();
            (
                "git-merge",
                format!(
                    "pushed {} {} to {}; the rest is still open",
                    if ids.len() == 1 { "change" } else { "changes" },
                    ids.join(", "),
                    str_of("target")
                ),
            )
        }
        "possibly_landed" => (
            "triangle-alert",
            format!(
                "pushed commit {} to {}: change {} possibly landed with modifications (the commit names it, but its changes differ)",
                str_of("commit").chars().take(10).collect::<String>(),
                str_of("target"),
                str_of("change").chars().take(8).collect::<String>()
            ),
        ),
        "draft" => (
            "git-pull-request-draft",
            "marked this as a draft".to_owned(),
        ),
        "ready" => (
            "git-pull-request",
            "marked this as ready for review".to_owned(),
        ),
        "attention_released" => (
            "users",
            "asked the reviewers: the required checks passed".to_owned(),
        ),
        "source_deleted" => (
            "git-branch",
            "deleted the source branch, which closed this".to_owned(),
        ),
        "source_restored" => ("git-branch", "restored the source branch".to_owned()),
        "auto_merge_set" => (
            "clock",
            format!(
                "set this to merge when ready ({})",
                strategy_label(&str_of("strategy")).to_lowercase()
            ),
        ),
        "auto_merge_cancelled" => (
            "clock",
            match str_of("reason").as_str() {
                "push" => "cancelled merging when ready by pushing a new revision".to_owned(),
                "closed" => "cancelled merging when ready by closing this".to_owned(),
                "failed" => format!("couldn't merge this when ready: {}", str_of("detail")),
                "no_permission" => {
                    "cancelled merging when ready: its setter can no longer merge".to_owned()
                }
                _ => "cancelled merging when ready".to_owned(),
            },
        ),
        "branch_updated" if str_of("method") == "rebase" => (
            "refresh-cw",
            format!(
                "updated the branch by rebasing it onto {}",
                str_of("target")
            ),
        ),
        "branch_updated" => (
            "refresh-cw",
            format!("updated the branch by merging {} into it", str_of("target")),
        ),
        "maintainer_edits_allowed" => (
            "pencil",
            "allowed edits by maintainers of this repository".to_owned(),
        ),
        "maintainer_edits_disallowed" => (
            "pencil",
            "stopped allowing edits by maintainers of this repository".to_owned(),
        ),
        "source_published" => (
            "eye",
            format!(
                "confirmed that the commits of {}, a {} repository, become readable here",
                str_of("source"),
                str_of("visibility")
            ),
        ),
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
        "approvals_stale" if e.data.get("changes").is_some() => {
            let ids: Vec<&str> = e
                .data
                .get("changes")
                .and_then(serde_json::Value::as_array)
                .map(|a| a.iter().filter_map(serde_json::Value::as_str).collect())
                .unwrap_or_default();
            (
                "history",
                format!(
                    "made earlier approvals of {} {} stale: revision {} changed {}",
                    if ids.len() == 1 { "change" } else { "changes" },
                    ids.join(", "),
                    e.data
                        .get("revision")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0),
                    if ids.len() == 1 {
                        "its patch"
                    } else {
                        "their patches"
                    }
                ),
            )
        }
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
    let system = matches!(
        e.kind.as_str(),
        "approvals_carried" | "approvals_stale" | "attention_released"
    );
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
        actor: match e.actor {
            Some(n) => Who::Account(n),
            None if system => Who::Slath,
            None => Who::Deleted,
        },
        text,
        link: None,
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
    /// A fork source head push protection held back.
    held: Option<HeldView>,
    /// `ns/name` of a fork source.
    source_repo: Option<String>,
    /// The viewer may turn edits by maintainers on or off.
    can_set_edits: bool,
    /// Copy-paste commands that check the merge request out.
    git_checkout: String,
    jj_checkout: String,
    /// A fork merge request whose `mr/<n>` the viewer doesn't fetch yet:
    /// the form that adds it, instead of commands that wouldn't work.
    checkout_form: Option<String>,
    /// Whose commands to show first.
    tool: ToolChoice,
    /// The current revision's commits and changed files, for the tabs
    /// (empty before the first revision).
    commits: String,
    files: String,
}

/// The viewer's tool for command blocks: preselected when known (setting
/// or recent pushes); otherwise Git is, and the browser remembers a pick.
#[derive(Clone, Copy)]
struct ToolChoice {
    git: bool,
    jj: bool,
}

impl ToolChoice {
    fn known(&self) -> bool {
        self.git || self.jj
    }
}

async fn tool_choice(state: &AppState, subject: &Subject) -> Result<ToolChoice, AppError> {
    use slath_service::tools::Tool;
    let t = state.services.tool_pref(subject).await?.effective();
    Ok(ToolChoice {
        git: t == Some(Tool::Git),
        jj: t == Some(Tool::Jj),
    })
}

/// A held-back fork source on the merge request page.
struct HeldView {
    head: String,
    /// The viewer may allow findings.
    may_allow: bool,
    incomplete: Option<String>,
    findings: Vec<HeldFindingView>,
}

struct HeldFindingView {
    place: String,
    description: String,
    rule: String,
    excerpt: String,
    /// The allowance page for this finding, if the viewer may allow it.
    allow: Option<String>,
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
    /// The source contains the target's head.
    up_to_date: bool,
    /// Files no counting approver had viewed when approving (at most
    /// [`UNVIEWED_MAX`]).
    unviewed: Vec<String>,
    /// Show all of them, not the first [`UNVIEWED_SHOWN`].
    all_unviewed: bool,
    /// The conversation's merge box with all of them.
    unviewed_href: String,
}

impl Card {
    /// The files not viewed by an approver that the card names.
    fn unviewed_shown(&self) -> &[String] {
        // "and 1 more" would take the place of the one file it hides.
        if self.all_unviewed || self.unviewed.len() <= UNVIEWED_SHOWN + 1 {
            &self.unviewed
        } else {
            &self.unviewed[..self.unviewed.len().min(UNVIEWED_SHOWN)]
        }
    }

    /// How many of them the card leaves out.
    fn unviewed_more(&self) -> usize {
        self.unviewed.len() - self.unviewed_shown().len()
    }
}

/// How many files not viewed by an approver the card names before "and N
/// more" (all of them when only one more is left), and how many the full
/// list holds.
const UNVIEWED_SHOWN: usize = 5;
const UNVIEWED_MAX: usize = 500;

/// Who the viewer is to this merge request, for choosing the next step.
struct Viewer {
    author: bool,
    edit: bool,
    merge: bool,
    review: bool,
    push: bool,
    /// The viewer may update the source branch with the target.
    update: bool,
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
    let up_to_date = mb.up_to_date;
    let mut unviewed = mb
        .coverage
        .as_ref()
        .map(|c| c.unviewed.clone())
        .unwrap_or_default();
    unviewed.truncate(UNVIEWED_MAX);
    let unviewed_href = format!("{base}?files=unviewed#merge-box");
    let done = |headline: String, reason: String, action| Card {
        headline,
        reason,
        tone: "done",
        action,
        rows: Vec::new(),
        since,
        mergeable: false,
        up_to_date,
        unviewed: unviewed.clone(),
        all_unviewed: false,
        unviewed_href: unviewed_href.clone(),
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
        "iterating" => "Iterating".to_owned(),
        "approvals" => "Approval required".to_owned(),
        "sensitive_paths" => "Maintainer approval required".to_owned(),
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
        "draft" | "iterating" if v.edit => Some(NextAction {
            label: "Mark as ready",
            icon: "git-pull-request",
            href: None,
            form: Some((format!("{base}/draft"), "draft", "false")),
        }),
        "approvals" | "changes_requested" | "sensitive_paths" if v.review && !v.reviewed => {
            link("Review", "eye", "#review-form")
        }
        "approvals" | "sensitive_paths" if v.edit => link("Ask for review", "users", "#reviewers"),
        "conflicts" if v.push || v.author => link("Resolve conflicts", "terminal", "#conflicts"),
        "identity" if v.push || v.author => {
            link("Choose changes", "git-commit-horizontal", "/changes")
        }
        "up_to_date" if v.update => link("Update branch", "refresh-cw", "#update-branch"),
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
        up_to_date,
        unviewed,
        all_unviewed: false,
        unviewed_href,
    }
}

async fn heading(
    state: &AppState,
    subject: &Subject,
    a: &RepoAccess,
    m: MergeRequest,
) -> Result<MrHeading, AppError> {
    let (icon, st, text) = state_view(&m);
    let source_exists = match m.source_repo_id {
        Some(src) => slath_git::refs::read(
            &state.runner,
            &state.services.repo_path(src),
            &slath_git::args::RefName::parse(m.source_ref.clone())
                .map_err(|_| AppError::not_found())?,
        )
        .await
        .map_err(|e| AppError::internal(&e))?
        .is_some(),
        None => false,
    };
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
        update: state.services.may_update_branch(subject, a, &m).await,
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
    let may_allow = a.can(subject, Action::RepoPush)
        && (a.can(subject, Action::RepoSettings)
            || !state.services.allowances_need_admin(a.repo.id).await?);
    let held = slath_service::push_protection::HeldSource::of(&m).map(|h| HeldView {
        head: short(&h.head).to_owned(),
        may_allow,
        incomplete: h.incomplete,
        findings: h
            .findings
            .into_iter()
            .map(|f| HeldFindingView {
                place: match (&f.path, f.place.as_str()) {
                    (Some(p), _) => format!("{p} line {} in commit {}", f.line, short(&f.object)),
                    (None, "tag_message") => format!("message of tag {}", short(&f.object)),
                    _ => format!("message of commit {}", short(&f.object)),
                },
                allow: may_allow.then(|| {
                    format!(
                        "/{}/{}/-/secret-scan/allow?fingerprint={}&rule={}",
                        a.repo.namespace,
                        a.repo.name,
                        f.fingerprint,
                        enc(&f.rule)
                    )
                }),
                description: f.description,
                rule: f.rule,
                excerpt: f.excerpt,
            })
            .collect(),
    });
    let source_repo = match m.source_repo_id {
        Some(id) if id != m.repo_id => state.services.mr_source_name(&m).await?,
        _ => None,
    };
    let can_set_edits =
        m.state == "open" && state.services.may_set_maintainer_edits(subject, &m).await;
    let n = m.number;
    let branch = m.source_branch();
    // A fork merge request is the branch mr/<n> here for those involved,
    // fetched over the address that authenticates.
    let fork_branch = source_repo.is_some() && me.is_some();
    let sees = fork_branch && state.services.sees_mr_branch(subject, &m).await;
    let checkout_form =
        (fork_branch && !sees).then(|| mr_url(&a.repo.namespace, &a.repo.name, n, "/checkout"));
    let auth_url = state
        .config
        .server
        .public_url
        .join(&format!("/-/auth/{}/{}.git", a.repo.namespace, a.repo.name));
    let mr_branch = format!("mr/{n}");
    let git_checkout = if sees {
        format!(
            "git fetch {u} {b}:{b} && git switch {b}",
            u = sh(&auth_url),
            b = sh(&mr_branch)
        )
    } else {
        format!("git fetch origin refs/merge-requests/{n}/head && git switch --detach FETCH_HEAD")
    };
    // jj fetches branches only: a fork's branch comes from the fork unless
    // mr/<n> shows here.
    let jj_checkout = match &source_repo {
        Some(_) if sees => format!(
            "jj git remote set-url origin {u} && jj git fetch --branch {b} && jj bookmark track {t} && jj new {b}",
            u = sh(&auth_url),
            b = sh(&mr_branch),
            t = sh(&format!("{mr_branch}@origin"))
        ),
        Some(r) => {
            let remote = r.split('/').next().unwrap_or("fork").to_owned();
            format!(
                "jj git remote add {rm} {url} && jj git fetch --remote {rm} --branch {b} && jj new {at}",
                rm = sh(&remote),
                url = sh(&state.config.server.public_url.join(&format!("/{r}.git"))),
                b = sh(&branch),
                at = sh(&format!("{branch}@{remote}"))
            )
        }
        None => format!(
            "jj git fetch --remote origin --branch {b} && jj new {r}",
            b = sh(&branch),
            r = sh(&format!("{branch}@origin"))
        ),
    };
    // The tabs count what the current revision has.
    let (commits, files) = match state.services.mr_revisions(&m).await?.last() {
        Some(r) => (
            r.commit_count.to_string(),
            state.services.mr_changed_paths(&m).await?.len().to_string(),
        ),
        None => (String::new(), String::new()),
    };
    Ok(MrHeading {
        commits,
        files,
        checkout_form,
        tool: tool_choice(state, subject).await?,
        git_checkout,
        jj_checkout,
        source_repo,
        can_set_edits,
        held,
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
    update_form: Option<UpdateForm>,
    auto_merge: Option<AutoMergeView>,
    /// The head, if the viewer can apply a suggestion of the conversation.
    apply_suggestions: Option<String>,
    /// Commands for Git and jj that resolve conflicts with the target.
    conflict_commands: Option<(String, String)>,
}

struct MergeForm {
    strategies: Vec<(String, &'static str)>,
    head: String,
    delete_source: bool,
}

/// Merge when ready: the form while it isn't set, or what is set.
struct AutoMergeView {
    /// Set: strategy label and who set it.
    set: Option<(&'static str, Who)>,
    /// The viewer may cancel it.
    may_cancel: bool,
    /// Not set: the strategies to choose from (default first).
    strategies: Vec<(String, &'static str)>,
}

/// Updating a branch that's behind its target.
struct UpdateForm {
    head: String,
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
    let may_apply = m.state == "open" && state.services.may_apply_suggestions(subject, a, m).await;
    let anchored: Vec<String> = anchor
        .context
        .get("lines")
        .and_then(|l| l.as_array())
        .map(|l| {
            l.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let mut comments = Vec::new();
    for (i, c) in t.comments.iter().enumerate() {
        if i == 0 && t.thread.finding.is_some() {
            comments.push(ThreadCommentView {
                id: c.id.to_string(),
                author: Who::account(c.author.clone()),
                resolves: false,
                bot: c.author_bot,
                html: MarkupHtml::from_markup(String::new()),
                html_empty: true,
                pending: false,
                created: c.created_at.into(),
                suggestion: None,
                text: Some(c.body.clone()),
            });
            continue;
        }
        // The suggestion block shows as its own card, so the comment's text
        // goes without it.
        let shown = anchor
            .path
            .as_ref()
            .and_then(|_| slath_markup::suggestion::without_suggestion(&c.body))
            .unwrap_or_else(|| c.body.clone());
        let html = state
            .services
            .render_markdown(subject, Some(rc), &shown)
            .await?;
        let suggestion = anchor
            .path
            .as_ref()
            .and_then(|_| slath_markup::suggestion::suggestion(&c.body))
            .map(|text| {
                let blocked = if anchor.side.as_deref() == Some("old") {
                    Some("It's on removed lines, so it can't be applied.")
                } else if !matches!(t.position, Position::Current { .. }) {
                    Some("Outdated: its lines changed since, so it can't be applied.")
                } else if c.pending {
                    Some("It can be applied once the review is submitted.")
                } else {
                    None
                };
                SuggestionView {
                    old: anchored.clone(),
                    new: text.lines().map(str::to_owned).collect(),
                    can_apply: blocked.is_none() && may_apply,
                    blocked,
                }
            });
        comments.push(ThreadCommentView {
            id: c.id.to_string(),
            html_empty: html.trim().is_empty(),
            suggestion,
            author: Who::account(c.author.clone()),
            resolves: c.resolves,
            bot: c.author_bot,
            html: MarkupHtml::from_markup(html),
            pending: c.pending,
            created: c.created_at.into(),
            text: None,
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
        addressed_by: t
            .thread
            .addressed_at
            .and(t.thread.addressed_by.clone())
            .filter(|_| t.thread.resolved_at.is_none()),
        comments,
        can_resolve,
        mine,
        finding: t.thread.finding.as_deref().map(finding_label),
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
    mr_page_showing(state, ctx, subject, mr, message, errors, ("all", false)).await
}

/// The conversation, narrowed by `show` (`all`, `unresolved`, `mine`); with
/// `all_unviewed`, its merge box names every file not viewed by an approver.
async fn mr_page_showing(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    (ns, repo, number): (&str, &str, i32),
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    (show, all_unviewed): (&'static str, bool),
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
                author: Who::account(comment.author.clone()),
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
                    thread_item(state, subject, &a, &m, &rc, *t).await?,
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
                    reviewer: Who::account(v.review.reviewer),
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
            MrTimelineItem::Referenced(e, src) => timeline.push(TimelineView::Event(EventView {
                icon: "link",
                actor: Who::account(e.actor),
                text: "mentioned this in".to_owned(),
                link: Some((src.path(), src.label())),
                created: e.created_at.into(),
                quiet: false,
            })),
            MrTimelineItem::Revision {
                revision: r,
                previous_head,
            } => timeline.push(TimelineView::Revision(RevisionView {
                seq: r.seq,
                bot: r.pushed_by.as_deref().is_some_and(|p| ctx.is_bot(p)),
                pusher: r.pushed_by.map_or(Who::Unknown, Who::Account),
                commits: r.commit_count,
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
            ("mine", TimelineView::Comment(c)) => c.author.name() == my_name.as_deref(),
            ("mine", TimelineView::Review(r)) => r.reviewer.name() == my_name.as_deref(),
            _ => false,
        });
    }
    let pending = state.services.pending_comments(subject, &m).await?;
    let timeline = fold_iterations(timeline, &mut since_at, |from, to| {
        mr_url(
            ns,
            repo,
            m.number,
            &format!("/interdiff?from={from}&to={to}"),
        )
    });
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
            let Some(name) = r.reviewer.name() else {
                continue;
            };
            match (reviewers.iter_mut().find(|(n, _)| n == name), st) {
                (Some(e), Some(st)) => e.1 = st,
                (None, Some(st)) => reviewers.push((name.to_owned(), st)),
                (None, None) => reviewers.push((name.to_owned(), "commented".to_owned())),
                (Some(_), None) => {}
            }
        }
    }
    let mut h = heading(state, subject, &a, m.clone()).await?;
    h.card.all_unviewed = all_unviewed;
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
    let apply_suggestions = timeline
        .iter()
        .any(|t| match t {
            TimelineView::Thread(x) => x
                .comments
                .iter()
                .any(|c| c.suggestion.as_ref().is_some_and(|s| s.can_apply)),
            _ => false,
        })
        .then(|| m.head_oid.clone().unwrap_or_default());
    let auto_merge = match state.services.mr_auto_merge(&m).await? {
        Some(am) if open => Some(AutoMergeView {
            may_cancel: state
                .services
                .may_cancel_auto_merge(subject, &a, &m, &am)
                .await,
            set: Some((strategy_label(&am.strategy), Who::account(am.by))),
            strategies: Vec::new(),
        }),
        None if open && !h.card.mergeable && a.can(subject, Action::MrAutoMerge) => {
            let (allowed, default) = state.services.mr_merge_settings(&a).await?;
            let mut strategies: Vec<(String, &'static str)> = allowed
                .iter()
                .map(|x| (x.clone(), strategy_label(x)))
                .collect();
            strategies.sort_by_key(|(x, _)| *x != default);
            Some(AutoMergeView {
                set: None,
                may_cancel: false,
                strategies,
            })
        }
        _ => None,
    };
    let update_form =
        (open && !h.card.up_to_date && state.services.may_update_branch(subject, &a, &m).await)
            .then(|| UpdateForm {
                head: m.head_oid.clone().unwrap_or_default(),
            });
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
    let mut res = render(&MrPage {
        header: RepoHeader::load(state, &a, subject, TAB).await,
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
        update_form,
        auto_merge,
        apply_suggestions,
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
    /// `unviewed`: the merge box names every file not viewed by an approver.
    files: Option<String>,
}

async fn view(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    if let (n, Some(format)) = crate::plain::split_suffix(&n) {
        let subject = session.subject();
        return crate::plain::merge_request(&state, &subject, (&ns, &repo, number(n)?), format)
            .await;
    }
    let message = match q.status.as_deref() {
        Some("saved") => Some("Merge request saved."),
        Some("restored") => Some("Branch restored."),
        Some("reviewers") => Some("Reviewers saved."),
        Some("merged") => Some("Merged."),
        Some("updated") => Some("Branch updated."),
        Some("applied") => Some("Suggestions applied."),
        Some("checkout") => {
            Some("Its branch now shows in your fetches; Check out has the commands.")
        }
        Some("auto_merge") => Some("It merges when it's ready."),
        Some("auto_merge_cancelled") => Some("Merge when ready cancelled."),
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
        (show, q.files.as_deref() == Some("unviewed")),
    )
    .await
}

/// What revision `rev` did to a change, for its row: `None` for the first
/// revision, where everything is new, and for unresolved commits, which
/// say so themselves.
fn outcome_text(o: Outcome, rev: i32) -> Option<String> {
    if rev < 2 {
        return None;
    }
    Some(match o {
        Outcome::New => format!("New in revision {rev}"),
        Outcome::Unchanged => format!("Unchanged in revision {rev}: the same patch"),
        Outcome::Updated => format!("Updated in revision {rev}"),
        Outcome::Returned { removed_in } => {
            format!("Returned in revision {rev}, after it was removed in revision {removed_in}")
        }
        Outcome::Unresolved => return None,
    })
}

/// A commit with the change it belongs to.
struct ChangeRow {
    commit: CommitRow,
    /// The commit's statuses in one: tone and words ("2 passed").
    checks: Option<(&'static str, String)>,
    entry: Option<StackEntry>,
    /// What this revision did to its change ("Updated in revision 3").
    outcome: Option<String>,
    /// What the commit can be rebound to: (value, label); `""` is a new
    /// change.
    options: Vec<(String, String)>,
}

#[derive(Template)]
#[template(path = "repo/merge_request_stack.html")]
struct StackPage {
    ctx: PageCtx,
    header: RepoHeader,
    h: MrHeading,
    rows: Vec<ChangeRow>,
    removed: Vec<RemovedChange>,
    /// The revision can only be reviewed as a whole.
    whole: bool,
    blocked: bool,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

async fn stack_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    (ns, repo, n): (&str, &str, i32),
    message: Option<&'static str>,
    errors: Vec<FieldError>,
) -> Result<Response, AppError> {
    let (a, m) = state.services.mr(subject, ns, repo, n).await?;
    let commits = commit_rows(
        state,
        subject,
        a.repo.id,
        &state.services.mr_commits(&m).await?,
    )
    .await?;
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
                    .map(|d| {
                        let d = &d.display_id;
                        (d.clone(), format!("Continue {}", short(d)))
                    })
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
                outcome: entry
                    .as_ref()
                    .and_then(|e| outcome_text(e.outcome, m.revisions)),
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
    let mut res = render(&StackPage {
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

/// Query keys only the diff of the files takes: links to the files from
/// before it moved from `…/changes` to `…/files`.
const FILES_KEYS: [&str; 5] = ["since", "view", "w", "file", "comment"];

/// The stack (the Changes tab). A query for the diff of the files (an
/// older link from when that page was here) goes on to `…/files`.
async fn stack(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Query(q): Query<StatusQuery>,
    RawQuery(raw): RawQuery,
) -> Result<Response, AppError> {
    let raw = raw.unwrap_or_default();
    let for_files = raw.split('&').any(|kv| {
        let key = kv.split('=').next().unwrap_or("");
        FILES_KEYS.contains(&key)
    });
    if for_files {
        return moved(
            &state,
            &session.subject(),
            (&ns, &repo, &n),
            &format!("/files?{raw}"),
        )
        .await;
    }
    let message = match q.status.as_deref() {
        Some("rebound") => Some("Saved. The next push keeps this choice."),
        _ => None,
    };
    stack_page(
        &state,
        ctx,
        &session.subject(),
        (&ns, &repo, number(&n)?),
        message,
        Vec::new(),
    )
    .await
}

/// The Commits tab's old address: the stack is the Changes tab now.
async fn old_commits(
    State(state): State<AppState>,
    session: MaybeSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Response, AppError> {
    let rest = match raw {
        Some(q) if !q.is_empty() => format!("/changes?{q}"),
        _ => "/changes".to_owned(),
    };
    moved(&state, &session.subject(), (&ns, &repo, &n), &rest).await
}

/// A permanent redirect to `rest` of a merge request the subject can
/// see; otherwise not found, as for the page itself.
async fn moved(
    state: &AppState,
    subject: &Subject,
    (ns, repo, n): (&str, &str, &str),
    rest: &str,
) -> Result<Response, AppError> {
    let (_, m) = state.services.mr(subject, ns, repo, number(n)?).await?;
    Ok(Redirect::permanent(&mr_url(ns, repo, m.number, rest)).into_response())
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
            Ok(Redirect::to(&mr_url(&ns, &repo, n, "/changes?status=rebound")).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            stack_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Template)]
#[template(path = "repo/merge_request_files.html")]
struct FilesPage {
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
struct FilesQuery {
    since: Option<i32>,
}

async fn files(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    hx: super::HxRequest,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Query(q): Query<FilesQuery>,
    Query(dq): Query<DiffQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let (a, m) = state.services.mr(&subject, &ns, &repo, number(&n)?).await?;
    let limits = &state.config.limits;
    let (raw, too_big, generated, highlights) = match state
        .services
        .mr_diff(&m, q.since, dq.git_options(&ctx))
        .await?
    {
        Some((raw, cut, head)) => {
            let dir = state.services.repo_path(m.repo_id);
            let g = super::diff::generated(&state.runner, &dir, &head, &raw, limits).await;
            let h = super::diff::highlights(&state.runner, &dir, &raw, limits, &dq, &ctx).await;
            (raw, cut, g, h)
        }
        None if q.since.is_some() => return Err(AppError::not_found()),
        None => (
            Vec::new(),
            false,
            std::collections::HashSet::new(),
            super::diff::Highlights::new(),
        ),
    };
    let href = match q.since {
        Some(k) => mr_url(&ns, &repo, m.number, &format!("/files?since={k}")),
        None => mr_url(&ns, &repo, m.number, "/files"),
    };
    let mut diff = super::diff::view(&raw, limits, &generated, &highlights, &dq, &ctx, href);
    if q.since.is_none() && m.state == "open" {
        attach_review(&state, &subject, &a, &m, &mut diff, &dq, None).await?;
    }
    let page = FilesPage {
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
    /// The versions, oldest first, with what the list shows of each.
    rows: Vec<VersionRow>,
    /// The current revision's changes, bottom first; empty for a stack
    /// of one change, which needs no list.
    stack: Vec<StackRow>,
    /// Changes not in the current revision.
    removed: Vec<RemovedChange>,
    /// The revision that removed the shown change, if it's removed.
    removed_in: Option<i32>,
    /// This change's position in the current stack and the stack's size;
    /// `None` without a stack column or when it isn't in the stack.
    change_pos: Option<(usize, usize)>,
    /// The changes below and above this one in the current revision.
    prev_change: Option<String>,
    next_change: Option<String>,
    /// The versions before and after the shown one.
    prev_version: Option<String>,
    next_version: Option<String>,
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
    /// The shown version holds a conflict jj recorded.
    conflict: Option<ConflictView>,
}

/// What a change page says about a conflict jj recorded in the shown
/// version.
struct ConflictView {
    /// The version is in the current revision, so it blocks merging.
    current: bool,
    /// The lowest change of the stack with a conflict, when that's another
    /// change below this one: its short display ID and page.
    below: Option<(String, String)>,
    /// Commands (Git, jj) resolving the lowest conflict, while the merge
    /// request is open.
    commands: Option<(String, String)>,
}

/// One version in a change's list of versions.
struct VersionRow {
    v: VersionInfo,
    /// Its commit's author and patch size, if Git could read it.
    stat: Option<VersionStat>,
    /// Comments in threads on this version's own diff.
    comments: usize,
    checks: Option<Checks>,
    /// How it was bound, from the ledger.
    matched: String,
}

/// One change in the stack column.
struct StackRow {
    position: usize,
    display_id: String,
    subject: String,
    href: String,
    /// The change shown on the page.
    current: bool,
    /// Its state in words with a tone, if not simply open: landed, new in
    /// this revision, unresolved, or conflicted.
    flags: Vec<(&'static str, String)>,
    /// Its verdicts that count, in words, with a tone.
    verdicts: Option<(&'static str, String)>,
    checks: Option<Checks>,
}

struct VersionStat {
    author: String,
    author_account: String,
    time: Time,
    files: usize,
    added: usize,
    removed: usize,
}

impl ChangePage {
    fn version_href(&self, v: &VersionInfo) -> String {
        format!("{}?rev={}", self.base, v.revision)
    }

    /// The version the picker compares from: the one compared now, else
    /// the one before the shown version.
    fn picker_from(&self) -> i32 {
        if let Some(f) = &self.from {
            return f.revision;
        }
        let i = self
            .change
            .versions
            .iter()
            .position(|v| v.revision == self.shown.revision)
            .unwrap_or(0);
        self.change
            .versions
            .get(i.saturating_sub(1))
            .map_or(self.shown.revision, |v| v.revision)
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
    }
    // The version picker can name the shown version twice: its own diff.
    .filter(|f| f.revision != shown.revision);
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
            let highlights =
                super::diff::highlights(&state.runner, &dir, &raw, limits, &dq, &ctx).await;
            let mut d = super::diff::view(&raw, limits, &generated, &highlights, &dq, &ctx, href);
            if from.is_none() && m.state == "open" && shown.revision == m.revisions {
                attach_review(
                    &state,
                    &subject,
                    &a,
                    &m,
                    &mut d,
                    &dq,
                    Some(&shown.commit_oid),
                )
                .await?;
            }
            Some(d)
        }
        None => None,
    };
    let ledger = state.services.mr_ledger(&m).await?;
    let mine: Vec<&slath_db::mr_ledger::Entry> = ledger
        .iter()
        .filter(|e| e.change_id.as_deref() == Some(change.display_id.as_str()))
        .collect();
    let rows = version_rows(&state, &subject, &a, &m, &change, &mine).await?;
    let (stack, removed) = stack_rows(&state, &a, &m, &change.display_id).await?;
    let conflict = if shown.conflicted {
        Some(conflict_view(&state, &m, &base, &change.display_id, &shown).await?)
    } else {
        None
    };
    let removed_in = removed
        .iter()
        .find(|r| r.display_id == change.display_id)
        .map(|r| r.removed_in);
    let at = stack.iter().position(|r| r.current);
    let neighbour = |d: isize| {
        at.and_then(|i| i.checked_add_signed(d))
            .and_then(|i| stack.get(i))
            .map(|r| r.href.clone())
    };
    let (prev_change, next_change) = (neighbour(-1), neighbour(1));
    let change_pos = at.map(|i| (i + 1, stack.len()));
    let vi = change
        .versions
        .iter()
        .position(|v| v.revision == shown.revision)
        .unwrap_or(0);
    let version_at = |i: Option<usize>| {
        i.and_then(|i| change.versions.get(i))
            .map(|v| format!("{base}?rev={}", v.revision))
    };
    let (prev_version, next_version) = (version_at(vi.checked_sub(1)), version_at(Some(vi + 1)));
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
        rows,
        stack,
        removed,
        removed_in,
        change_pos,
        prev_change,
        next_change,
        prev_version,
        next_version,
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
        conflict,
        ctx,
    };
    match &page.diff {
        Some(d) => Ok(super::diff::respond(hx, &page.ctx, d, &page)),
        None => Ok(render(&page)),
    }
}

/// What the page of `shown` (a version of the change `id` with a conflict)
/// says about it: whether it blocks merging, the lowest change with a
/// conflict below it (jj's conflicts carry over to the changes above, so
/// resolving that one often resolves this one too), and the commands that
/// resolve the lowest.
async fn conflict_view(
    state: &AppState,
    m: &MergeRequest,
    base: &str,
    id: &str,
    shown: &VersionInfo,
) -> Result<ConflictView, AppError> {
    let current = m.revisions == shown.revision;
    if !current {
        return Ok(ConflictView {
            current,
            below: None,
            commands: None,
        });
    }
    let stack = state.services.mr_stack(m).await?;
    let lowest = stack
        .entries
        .iter()
        .filter(|e| e.conflicted)
        .min_by_key(|e| e.position);
    let below = lowest
        .and_then(|e| e.display_id.as_deref())
        .filter(|d| *d != id)
        .map(|d| {
            let page = match base.rsplit_once('/') {
                Some((changes, _)) => format!("{changes}/{d}"),
                None => d.to_owned(),
            };
            (d.chars().take(8).collect(), page)
        });
    let commands = (m.state == "open").then(|| {
        let (change, commit) = match lowest {
            Some(e) => (
                e.display_id.clone().unwrap_or_default(),
                e.commit_oid.clone(),
            ),
            None => (id.to_owned(), shown.commit_oid.clone()),
        };
        slath_service::merge::jj_conflict_commands(
            &sh(&m.source_branch()),
            &sh(&m.target_branch),
            &change,
            &commit.chars().take(12).collect::<String>(),
        )
    });
    Ok(ConflictView {
        current,
        below,
        commands,
    })
}

/// The stack column of a change page: the current revision's changes,
/// bottom first, with their state, verdicts, and checks, and the changes
/// it no longer has. No rows for a revision of one change (or reviewed as
/// a whole) without removed changes: there's nothing to move between.
async fn stack_rows(
    state: &AppState,
    a: &RepoAccess,
    m: &MergeRequest,
    shown: &str,
) -> Result<(Vec<StackRow>, Vec<RemovedChange>), AppError> {
    let stack = state.services.mr_stack(m).await?;
    if stack.entries.len() + stack.removed.len() < 2 {
        return Ok((Vec::new(), stack.removed));
    }
    let subjects: std::collections::HashMap<String, String> = state
        .services
        .mr_commits(m)
        .await?
        .into_iter()
        .map(|c| {
            (
                c.oid.as_str().to_owned(),
                slath_markup::reveal_bidi(&c.subject).into_owned(),
            )
        })
        .collect();
    let verdicts = state.services.mr_change_verdicts(a, m).await?;
    let oids: Vec<String> = stack.entries.iter().map(|e| e.commit_oid.clone()).collect();
    let statuses = state.services.statuses_of_commits(m, &oids).await?;
    let base = mr_url(&a.repo.namespace, &a.repo.name, m.number, "");
    let rows = stack
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let id = e.display_id.clone().unwrap_or_default();
            let mut flags = Vec::new();
            if e.change_state.as_deref() == Some("merged") {
                flags.push(("done", "Landed".to_owned()));
            } else if m.revisions > 1 {
                match e.outcome {
                    Outcome::New => {
                        flags.push(("waiting", format!("New in revision {}", m.revisions)));
                    }
                    Outcome::Returned { .. } => {
                        flags.push(("waiting", format!("Returned in revision {}", m.revisions)));
                    }
                    _ => {}
                }
            }
            if e.binding == "unresolved" {
                flags.push(("blocking", "Unresolved".to_owned()));
            }
            if e.conflicted {
                flags.push(("blocking", "Conflict".to_owned()));
            }
            let v = verdicts
                .iter()
                .find(|v| !id.is_empty() && v.display_id == id);
            let verdicts = v.and_then(|v| {
                if !v.blockers.is_empty() {
                    Some((
                        "blocking",
                        format!("Changes requested by {}", v.blockers.join(", ")),
                    ))
                } else if !v.approvers.is_empty() {
                    Some(("done", format!("Approved by {}", v.approvers.join(", "))))
                } else {
                    None
                }
            });
            StackRow {
                position: i + 1,
                subject: subjects.get(&e.commit_oid).cloned().unwrap_or_default(),
                href: if id.is_empty() {
                    format!("{base}/changes")
                } else {
                    format!("{base}/changes/{id}")
                },
                current: !id.is_empty() && id == shown,
                display_id: id,
                flags,
                verdicts,
                checks: statuses.get(&e.commit_oid).and_then(|s| checks_summary(s)),
            }
        })
        .collect();
    Ok((rows, stack.removed))
}

/// The rows of a change's list of versions: author, age, and patch size
/// from Git, comments on each version's own diff, its checks, and how the
/// ledger bound it.
async fn version_rows(
    state: &AppState,
    subject: &Subject,
    a: &RepoAccess,
    m: &MergeRequest,
    change: &ChangeInfo,
    ledger: &[&slath_db::mr_ledger::Entry],
) -> Result<Vec<VersionRow>, AppError> {
    let stats = state.services.mr_version_stats(m, &change.versions).await?;
    let idents: Vec<slath_service::git_identities::CommitIdent<'_>> = stats
        .iter()
        .map(|s| slath_service::git_identities::CommitIdent {
            oid: s.oid.as_str(),
            author_email: &s.author_email,
            committer_email: "",
        })
        .collect();
    let people = state
        .services
        .commit_people(subject, a.repo.id, &idents)
        .await?;
    let mut stats: Vec<Option<VersionStat>> = stats
        .iter()
        .zip(people)
        .map(|(s, p)| {
            Some(VersionStat {
                author: s.author_name.clone(),
                author_account: p.author,
                time: time(s.author_time),
                files: s.files,
                added: s.added,
                removed: s.removed,
            })
        })
        .collect();
    stats.resize_with(change.versions.len(), || None);
    let threads = state.services.mr_threads(subject, a, m).await?;
    let oids: Vec<String> = change
        .versions
        .iter()
        .map(|v| v.commit_oid.clone())
        .collect();
    let statuses = state.services.statuses_of_commits(m, &oids).await?;
    Ok(change
        .versions
        .iter()
        .zip(stats)
        .map(|(v, stat)| VersionRow {
            stat,
            comments: threads
                .iter()
                .filter(|t| t.thread.anchor.new_oid.as_deref() == Some(v.commit_oid.as_str()))
                .map(|t| t.comments.iter().filter(|c| c.deleted_at.is_none()).count())
                .sum(),
            checks: statuses.get(&v.commit_oid).and_then(|s| checks_summary(s)),
            matched: ledger
                .iter()
                .rev()
                .find(|e| {
                    e.kind == "binding"
                        && e.revision_seq == Some(v.revision)
                        && e.detail.get("commit").and_then(serde_json::Value::as_str)
                            == Some(v.commit_oid.as_str())
                })
                .map_or_else(|| "recorded".to_owned(), |e| binding_text(e))
                .chars()
                .enumerate()
                .map(|(i, c)| if i == 0 { c.to_ascii_uppercase() } else { c })
                .collect(),
            v: v.clone(),
        })
        .collect())
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

/// Adds review features to a merge request diff: threads under their
/// lines, the comment form, and viewed files (collapsed while unchanged).
/// `commit`: a change's own diff, whose threads are those made on it.
async fn attach_review(
    state: &AppState,
    subject: &Subject,
    a: &RepoAccess,
    m: &MergeRequest,
    diff: &mut DiffView,
    dq: &DiffQuery,
    commit: Option<&str>,
) -> Result<(), AppError> {
    use slath_service::mr_threads::Position;
    if matches!(subject, Subject::Anonymous) {
        return Ok(());
    }
    let rc = RefContext {
        ns: a.repo.namespace.clone(),
        repo: a.repo.name.clone(),
    };
    let mut threads: std::collections::HashMap<
        (String, String, String),
        Vec<super::diff::InlineThread>,
    > = std::collections::HashMap::new();
    for t in state.services.mr_threads(subject, a, m).await? {
        let key = match (&t.position, commit) {
            (
                Position::Current {
                    path,
                    side,
                    line_end,
                    ..
                },
                None,
            ) => (path.clone(), side.clone(), line_end.to_string()),
            (_, Some(c)) if t.thread.anchor.new_oid.as_deref() == Some(c) => {
                let an = &t.thread.anchor;
                match (&an.path, &an.side, an.line_end) {
                    (Some(p), Some(sd), Some(e)) => (p.clone(), sd.clone(), e.to_string()),
                    _ => continue,
                }
            }
            _ => continue,
        };
        let mut comments = Vec::new();
        for (i, c) in t.comments.iter().enumerate() {
            let author = Who::account(c.author.clone());
            if i == 0 && t.thread.finding.is_some() {
                // A finding's message is plain text.
                comments.push((
                    author,
                    MarkupHtml::from_markup(String::new()),
                    false,
                    Some(c.body.clone()),
                ));
                continue;
            }
            let html = state
                .services
                .render_markdown(subject, Some(&rc), &c.body)
                .await?;
            comments.push((author, MarkupHtml::from_markup(html), c.pending, None));
        }
        threads
            .entry(key)
            .or_default()
            .push(super::diff::InlineThread {
                link: mr_url(
                    &a.repo.namespace,
                    &a.repo.name,
                    m.number,
                    &format!("#thread-{}", t.thread.id),
                ),
                id: t.thread.id.to_string(),
                finding: t.thread.finding.as_deref().map(finding_label),
                resolved: t.thread.resolved_at.is_some(),
                addressed_by: t
                    .thread
                    .addressed_at
                    .and(t.thread.addressed_by.clone())
                    .filter(|_| t.thread.resolved_at.is_none()),
                from: match t.position {
                    Position::Current { from, .. } => from,
                    _ => None,
                },
                comments,
            });
    }
    let viewed = state.services.viewed_files(subject, m).await?;
    for f in &mut diff.files {
        if viewed.contains(&f.path) {
            f.viewed = true;
            if dq.file != Some(f.index) {
                f.load = "click";
                f.hunks.clear();
            }
        }
    }
    diff.mark_review(&threads);
    let base = mr_url(&a.repo.namespace, &a.repo.name, m.number, "");
    diff.review = Some(super::diff::ReviewCtx {
        thread_post: format!("{base}/threads"),
        viewed_post: format!("{base}/viewed"),
        back: diff.href.clone(),
        can_comment: a.can(subject, Action::MrComment),
        threads,
        composer: dq.composer(),
        commit: commit.map(str::to_owned),
        revision: m.revisions,
    });
    Ok(())
}

#[derive(Deserialize)]
struct ThreadForm {
    #[serde(default)]
    body: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    side: String,
    #[serde(default)]
    line_start: String,
    #[serde(default)]
    line_end: String,
    #[serde(default)]
    revision: String,
    commit: Option<String>,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    back: String,
}

async fn new_thread(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<ThreadForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let start: Option<i32> = f.line_start.trim().parse().ok();
    let input = slath_service::mr_threads::NewThread {
        body: f.body,
        path: Some(f.path).filter(|p| !p.is_empty()),
        side: Some(f.side).filter(|p| !p.is_empty()),
        line_start: start,
        line_end: f.line_end.trim().parse().ok().or(start),
        revision: f.revision.trim().parse().ok(),
        commit: f.commit.filter(|c| !c.is_empty()),
        pending: f.mode == "pending",
    };
    match state
        .services
        .create_thread(&s.subject, (&ns, &repo, n), &input)
        .await
    {
        Ok(t) => Ok(Redirect::to(&back_to(
            &ns,
            &repo,
            n,
            &f.back,
            &format!("#thread-{}", t.thread.id),
        ))
        .into_response()),
        Err(ServiceError::Invalid(errors)) => {
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct ViewedForm {
    #[serde(default)]
    path: String,
    #[serde(default)]
    viewed: String,
    #[serde(default)]
    back: String,
}

async fn set_viewed(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<ViewedForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    state
        .services
        .set_file_viewed(&s.subject, (&ns, &repo, n), &f.path, f.viewed == "true")
        .await?;
    Ok(Redirect::to(&back_to(&ns, &repo, n, &f.back, "")).into_response())
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

#[derive(Deserialize)]
struct EditsForm {
    #[serde(default)]
    allow: String,
}

async fn set_maintainer_edits(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<EditsForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    state
        .services
        .set_maintainer_edits(&s.subject, &ns, &repo, n, f.allow == "true")
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
        .submit_review(&s.subject, (&ns, &repo, n), &f.verdict, &f.body, None)
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

/// "Check out" on a fork merge request: its branch `mr/<n>` shows in the
/// viewer's fetches from now on.
async fn note_checkout(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let (_, m) = state.services.mr(&s.subject, &ns, &repo, n).await?;
    state.services.note_checkout(&s.subject, &m).await?;
    Ok(Redirect::to(&mr_url(&ns, &repo, n, "?status=checkout")).into_response())
}

/// Applies the chosen suggestions (`comments`, or just `only` when a
/// suggestion's own button was used) in the chosen mode.
async fn apply_suggestions(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let pairs = crate::auth::form_pairs(&body);
    let get = |k: &str| pairs.iter().find(|(p, _)| p == k).map(|(_, v)| v.clone());
    let picked: Vec<String> = match get("only") {
        Some(one) => vec![one],
        None => pairs
            .iter()
            .filter(|(p, _)| p == "comments")
            .map(|(_, v)| v.clone())
            .collect(),
    };
    let ids: Vec<uuid::Uuid> = picked.iter().filter_map(|c| c.parse().ok()).collect();
    let mode = slath_service::suggestions::ApplyMode::parse(&get("mode").unwrap_or_default())?;
    let head = get("expected_head_oid");
    match state
        .services
        .apply_suggestions(&s.subject, (&ns, &repo, n), &ids, mode, head.as_deref())
        .await
    {
        Ok(_) => Ok(Redirect::to(&mr_url(&ns, &repo, n, "?status=applied")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            let errors = errors
                .into_iter()
                .map(|e| FieldError {
                    field: "suggestions".into(),
                    ..e
                })
                .collect();
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(ServiceError::Conflict(why)) => {
            let errors = vec![FieldError {
                field: "suggestions".into(),
                code: "conflict".into(),
                message: why,
            }];
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct AutoMergeForm {
    #[serde(default)]
    strategy: String,
}

async fn set_auto_merge(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<AutoMergeForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    match state
        .services
        .set_auto_merge(&s.subject, (&ns, &repo, n), Some(f.strategy.as_str()))
        .await
    {
        Ok(m) if m.state == "merged" => {
            Ok(Redirect::to(&mr_url(&ns, &repo, n, "?status=merged")).into_response())
        }
        Ok(_) => Ok(Redirect::to(&mr_url(&ns, &repo, n, "?status=auto_merge")).into_response()),
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

async fn cancel_auto_merge(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    state
        .services
        .cancel_auto_merge(&s.subject, (&ns, &repo, n))
        .await?;
    Ok(Redirect::to(&mr_url(&ns, &repo, n, "?status=auto_merge_cancelled")).into_response())
}

#[derive(Deserialize)]
struct UpdateBranchForm {
    #[serde(default)]
    method: String,
    #[serde(default)]
    expected_head_oid: String,
}

async fn update_branch(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<UpdateBranchForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    match state
        .services
        .update_mr_branch(
            &s.subject,
            (&ns, &repo, n),
            &f.method,
            Some(f.expected_head_oid.as_str()),
        )
        .await
    {
        Ok(_) => Ok(Redirect::to(&mr_url(&ns, &repo, n, "?status=updated")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(ServiceError::Conflict(why)) => {
            let errors = vec![FieldError {
                field: "update".into(),
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
    /// The reply box's Resolve thread checkbox.
    #[serde(default)]
    resolve: Option<String>,
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
            (f.mode == "pending", f.resolve.is_some()),
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
            get(old_commits),
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
            get(stack),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/files",
            Access::Resource,
            get(files),
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
            "/{ns}/{repo}/merge-requests/{n}/maintainer-edits",
            Access::Resource,
            post(set_maintainer_edits),
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
            "/{ns}/{repo}/merge-requests/{n}/threads",
            Access::Resource,
            post(new_thread),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/viewed",
            Access::Resource,
            post(set_viewed),
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
            "/{ns}/{repo}/merge-requests/{n}/auto-merge",
            Access::Resource,
            post(set_auto_merge),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/checkout",
            Access::Resource,
            post(note_checkout),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/suggestions/apply",
            Access::Resource,
            post(apply_suggestions),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/auto-merge/cancel",
            Access::Resource,
            post(cancel_auto_merge),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/update-branch",
            Access::Resource,
            post(update_branch),
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

#[cfg(test)]
mod tests {
    use super::{Card, UNVIEWED_SHOWN};

    fn card(files: usize, all: bool) -> Card {
        Card {
            headline: String::new(),
            reason: String::new(),
            tone: "waiting",
            action: None,
            rows: Vec::new(),
            since: None,
            mergeable: false,
            up_to_date: true,
            unviewed: (0..files).map(|i| format!("f{i}")).collect(),
            all_unviewed: all,
            unviewed_href: String::new(),
        }
    }

    #[test]
    fn the_card_names_five_unviewed_files_then_how_many_more() {
        let c = card(UNVIEWED_SHOWN + 7, false);
        assert_eq!(c.unviewed_shown().len(), UNVIEWED_SHOWN);
        assert_eq!(c.unviewed_more(), 7);
        let c = card(3, false);
        assert_eq!((c.unviewed_shown().len(), c.unviewed_more()), (3, 0));
        // One more than five is shown, not hidden behind "and 1 more".
        let c = card(UNVIEWED_SHOWN + 1, false);
        assert_eq!(
            (c.unviewed_shown().len(), c.unviewed_more()),
            (UNVIEWED_SHOWN + 1, 0)
        );
        let c = card(UNVIEWED_SHOWN + 2, false);
        assert_eq!(
            (c.unviewed_shown().len(), c.unviewed_more()),
            (UNVIEWED_SHOWN, 2)
        );
        let c = card(UNVIEWED_SHOWN + 7, true);
        assert_eq!(
            (c.unviewed_shown().len(), c.unviewed_more()),
            (UNVIEWED_SHOWN + 7, 0)
        );
    }
}
