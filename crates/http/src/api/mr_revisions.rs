// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The revisions of a merge request and the interdiff between two of them.

use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::ApiSubject;
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// One revision: what the source branch held after a push that changed it.
#[derive(Serialize, ToSchema)]
pub struct RevisionBody {
    /// 1 for the first revision, counting up.
    pub seq: i32,
    pub head_oid: String,
    /// The target branch's head when the revision was made.
    pub base_oid: String,
    /// `None` when source and target share no history.
    pub merge_base_oid: Option<String>,
    pub commit_count: i32,
    /// `per-change` or `whole-only`.
    pub mode: String,
    /// Why the revision is `whole-only`.
    pub mode_reason: Option<String>,
    /// `None` when it wasn't a push by a known account.
    pub pushed_by: Option<String>,
    pub created_at: String,
}

impl From<slath_db::merge_requests::Revision> for RevisionBody {
    fn from(r: slath_db::merge_requests::Revision) -> Self {
        Self {
            seq: r.seq,
            head_oid: r.head_oid,
            base_oid: r.base_oid,
            merge_base_oid: r.merge_base_oid,
            commit_count: r.commit_count,
            mode: r.mode,
            mode_reason: r.mode_reason,
            pushed_by: r.pushed_by,
            created_at: crate::ui::Time::from(r.created_at).iso,
        }
    }
}

/// A merge request's revisions, oldest first.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/revisions", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = Vec<RevisionBody>), (status = 404, body = super::Problem)))]
pub async fn list_revisions(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<Vec<RevisionBody>>, AppError> {
    let (_, m) = state.services.mr(&s, &ns, &repo, number).await?;
    let list = state.services.mr_revisions(&m).await?;
    Ok(Json(list.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct InterdiffQuery {
    /// The earlier revision.
    pub from: i32,
    /// The later revision (default: the current one).
    pub to: Option<i32>,
}

/// Two revisions compared, as plain text (`git range-diff`). It pairs
/// commits by similarity, so it's a heuristic. 404 when a revision is
/// unknown or has no merge base.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/interdiff", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path), InterdiffQuery),
    responses((status = 200, content_type = "text/plain", body = String), (status = 404, body = super::Problem), (status = 413, body = super::Problem)))]
pub async fn get_interdiff(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Query(q): Query<InterdiffQuery>,
) -> Result<Response, AppError> {
    crate::plain::interdiff(&state, &s, (&ns, &repo, number), q.from, q.to).await
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/revisions",
            Access::Resource,
            get(list_revisions),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/interdiff",
            Access::Resource,
            get(get_interdiff),
        )
}
