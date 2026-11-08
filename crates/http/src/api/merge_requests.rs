// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/repos/{ns}/{repo}/merge-requests`: merge requests, their revisions,
//! and their comments.

use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use slath_markup::refs::RefContext;
use slath_service::issues::TimelineItem;
use slath_service::labels::FilterInput;
use slath_service::merge_requests::{MrTimelineItem, NewMr};
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
pub struct MergeRequest {
    pub number: i32,
    pub title: String,
    /// Markdown source.
    pub description: String,
    /// The description rendered for the caller.
    pub description_html: String,
    /// `open`, `merged`, or `closed`.
    pub state: String,
    pub draft: bool,
    /// `None` for deleted accounts.
    pub author: Option<String>,
    pub source_branch: String,
    pub target_branch: String,
    /// Head commit of the current revision.
    pub head_oid: Option<String>,
    /// Number of revisions (pushes that changed the source).
    pub revisions: i32,
    pub comments: i64,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
}

fn mr_json(m: slath_db::merge_requests::MergeRequest, html: String) -> MergeRequest {
    MergeRequest {
        source_branch: m.source_branch(),
        number: m.number,
        title: m.title,
        description: m.description,
        description_html: html,
        state: m.state,
        draft: m.draft,
        author: m.author,
        target_branch: m.target_branch,
        head_oid: m.head_oid,
        revisions: m.revisions,
        comments: m.comments,
        created_at: ts(m.created_at),
        updated_at: ts(m.updated_at),
        closed_at: m.closed_at.map(ts),
        merged_at: m.merged_at.map(ts),
    }
}

async fn render(
    state: &AppState,
    s: &slath_core::authz::Subject,
    ns: &str,
    repo: &str,
    m: slath_db::merge_requests::MergeRequest,
) -> Result<MergeRequest, AppError> {
    let ctx = RefContext {
        ns: ns.to_owned(),
        repo: repo.to_owned(),
    };
    let html = state
        .services
        .render_markdown(s, Some(&ctx), &m.description)
        .await?;
    Ok(mr_json(m, html))
}

#[derive(Serialize, ToSchema)]
pub struct MrPage {
    pub items: Vec<MergeRequest>,
    pub next_cursor: Option<String>,
}

#[derive(Deserialize, IntoParams)]
pub struct MrQuery {
    /// `open` (default), `merged`, `closed`, or `all`.
    pub state: Option<String>,
    /// `created` (default) or `updated`.
    pub sort: Option<String>,
    /// Comma-separated label names; merge requests must have all of them.
    pub labels: Option<String>,
    /// Username.
    pub assignee: Option<String>,
    /// Username.
    pub author: Option<String>,
    /// Milestone title.
    pub milestone: Option<String>,
}

/// A repository's merge requests, newest first.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), MrQuery, PageParams),
    responses((status = 200, body = MrPage), (status = 404, body = super::Problem)))]
pub async fn list_mrs(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    Query(q): Query<MrQuery>,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let filter = FilterInput {
        state: Some(q.state.unwrap_or_else(|| "open".to_owned())),
        sort_updated: q.sort.as_deref() == Some("updated"),
        labels: q
            .labels
            .as_deref()
            .unwrap_or("")
            .split(',')
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect(),
        assignee: q.assignee,
        author: q.author,
        milestone: q.milestone,
    };
    let (_, items, _) = state
        .services
        .mr_list(&s, &ns, &repo, &filter, offset as i64, limit as i64 + 1)
        .await?;
    let mut out = Vec::new();
    for m in items {
        out.push(render(&state, &s, &ns, &repo, m).await?);
    }
    Ok(page(&state, &uri, out, offset, limit))
}

#[derive(Deserialize, ToSchema)]
pub struct NewMergeRequest {
    #[serde(default)]
    pub source_branch: String,
    /// Default: the repository's default branch.
    pub target_branch: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub draft: bool,
    /// Usernames.
    #[serde(default)]
    pub reviewers: Vec<String>,
    /// Usernames.
    #[serde(default)]
    pub assignees: Vec<String>,
    /// Label names.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Milestone title.
    pub milestone: Option<String>,
    /// Default: the repository's setting.
    pub delete_source_on_merge: Option<bool>,
}

/// Opens a merge request from a branch of this repository.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/merge-requests", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path)), request_body = NewMergeRequest,
    responses((status = 201, body = MergeRequest), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_mr(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    Json(b): Json<NewMergeRequest>,
) -> Result<(StatusCode, Json<MergeRequest>), AppError> {
    let m = state
        .services
        .create_mr(
            &s,
            &ns,
            &repo,
            &NewMr {
                source_branch: b.source_branch,
                target_branch: b.target_branch.unwrap_or_default(),
                title: b.title,
                description: b.description,
                draft: b.draft,
                reviewers: b.reviewers,
                assignees: b.assignees,
                labels: b.labels,
                milestone: b.milestone,
                delete_source_on_merge: b.delete_source_on_merge,
            },
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(render(&state, &s, &ns, &repo, m).await?),
    ))
}

/// One merge request.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = MergeRequest), (status = 404, body = super::Problem)))]
pub async fn get_mr(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<MergeRequest>, AppError> {
    let (_, m) = state.services.mr(&s, &ns, &repo, number).await?;
    Ok(Json(render(&state, &s, &ns, &repo, m).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateMergeRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    /// `open` or `closed`.
    pub state: Option<String>,
    pub draft: Option<bool>,
}

/// Edits, closes or reopens, or marks a merge request draft or ready
/// (author or triage).
#[utoipa::path(patch, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = UpdateMergeRequest,
    responses((status = 200, body = MergeRequest), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn update_mr(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<UpdateMergeRequest>,
) -> Result<Json<MergeRequest>, AppError> {
    let (_, cur) = state.services.mr(&s, &ns, &repo, number).await?;
    if b.title.is_some() || b.description.is_some() {
        let title = b.title.unwrap_or(cur.title);
        let description = b.description.unwrap_or(cur.description);
        state
            .services
            .edit_mr(&s, &ns, &repo, number, &title, &description)
            .await?;
    }
    if let Some(d) = b.draft {
        state
            .services
            .set_mr_draft(&s, &ns, &repo, number, d)
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
            .set_mr_open(&s, &ns, &repo, number, open)
            .await?;
    }
    let (_, m) = state.services.mr(&s, &ns, &repo, number).await?;
    Ok(Json(render(&state, &s, &ns, &repo, m).await?))
}

#[derive(Serialize, ToSchema)]
pub struct MrComment {
    pub id: String,
    pub author: Option<String>,
    pub body: String,
    pub body_html: String,
    pub created_at: String,
    pub edited_at: Option<String>,
}

/// The conversation's comments, oldest first.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/comments", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = Vec<MrComment>), (status = 404, body = super::Problem)))]
pub async fn list_mr_comments(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<Vec<MrComment>>, AppError> {
    let (a, m) = state.services.mr(&s, &ns, &repo, number).await?;
    let items = state
        .services
        .mr_timeline(&s, &a, &m)
        .await?
        .into_iter()
        .filter_map(|i| match i {
            MrTimelineItem::Comment(TimelineItem::Comment {
                comment,
                html,
                hidden,
                ..
            }) if comment.deleted_at.is_none() && !(hidden && html.is_empty()) => Some(MrComment {
                id: comment.id.to_string(),
                author: comment.author,
                body: comment.body,
                body_html: html,
                created_at: ts(comment.created_at),
                edited_at: comment.edited_at.map(ts),
            }),
            _ => None,
        })
        .collect();
    Ok(Json(items))
}

#[derive(Deserialize, ToSchema)]
pub struct MrCommentBody {
    #[serde(default)]
    pub body: String,
}

/// Comments on a merge request.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/comments", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = MrCommentBody,
    responses((status = 201, body = MrComment), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_mr_comment(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<MrCommentBody>,
) -> Result<(StatusCode, Json<MrComment>), AppError> {
    let c = state
        .services
        .add_mr_comment(&s, &ns, &repo, number, &b.body)
        .await?;
    let ctx = RefContext {
        ns: ns.clone(),
        repo: repo.clone(),
    };
    let html = state
        .services
        .render_markdown(&s, Some(&ctx), &c.body)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(MrComment {
            id: c.id.to_string(),
            author: c.author,
            body: c.body,
            body_html: html,
            created_at: ts(c.created_at),
            edited_at: c.edited_at.map(ts),
        }),
    ))
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests",
            Access::Resource,
            get(list_mrs).post(create_mr),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}",
            Access::Resource,
            get(get_mr).patch(update_mr),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/comments",
            Access::Resource,
            get(list_mr_comments).post(create_mr_comment),
        )
}
