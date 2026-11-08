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
use slath_service::mr_changes::{Rebind, StackEntry};
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

enum TimelineView {
    Comment(CommentView),
    Event(EventView),
    Revision(RevisionView),
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
    Some(EventView {
        icon,
        actor: e.actor.unwrap_or_else(|| "A deleted user".to_owned()),
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
    Ok(MrHeading {
        perms: state.services.mr_perms(subject, a, &m),
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
}

impl MrPage {
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

async fn mr_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    (ns, repo, number): (&str, &str, i32),
    message: Option<&'static str>,
    errors: Vec<FieldError>,
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
    let mut timeline = Vec::new();
    for item in state.services.mr_timeline(subject, &a, &m).await? {
        match item {
            MrTimelineItem::Comment(TimelineItem::Comment {
                comment,
                html,
                hidden,
                own,
            }) => timeline.push(TimelineView::Comment(CommentView {
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
                since_href: previous_head
                    .is_some()
                    .then(|| mr_url(ns, repo, m.number, &format!("/changes?since={}", r.seq - 1))),
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
    let (assignees, reviewers, labels, milestone) = state.services.mr_details(&m).await?;
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
        h: heading(state, subject, &a, m).await?,
        body_empty: body_html.trim().is_empty(),
        body_html: MarkupHtml::from_markup(body_html),
        timeline,
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
        _ => None,
    };
    mr_page(
        &state,
        ctx,
        &session.subject(),
        (&ns, &repo, number(&n)?),
        message,
        Vec::new(),
    )
    .await
}

/// A commit with the change it belongs to.
struct ChangeRow {
    commit: CommitRow,
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
            ChangeRow {
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
