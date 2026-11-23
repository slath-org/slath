// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Linked merge requests: `…/merge-requests/{n}/group`, an ordered group
//! of merge requests on this instance that merge together.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiSubject, ApiUser};
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// One member, as far as you can read it.
#[derive(Serialize, ToSchema)]
pub struct GroupMemberBody {
    /// `ns/repo!n`.
    pub merge_request: String,
    pub namespace: String,
    pub repo: String,
    pub number: i32,
    pub title: String,
    /// The title is user-generated text.
    pub untrusted_content: bool,
    pub origin: super::TextOrigin,
    /// `open`, `merged`, or `closed`.
    pub state: String,
    pub head_oid: Option<String>,
    /// The readiness card's tone: `done`, `waiting`, or `blocking`.
    pub readiness_tone: String,
    pub readiness: String,
    /// Mergeable now with all its own requirements.
    pub mergeable: bool,
    /// You may merge it.
    pub can_merge: bool,
}

/// Why the last group merge stopped.
#[derive(Serialize, ToSchema)]
pub struct GroupFailureBody {
    /// The member it stopped at, if you can read it.
    pub merge_request: Option<String>,
    pub reason: String,
    pub at: Option<String>,
}

/// A linked group: the members you can read, in merge order.
#[derive(Serialize, ToSchema)]
pub struct GroupBody {
    /// `open`, `partially_landed` (some members merged), or `merged`.
    pub state: String,
    pub members: Vec<GroupMemberBody>,
    pub last_failure: Option<GroupFailureBody>,
    /// You may change the group (edit every member).
    pub can_edit: bool,
    /// You may merge every open member.
    pub can_merge: bool,
    /// Every open member is mergeable now.
    pub ready: bool,
    /// A group merge runs now.
    pub merging: bool,
}

impl From<slath_service::mr_groups::GroupView> for GroupBody {
    fn from(g: slath_service::mr_groups::GroupView) -> Self {
        Self {
            state: g.state.to_owned(),
            members: g
                .members
                .into_iter()
                .map(|m| GroupMemberBody {
                    merge_request: m.reference(),
                    namespace: m.namespace,
                    repo: m.repo,
                    number: m.number,
                    title: m.title,
                    untrusted_content: true,
                    origin: m.origin.into(),
                    state: m.state,
                    head_oid: m.head_oid,
                    readiness_tone: m.tone,
                    readiness: m.headline,
                    mergeable: m.mergeable,
                    can_merge: m.can_merge,
                })
                .collect(),
            last_failure: g.last_failure.map(|f| GroupFailureBody {
                merge_request: f.member,
                reason: f.reason,
                at: f.at.map(|t| crate::ui::Time::from(t).iso),
            }),
            can_edit: g.can_edit,
            can_merge: g.can_merge,
            ready: g.ready,
            merging: g.merging,
        }
    }
}

/// The group of a merge request; 404 when it isn't linked.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/group", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = GroupBody), (status = 404, body = super::Problem)))]
pub async fn get_group(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<GroupBody>, AppError> {
    let g = state
        .services
        .mr_group(&s, (&ns, &repo, number))
        .await?
        .ok_or_else(AppError::not_found)?;
    Ok(Json(g.into()))
}

/// Every field is optional in the JSON so that authorization runs first.
#[derive(Deserialize, ToSchema)]
pub struct PutGroupBody {
    /// Every member in merge order, including this merge request:
    /// `ns/repo!n` or merge request URLs. Merged members stay, first.
    #[serde(default)]
    pub members: Vec<String>,
}

/// Links merge requests into a group, or changes its members and order
/// (edit right on every merge request named and on every current member).
#[utoipa::path(put, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/group", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = PutGroupBody,
    responses((status = 200, body = GroupBody), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn put_group(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<PutGroupBody>,
) -> Result<Json<GroupBody>, AppError> {
    let g = state
        .services
        .set_mr_group(&s, (&ns, &repo, number), &b.members)
        .await?;
    Ok(Json(g.into()))
}

/// Unlinks this merge request from its group; a group left with fewer
/// than two members dissolves.
#[utoipa::path(delete, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/group", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem)))]
pub async fn delete_group(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<StatusCode, AppError> {
    state
        .services
        .unlink_mr_group(&s, (&ns, &repo, number))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new().route_as(
        "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/group",
        Access::Resource,
        axum::routing::get(get_group)
            .put(put_group)
            .delete(delete_group),
    )
}
