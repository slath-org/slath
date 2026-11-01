// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/repos/{ns}/{repo}/issues`: issues and their comments.

use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use slath_db::issues::Filter;
use slath_markup::refs::RefContext;
use slath_service::issues::TimelineItem;
use utoipa::{IntoParams, ToSchema};

use super::support::{PageParams, page};
use super::{ApiSubject, ApiUser};
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

fn ts(t: time::OffsetDateTime) -> String {
    crate::ui::Time::from(t).iso
}

#[derive(Serialize, ToSchema)]
pub struct Issue {
    pub number: i32,
    pub title: String,
    /// Markdown source.
    pub body: String,
    /// The description rendered for the caller (references resolved).
    pub body_html: String,
    /// `open` or `closed`.
    pub state: String,
    /// `completed`, `not_planned`, or `duplicate` when closed.
    pub state_reason: Option<String>,
    /// `None` for deleted accounts.
    pub author: Option<String>,
    pub comments: i64,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

fn issue_json(i: slath_db::issues::Issue, body_html: String) -> Issue {
    Issue {
        number: i.number,
        title: i.title,
        body: i.body,
        body_html,
        state: i.state,
        state_reason: i.state_reason,
        author: i.author,
        comments: i.comments,
        created_at: ts(i.created_at),
        updated_at: ts(i.updated_at),
        closed_at: i.closed_at.map(ts),
    }
}

#[derive(Serialize, ToSchema)]
pub struct IssuePage {
    pub items: Vec<Issue>,
    pub next_cursor: Option<String>,
}

#[derive(Deserialize, IntoParams)]
pub struct IssueQuery {
    /// `open` (default), `closed`, or `all`.
    pub state: Option<String>,
    /// `created` (default) or `updated`.
    pub sort: Option<String>,
}

/// A repository's issues, newest first.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/issues", tag = "issues", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), IssueQuery, PageParams),
    responses((status = 200, body = IssuePage), (status = 404, body = super::Problem)))]
pub async fn list_issues(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    Query(q): Query<IssueQuery>,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let filter = Filter {
        state: match q.state.as_deref() {
            Some("all") => None,
            Some("closed") => Some("closed".to_owned()),
            _ => Some("open".to_owned()),
        },
        sort_updated: q.sort.as_deref() == Some("updated"),
        ..Filter::default()
    };
    let (a, items, _) = state
        .services
        .issue_list(&s, &ns, &repo, &filter, offset as i64, limit as i64 + 1)
        .await?;
    let ctx = RefContext {
        ns: a.repo.namespace.clone(),
        repo: a.repo.name.clone(),
    };
    let mut out = Vec::new();
    for i in items {
        let html = state
            .services
            .render_markdown(&s, Some(&ctx), &i.body)
            .await?;
        out.push(issue_json(i, html));
    }
    Ok(page(&state, &uri, out, offset, limit))
}

#[derive(Deserialize, ToSchema)]
pub struct NewIssue {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
}

/// Opens an issue.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/issues", tag = "issues", security(("token" = ["issues:write"])), params(("ns" = String, Path), ("repo" = String, Path)), request_body = NewIssue,
    responses((status = 201, body = Issue), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_issue(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    Json(b): Json<NewIssue>,
) -> Result<(StatusCode, Json<Issue>), AppError> {
    let i = state
        .services
        .create_issue(&s, &ns, &repo, &b.title, &b.body)
        .await?;
    let ctx = RefContext {
        ns: ns.clone(),
        repo: repo.clone(),
    };
    let html = state
        .services
        .render_markdown(&s, Some(&ctx), &i.body)
        .await?;
    Ok((StatusCode::CREATED, Json(issue_json(i, html))))
}

/// One issue.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/issues/{number}", tag = "issues", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = Issue), (status = 404, body = super::Problem)))]
pub async fn get_issue(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<Issue>, AppError> {
    let (a, i) = state.services.issue(&s, &ns, &repo, number).await?;
    let ctx = RefContext {
        ns: a.repo.namespace.clone(),
        repo: a.repo.name.clone(),
    };
    let html = state
        .services
        .render_markdown(&s, Some(&ctx), &i.body)
        .await?;
    Ok(Json(issue_json(i, html)))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateIssue {
    pub title: Option<String>,
    pub body: Option<String>,
    /// `open` or `closed`.
    pub state: Option<String>,
    /// With `state: closed`: `completed` (default), `not_planned`, `duplicate`.
    pub state_reason: Option<String>,
}

/// Edits an issue, or closes or reopens it (author or triage).
#[utoipa::path(patch, path = "/api/v1/repos/{ns}/{repo}/issues/{number}", tag = "issues", security(("token" = ["issues:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = UpdateIssue,
    responses((status = 200, body = Issue), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn update_issue(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<UpdateIssue>,
) -> Result<Json<Issue>, AppError> {
    let (_, cur) = state.services.issue(&s, &ns, &repo, number).await?;
    if b.title.is_some() || b.body.is_some() {
        let title = b.title.unwrap_or(cur.title);
        let body = b.body.unwrap_or(cur.body);
        state
            .services
            .edit_issue(&s, &ns, &repo, number, &title, &body)
            .await?;
    }
    if let Some(st) = b.state.as_deref() {
        let open = match st {
            "open" => true,
            "closed" => false,
            _ => {
                return Err(slath_service::ServiceError::invalid(
                    "state",
                    "invalid",
                    "Use open or closed.",
                )
                .into());
            }
        };
        state
            .services
            .set_issue_state(&s, &ns, &repo, number, open, b.state_reason.as_deref())
            .await?;
    }
    get_issue(State(state), ApiSubject(s), Path((ns, repo, number))).await
}

#[derive(Serialize, ToSchema)]
pub struct IssueComment {
    pub id: String,
    pub author: Option<String>,
    /// Empty when deleted or hidden from the caller.
    pub body: String,
    pub body_html: String,
    pub deleted: bool,
    pub hidden: bool,
    pub created_at: String,
    pub edited_at: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct CommentPage {
    pub items: Vec<IssueComment>,
    pub next_cursor: Option<String>,
}

/// An issue's comments, oldest first.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/issues/{number}/comments", tag = "issues", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path), PageParams),
    responses((status = 200, body = CommentPage), (status = 404, body = super::Problem)))]
pub async fn list_comments(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let (a, i) = state.services.issue(&s, &ns, &repo, number).await?;
    let items: Vec<IssueComment> = state
        .services
        .issue_timeline(&s, &a, &i)
        .await?
        .into_iter()
        .filter_map(|t| match t {
            TimelineItem::Comment {
                comment,
                html,
                hidden,
                ..
            } => {
                let visible = !html.is_empty();
                Some(IssueComment {
                    id: comment.id.to_string(),
                    author: comment.author,
                    body: if visible { comment.body } else { String::new() },
                    body_html: html,
                    deleted: comment.deleted_at.is_some(),
                    hidden: comment.hidden_at.is_some() && (hidden || !visible),
                    created_at: ts(comment.created_at),
                    edited_at: comment.edited_at.map(ts),
                })
            }
            TimelineItem::Event(..) => None,
        })
        .skip(offset)
        .take(limit + 1)
        .collect();
    Ok(page(&state, &uri, items, offset, limit))
}

#[derive(Deserialize, ToSchema)]
pub struct CommentBody {
    #[serde(default)]
    pub body: String,
}

/// Adds a comment.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/issues/{number}/comments", tag = "issues", security(("token" = ["issues:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = CommentBody,
    responses((status = 201, body = IssueComment), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_comment(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<CommentBody>,
) -> Result<(StatusCode, Json<IssueComment>), AppError> {
    let c = state
        .services
        .add_issue_comment(&s, &ns, &repo, number, &b.body)
        .await?;
    let ctx = RefContext { ns, repo };
    let html = state
        .services
        .render_markdown(&s, Some(&ctx), &c.body)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(IssueComment {
            id: c.id.to_string(),
            author: c.author,
            body: c.body,
            body_html: html,
            deleted: false,
            hidden: false,
            created_at: ts(c.created_at),
            edited_at: None,
        }),
    ))
}

fn comment_id(s: &str) -> Result<uuid::Uuid, AppError> {
    s.parse().map_err(|_| AppError::not_found())
}

/// Edits one's own comment.
#[utoipa::path(patch, path = "/api/v1/repos/{ns}/{repo}/issues/comments/{id}", tag = "issues", security(("token" = ["issues:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("id" = String, Path)), request_body = CommentBody,
    responses((status = 204), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn update_comment(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, id)): Path<(String, String, String)>,
    Json(b): Json<CommentBody>,
) -> Result<StatusCode, AppError> {
    state
        .services
        .edit_issue_comment(&s, &ns, &repo, comment_id(&id)?, &b.body)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes a comment (own, or any for maintainers).
#[utoipa::path(delete, path = "/api/v1/repos/{ns}/{repo}/issues/comments/{id}", tag = "issues", security(("token" = ["issues:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("id" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn delete_comment(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, id)): Path<(String, String, String)>,
) -> Result<StatusCode, AppError> {
    state
        .services
        .delete_issue_comment(&s, &ns, &repo, comment_id(&id)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as(
            "/api/v1/repos/{ns}/{repo}/issues",
            Access::Resource,
            get(list_issues).post(create_issue),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/issues/{number}",
            Access::Resource,
            get(get_issue).patch(update_issue),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/issues/{number}/comments",
            Access::Resource,
            get(list_comments).post(create_comment),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/issues/comments/{id}",
            Access::Resource,
            axum::routing::patch(update_comment).delete(delete_comment),
        )
}
