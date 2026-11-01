// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Issue pages: list, new, view with timeline, edit, close/reopen, and
//! comments.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::authz::{Action, Subject};
use slath_db::issues::Filter;
use slath_markup::refs::RefContext;
use slath_service::issues::{IssuePerms, TimelineItem};
use slath_service::repos::RepoAccess;
use slath_service::{FieldError, ServiceError};

use super::repos::RepoHeader;
use super::settings::error_for;
use super::{MarkupHtml, PageCtx, Time, render};
use crate::AppState;
use crate::auth::{MaybeSession, RequireSession};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

const PER_PAGE: i64 = 50;

fn issue_url(a: &RepoAccess, n: i32, rest: &str) -> String {
    format!("/{}/{}/issues/{n}{rest}", a.repo.namespace, a.repo.name)
}

fn ref_ctx(a: &RepoAccess) -> RefContext {
    RefContext {
        ns: a.repo.namespace.clone(),
        repo: a.repo.name.clone(),
    }
}

struct IssueRow {
    number: i32,
    title: String,
    state: String,
    icon: &'static str,
    author: Option<String>,
    created: Time,
    comments: i64,
}

#[derive(Template)]
#[template(path = "repo/issues.html")]
struct ListPage {
    ctx: PageCtx,
    header: RepoHeader,
    issues: Vec<IssueRow>,
    state: String,
    open: i64,
    closed: i64,
    can_create: bool,
    next: Option<String>,
}

#[derive(Deserialize, Default)]
struct ListQuery {
    state: Option<String>,
    page: Option<i64>,
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
        Some("closed") => "closed",
        Some("all") => "all",
        _ => "open",
    };
    let page = q.page.unwrap_or(1).clamp(1, 10_000);
    let filter = Filter {
        state: (st != "all").then(|| st.to_owned()),
        ..Filter::default()
    };
    let (a, items, (open, closed)) = state
        .services
        .issue_list(
            &subject,
            &ns,
            &repo,
            &filter,
            (page - 1) * PER_PAGE,
            PER_PAGE + 1,
        )
        .await?;
    let more = items.len() as i64 > PER_PAGE;
    Ok(render(&ListPage {
        header: RepoHeader::new(&a, &subject, "issues"),
        can_create: a.can(&subject, Action::IssueCreate),
        issues: items
            .into_iter()
            .take(PER_PAGE as usize)
            .map(|i| IssueRow {
                icon: if i.state == "open" {
                    "circle-dot"
                } else {
                    "circle-check"
                },
                number: i.number,
                title: i.title,
                state: i.state,
                author: i.author,
                created: i.created_at.into(),
                comments: i.comments,
            })
            .collect(),
        state: st.to_owned(),
        open,
        closed,
        next: more.then(|| format!("?state={st}&page={}", page + 1)),
        ctx,
    }))
}

#[derive(Template)]
#[template(path = "repo/issue_new.html")]
struct NewPage {
    ctx: PageCtx,
    header: RepoHeader,
    title: String,
    body: String,
    errors: Vec<FieldError>,
}

impl NewPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn new_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let a = state
        .services
        .repo_access(&s.subject, &ns, &repo, Action::IssueCreate)
        .await?;
    if !a.repo.issues_enabled {
        return Err(AppError::not_found());
    }
    Ok(render(&NewPage {
        header: RepoHeader::new(&a, &s.subject, "issues"),
        ctx,
        title: String::new(),
        body: String::new(),
        errors: Vec::new(),
    }))
}

#[derive(Deserialize)]
struct IssueForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
}

async fn create(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<IssueForm>,
) -> Result<Response, AppError> {
    match state
        .services
        .create_issue(&s.subject, &ns, &repo, &f.title, &f.body)
        .await
    {
        Ok(issue) => {
            Ok(Redirect::to(&format!("/{ns}/{repo}/issues/{}", issue.number)).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            let a = state
                .services
                .repo_access(&s.subject, &ns, &repo, Action::RepoRead)
                .await?;
            let mut res = render(&NewPage {
                header: RepoHeader::new(&a, &s.subject, "issues"),
                ctx,
                title: f.title,
                body: f.body,
                errors,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        Err(e) => Err(e.into()),
    }
}

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
    link: Option<String>,
    link_text: String,
    created: Time,
}

enum TimelineView {
    Comment(CommentView),
    Event(EventView),
}

fn event_view(e: slath_db::issues::Event, source: Option<String>) -> Option<EventView> {
    let str_of = |k: &str| {
        e.data
            .get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let (icon, text, link, link_text) = match e.kind.as_str() {
        "closed" => (
            "circle-check",
            match str_of("reason").as_str() {
                "not_planned" => "closed this as not planned".to_owned(),
                "duplicate" => "closed this as a duplicate".to_owned(),
                _ => "closed this as completed".to_owned(),
            },
            None,
            String::new(),
        ),
        "reopened" => (
            "circle-dot",
            "reopened this".to_owned(),
            None,
            String::new(),
        ),
        "renamed" => (
            "pencil",
            format!(
                "changed the title from “{}” to “{}”",
                str_of("from"),
                str_of("to")
            ),
            None,
            String::new(),
        ),
        "referenced" => {
            let s = source?;
            let (path, n) = s.rsplit_once('#')?;
            (
                "link",
                "mentioned this in".to_owned(),
                Some(format!("/{path}/issues/{n}")),
                s,
            )
        }
        "labeled" => (
            "tag",
            format!("added the label {}", str_of("label")),
            None,
            String::new(),
        ),
        "unlabeled" => (
            "tag",
            format!("removed the label {}", str_of("label")),
            None,
            String::new(),
        ),
        "assigned" => (
            "user",
            format!("assigned {}", str_of("user")),
            None,
            String::new(),
        ),
        "unassigned" => (
            "user",
            format!("unassigned {}", str_of("user")),
            None,
            String::new(),
        ),
        "milestoned" => (
            "flag",
            format!("added this to the milestone {}", str_of("milestone")),
            None,
            String::new(),
        ),
        "demilestoned" => (
            "flag",
            format!("removed this from the milestone {}", str_of("milestone")),
            None,
            String::new(),
        ),
        _ => return None,
    };
    Some(EventView {
        icon,
        actor: e.actor.unwrap_or_else(|| "A deleted user".to_owned()),
        text,
        link,
        link_text,
        created: e.created_at.into(),
    })
}

#[derive(Template)]
#[template(path = "repo/issue.html")]
struct IssuePage {
    ctx: PageCtx,
    header: RepoHeader,
    issue: slath_db::issues::Issue,
    created: Time,
    state_icon: &'static str,
    reason_text: &'static str,
    body_html: MarkupHtml,
    body_empty: bool,
    timeline: Vec<TimelineView>,
    perms: IssuePerms,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
}

#[derive(Deserialize, Default)]
struct StatusQuery {
    status: Option<String>,
}

async fn issue_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    (ns, repo, number): (&str, &str, i32),
    message: Option<&'static str>,
    errors: Vec<FieldError>,
) -> Result<Response, AppError> {
    let (a, issue) = state.services.issue(subject, ns, repo, number).await?;
    let rc = ref_ctx(&a);
    let body_html = state
        .services
        .render_markdown(subject, Some(&rc), &issue.body)
        .await?;
    let mut timeline = Vec::new();
    for item in state.services.issue_timeline(subject, &a, &issue).await? {
        match item {
            TimelineItem::Comment {
                comment,
                html,
                hidden,
                own,
            } => timeline.push(TimelineView::Comment(CommentView {
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
            TimelineItem::Event(e, source) => {
                if let Some(v) = event_view(e, source) {
                    timeline.push(TimelineView::Event(v));
                }
            }
        }
    }
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&IssuePage {
        header: RepoHeader::new(&a, subject, "issues"),
        perms: state.services.issue_perms(subject, &a, &issue),
        created: issue.created_at.into(),
        state_icon: if issue.state == "open" {
            "circle-dot"
        } else {
            "circle-check"
        },
        reason_text: match issue.state_reason.as_deref() {
            Some("not_planned") => "not planned",
            Some("duplicate") => "duplicate",
            _ => "completed",
        },
        body_empty: body_html.trim().is_empty(),
        body_html: MarkupHtml::from_markup(body_html),
        timeline,
        issue,
        message,
        errors,
        ctx,
    });
    *res.status_mut() = status;
    Ok(res)
}

fn number(s: &str) -> Result<i32, AppError> {
    s.parse::<i32>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(AppError::not_found)
}

async fn view(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let message = match q.status.as_deref() {
        Some("saved") => Some("Issue saved."),
        _ => None,
    };
    issue_page(
        &state,
        ctx,
        &session.subject(),
        (&ns, &repo, number(&n)?),
        message,
        Vec::new(),
    )
    .await
}

/// `/{ns}/{repo}/{n}`: a number leads to the issue.
async fn by_number(
    Path((ns, repo, n)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    Ok(Redirect::to(&format!("/{ns}/{repo}/issues/{n}")).into_response())
}

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

async fn edit_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let (a, issue) = state
        .services
        .issue(&s.subject, &ns, &repo, number(&n)?)
        .await?;
    if !state.services.issue_perms(&s.subject, &a, &issue).edit {
        return Err(AppError::forbidden());
    }
    Ok(render(&EditPage {
        header: RepoHeader::new(&a, &s.subject, "issues"),
        heading: format!("Edit issue #{}", issue.number),
        action: issue_url(&a, issue.number, "/edit"),
        back: issue_url(&a, issue.number, ""),
        title: Some(issue.title),
        body: issue.body,
        errors: Vec::new(),
        ctx,
    }))
}

async fn edit(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<IssueForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    match state
        .services
        .edit_issue(&s.subject, &ns, &repo, n, &f.title, &f.body)
        .await
    {
        Ok(()) => {
            Ok(Redirect::to(&format!("/{ns}/{repo}/issues/{n}?status=saved")).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            let (a, _) = state.services.issue(&s.subject, &ns, &repo, n).await?;
            let mut res = render(&EditPage {
                header: RepoHeader::new(&a, &s.subject, "issues"),
                heading: format!("Edit issue #{n}"),
                action: issue_url(&a, n, "/edit"),
                back: issue_url(&a, n, ""),
                title: Some(f.title),
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

#[derive(Deserialize)]
struct StateForm {
    #[serde(default)]
    state: String,
    reason: Option<String>,
}

async fn set_state(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<StateForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    state
        .services
        .set_issue_state(
            &s.subject,
            &ns,
            &repo,
            n,
            f.state == "open",
            f.reason.as_deref(),
        )
        .await?;
    Ok(Redirect::to(&format!("/{ns}/{repo}/issues/{n}")).into_response())
}

#[derive(Deserialize)]
struct CommentForm {
    #[serde(default)]
    body: String,
}

async fn comment(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<CommentForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    match state
        .services
        .add_issue_comment(&s.subject, &ns, &repo, n, &f.body)
        .await
    {
        Ok(c) => {
            Ok(Redirect::to(&format!("/{ns}/{repo}/issues/{n}#comment-{}", c.id)).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            issue_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
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
    let (a, issue) = state.services.issue(&s.subject, &ns, &repo, n).await?;
    let c = state
        .services
        .issue_timeline(&s.subject, &a, &issue)
        .await?
        .into_iter()
        .find_map(|t| match t {
            TimelineItem::Comment {
                comment, own: true, ..
            } if comment.id == id && comment.deleted_at.is_none() => Some(comment),
            _ => None,
        })
        .ok_or_else(AppError::not_found)?;
    Ok(render(&EditPage {
        header: RepoHeader::new(&a, &s.subject, "issues"),
        heading: "Edit comment".to_owned(),
        action: issue_url(&a, n, &format!("/comments/{id}/edit")),
        back: issue_url(&a, n, &format!("#comment-{id}")),
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
    match state
        .services
        .edit_issue_comment(&s.subject, &ns, &repo, id, &f.body)
        .await
    {
        Ok(()) => {
            Ok(Redirect::to(&format!("/{ns}/{repo}/issues/{n}#comment-{id}")).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            let (a, _) = state.services.issue(&s.subject, &ns, &repo, n).await?;
            let mut res = render(&EditPage {
                header: RepoHeader::new(&a, &s.subject, "issues"),
                heading: "Edit comment".to_owned(),
                action: issue_url(&a, n, &format!("/comments/{id}/edit")),
                back: issue_url(&a, n, &format!("#comment-{id}")),
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
    match op.as_str() {
        "delete" => {
            state
                .services
                .delete_issue_comment(&s.subject, &ns, &repo, id)
                .await?
        }
        "hide" => {
            state
                .services
                .hide_issue_comment(&s.subject, &ns, &repo, id, true)
                .await?
        }
        "unhide" => {
            state
                .services
                .hide_issue_comment(&s.subject, &ns, &repo, id, false)
                .await?
        }
        _ => return Err(AppError::not_found()),
    }
    Ok(Redirect::to(&format!("/{ns}/{repo}/issues/{n}")).into_response())
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as(
            "/{ns}/{repo}/issues",
            Access::Resource,
            get(list).post(create),
        )
        .route_as("/{ns}/{repo}/issues/new", Access::Resource, get(new_page))
        .route_as("/{ns}/{repo}/issues/{n}", Access::Resource, get(view))
        .route_as(
            "/{ns}/{repo}/issues/{n}/edit",
            Access::Resource,
            get(edit_page).post(edit),
        )
        .route_as(
            "/{ns}/{repo}/issues/{n}/state",
            Access::Resource,
            post(set_state),
        )
        .route_as(
            "/{ns}/{repo}/issues/{n}/comments",
            Access::Resource,
            post(comment),
        )
        .route_as(
            "/{ns}/{repo}/issues/{n}/comments/{id}/edit",
            Access::Resource,
            get(comment_edit_page).post(comment_edit),
        )
        .route_as(
            "/{ns}/{repo}/issues/{n}/comments/{id}/{op}",
            Access::Resource,
            post(comment_op),
        )
        .route_as("/{ns}/{repo}/{n}", Access::Resource, get(by_number))
}
