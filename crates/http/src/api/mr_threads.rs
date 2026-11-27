// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Review threads of merge requests: line comments, replies, pending
//! comments, and resolving.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use slath_service::mr_threads::{NewThread, Position, ThreadView};
use utoipa::{IntoParams, ToSchema};

use super::{ApiSubject, ApiUser};
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

fn ts(t: time::OffsetDateTime) -> String {
    crate::ui::Time::from(t).iso
}

/// A comment of a thread.
#[derive(Serialize, ToSchema)]
pub struct ThreadCommentBody {
    pub id: String,
    /// Always `true`: this object's text is user-generated.
    pub untrusted_content: bool,
    /// Who wrote it.
    pub origin: super::TextOrigin,
    pub author: Option<String>,
    pub body: String,
    /// Only its author sees it until their review is submitted.
    pub pending: bool,
    pub created_at: String,
}

/// Where a thread shows in the current revision.
#[derive(Serialize, ToSchema)]
pub struct ThreadPosition {
    /// `general` (the whole merge request), `current`, or `outdated`.
    pub state: &'static str,
    pub path: Option<String>,
    /// `old` or `new`.
    pub side: Option<String>,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    /// The revision it was made on, when it's carried from an earlier one.
    pub from_revision: Option<i32>,
}

/// A review thread.
#[derive(Serialize, ToSchema)]
pub struct ThreadBody {
    pub id: String,
    /// The revision it was made on, and its anchor there.
    pub revision: i32,
    pub path: Option<String>,
    pub side: Option<String>,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    pub author: Option<String>,
    pub resolved: bool,
    pub resolved_by: Option<String>,
    /// Who marked it addressed (bots do, for threads people started); it
    /// isn't resolved. Cleared by resolving, reopening, or a reply by
    /// anyone else.
    pub addressed_by: Option<String>,
    pub addressed_at: Option<String>,
    /// While it's marked addressed: what Git says about that claim.
    /// `null` for threads on the whole merge request or on a brief, which
    /// have no lines to compare.
    pub addressed_check: Option<AddressedCheck>,
    pub position: ThreadPosition,
    pub comments: Vec<ThreadCommentBody>,
    pub created_at: String,
    /// Set when a commit status's annotation made the thread: its first
    /// comment is the reporter's plain-text message. Findings stay on
    /// their revision and never count as verdicts or requirements.
    pub finding: Option<ThreadFinding>,
    /// Set for a thread on an item of a change's brief (no path then).
    pub brief: Option<ThreadBrief>,
}

/// Whether a thread marked addressed had its lines changed.
#[derive(Serialize, ToSchema)]
pub struct AddressedCheck {
    /// The lines it's on changed since its revision: it's outdated on the
    /// current one. `false` while they're still there, unchanged.
    pub lines_changed: bool,
}

/// The brief item a thread is on.
#[derive(Serialize, ToSchema)]
pub struct ThreadBrief {
    /// The brief's edit (see `…/changes/{change}/briefs`).
    pub id: String,
    /// The change's display ID, and the number of its version.
    pub change: String,
    pub version: Option<usize>,
    /// The item's index in the edit's `items`, and its kind.
    pub item: usize,
    pub kind: String,
    /// The change's current brief (its latest version's newest edit) still
    /// has an item of this kind with this text; once it doesn't, the
    /// thread is on an earlier brief.
    pub current: bool,
}

/// What a finding thread came from.
#[derive(Serialize, ToSchema)]
pub struct ThreadFinding {
    /// Always `true`: this object's text is user-generated.
    pub untrusted_content: bool,
    /// Who wrote it.
    pub origin: super::TextOrigin,
    /// Who reported the status (a user or bot); `null` once that account
    /// is gone.
    pub reporter: Option<String>,
    /// The status context that reported it.
    pub context: String,
    /// `notice`, `warning`, or `failure`.
    pub severity: String,
    pub category: String,
    /// `open`, `resolved`, or `stale`: unresolved while its revision is no
    /// longer the current one. A stale finding never comes back; a newer
    /// revision gets findings of its own.
    pub state: &'static str,
}

impl From<ThreadView> for ThreadBody {
    fn from(v: ThreadView) -> Self {
        let addressed_check = v
            .addressed_check()
            .map(|lines_changed| AddressedCheck { lines_changed });
        let position = match v.position {
            Position::General => ThreadPosition {
                state: "general",
                path: None,
                side: None,
                line_start: None,
                line_end: None,
                from_revision: None,
            },
            Position::Outdated => ThreadPosition {
                state: "outdated",
                path: None,
                side: None,
                line_start: None,
                line_end: None,
                from_revision: None,
            },
            Position::Current {
                path,
                side,
                line_start,
                line_end,
                from,
            } => ThreadPosition {
                state: "current",
                path: Some(path),
                side: Some(side),
                line_start: Some(line_start),
                line_end: Some(line_end),
                from_revision: from,
            },
        };
        let brief = v.brief.map(|b| ThreadBrief {
            id: b.brief_id.to_string(),
            change: b.change,
            version: b.version,
            item: b.item,
            kind: b.kind.as_str().to_owned(),
            current: b.current,
        });
        let t = v.thread;
        // A finding's text comes from its status; its first comment has
        // the reporter's origin.
        let finding_origin = v
            .comments
            .first()
            .map_or(slath_core::origin::Origin::Anonymous, |c| c.origin);
        let reporter = t.author.clone();
        Self {
            id: t.id.to_string(),
            revision: t.revision_seq,
            path: t.anchor.path,
            side: t.anchor.side,
            line_start: t.anchor.line_start,
            line_end: t.anchor.line_end,
            author: t.author,
            resolved: t.resolved_at.is_some(),
            resolved_by: t.resolved_by,
            addressed_by: t.addressed_at.and(t.addressed_by),
            addressed_at: t.addressed_at.map(ts),
            addressed_check,
            position,
            comments: v
                .comments
                .into_iter()
                .map(|c| ThreadCommentBody {
                    untrusted_content: true,
                    origin: c.origin.into(),
                    id: c.id.to_string(),
                    author: c.author,
                    body: c.body,
                    pending: c.pending,
                    created_at: ts(c.created_at),
                })
                .collect(),
            created_at: ts(t.created_at),
            brief,
            finding: t.finding.map(|f| ThreadFinding {
                untrusted_content: true,
                origin: finding_origin.into(),
                reporter,
                context: f.context,
                severity: f.severity,
                category: f.category,
                state: v.finding_state.unwrap_or("open"),
            }),
        }
    }
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ThreadQuery {
    /// Only resolved (`true`) or unresolved (`false`) threads.
    pub resolved: Option<bool>,
}

/// The threads the caller may see, with their position in the current
/// revision.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/threads", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path), ThreadQuery),
    responses((status = 200, body = Vec<ThreadBody>), (status = 404, body = super::Problem)))]
pub async fn list_threads(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Query(q): Query<ThreadQuery>,
) -> Result<Json<Vec<ThreadBody>>, AppError> {
    let (a, m) = state.services.mr(&s, &ns, &repo, number).await?;
    let list = state.services.mr_threads(&s, &a, &m).await?;
    Ok(Json(
        list.into_iter()
            .filter(|t| {
                q.resolved
                    .is_none_or(|r| t.thread.resolved_at.is_some() == r)
            })
            .map(Into::into)
            .collect(),
    ))
}

#[derive(Deserialize, ToSchema)]
pub struct NewThreadBody {
    #[serde(default)]
    pub body: String,
    /// Without a path, a thread on the whole merge request.
    pub path: Option<String>,
    /// `old` or `new` (default).
    pub side: Option<String>,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    /// Default: the current revision.
    pub revision: Option<i32>,
    /// A version's commit, for a thread on one change's own diff.
    pub commit: Option<String>,
    /// Keep it in your pending review until you submit it.
    #[serde(default)]
    pub pending: bool,
    /// An item of a change brief's edit, in place of a path.
    pub brief: Option<NewThreadBrief>,
}

/// The brief item a new thread is on.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewThreadBrief {
    /// The brief's edit.
    pub id: uuid::Uuid,
    /// The item's index in the edit's `items`, from 0.
    pub item: usize,
}

/// Starts a thread on lines of a revision's diff, on an item of a change's
/// brief, or on the whole merge request.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/threads", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = NewThreadBody,
    responses((status = 201, body = ThreadBody), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_thread(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<NewThreadBody>,
) -> Result<(StatusCode, Json<ThreadBody>), AppError> {
    let v = state
        .services
        .create_thread(
            &s,
            (&ns, &repo, number),
            &NewThread {
                body: b.body,
                path: b.path,
                side: b.side,
                line_start: b.line_start,
                line_end: b.line_end,
                revision: b.revision,
                commit: b.commit,
                pending: b.pending,
                brief: b.brief.map(|x| (x.id, x.item)),
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(v.into())))
}

#[derive(Deserialize, ToSchema)]
pub struct ReplyBody {
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub pending: bool,
    /// Also resolve the thread: now, or with a pending reply when its
    /// review is submitted. Needs the right to resolve it.
    #[serde(default)]
    pub resolve: bool,
}

/// Answers in a thread; with `resolve`, also resolves it (a bot can't
/// resolve a thread a person started: 409).
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/threads/{id}/comments", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path), ("id" = String, Path)), request_body = ReplyBody,
    responses((status = 201, body = ThreadCommentBody), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn reply(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number, id)): Path<(String, String, i32, String)>,
    Json(b): Json<ReplyBody>,
) -> Result<(StatusCode, Json<ThreadCommentBody>), AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    let c = state
        .services
        .reply(
            &s,
            (&ns, &repo, number),
            id,
            &b.body,
            (b.pending, b.resolve),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(ThreadCommentBody {
            untrusted_content: true,
            origin: c.origin.into(),
            id: c.id.to_string(),
            author: c.author,
            body: c.body,
            pending: c.pending,
            created_at: ts(c.created_at),
        }),
    ))
}

#[derive(Deserialize, ToSchema)]
pub struct ThreadUpdate {
    pub resolved: Option<bool>,
}

/// Resolves or reopens a thread (the merge request's author, the thread's
/// author, or someone who can push). A bot can't settle a thread a person
/// started (409): it marks it addressed instead.
#[utoipa::path(patch, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/threads/{id}", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path), ("id" = String, Path)), request_body = ThreadUpdate,
    responses((status = 204), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem)))]
pub async fn update_thread(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number, id)): Path<(String, String, i32, String)>,
    Json(b): Json<ThreadUpdate>,
) -> Result<StatusCode, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    // Access first: an invisible merge request is a 404 whatever the body.
    state.services.mr(&s, &ns, &repo, number).await?;
    let Some(resolved) = b.resolved else {
        return Err(slath_service::ServiceError::invalid(
            "resolved",
            "required",
            "Set resolved to true or false.",
        )
        .into());
    };
    state
        .services
        .set_thread_resolved(&s, (&ns, &repo, number), id, resolved)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Marks a thread addressed, for whoever may resolve it: what bots do for
/// threads people started. It satisfies nothing; the thread stays open
/// until a person resolves it. 409 for a resolved thread.
#[utoipa::path(put, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/threads/{id}/addressed", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path), ("id" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem)))]
pub async fn mark_addressed(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number, id)): Path<(String, String, i32, String)>,
) -> Result<StatusCode, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state
        .services
        .set_thread_addressed(&s, (&ns, &repo, number), id, true)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Clears a thread's addressed mark.
#[utoipa::path(delete, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/threads/{id}/addressed", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path), ("id" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn clear_addressed(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number, id)): Path<(String, String, i32, String)>,
) -> Result<StatusCode, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state
        .services
        .set_thread_addressed(&s, (&ns, &repo, number), id, false)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, patch, post, put};
    Router::new()
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/threads",
            Access::Resource,
            get(list_threads).post(create_thread),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/threads/{id}",
            Access::Resource,
            patch(update_thread),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/threads/{id}/comments",
            Access::Resource,
            post(reply),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/threads/{id}/addressed",
            Access::Resource,
            put(mark_addressed).delete(clear_addressed),
        )
}
