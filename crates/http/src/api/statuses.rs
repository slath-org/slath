// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Commit statuses: reports on a commit, with their reporter.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use slath_service::statuses::StatusInput;
use utoipa::ToSchema;

use super::{ApiSubject, ApiUser};
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// A reported status.
#[derive(Serialize, ToSchema)]
pub struct CommitStatus {
    pub id: String,
    pub commit_oid: String,
    pub context: String,
    /// `pending`, `success`, `failure`, or `error`.
    pub state: String,
    pub description: Option<String>,
    pub target_url: Option<String>,
    /// Who reported it (a user or bot); `null` once that account is gone.
    pub reporter: Option<String>,
    /// Always `reported`: Slath knows who reported it, not that the check
    /// ran.
    pub evidence: &'static str,
    pub created_at: String,
}

impl From<slath_db::statuses::Status> for CommitStatus {
    fn from(s: slath_db::statuses::Status) -> Self {
        Self {
            id: s.id.to_string(),
            commit_oid: s.commit_oid,
            context: s.context,
            state: s.state,
            description: s.description,
            target_url: s.target_url,
            reporter: s.reporter,
            evidence: "reported",
            created_at: crate::ui::Time::from(s.created_at).iso,
        }
    }
}

/// Every field is optional in the JSON so that authorization runs before
/// validation.
#[derive(Deserialize, ToSchema)]
pub struct NewStatus {
    /// `pending`, `success`, `failure`, or `error`.
    #[serde(default)]
    pub state: String,
    /// Up to 100 letters, digits, spaces, and `. _ / : -`.
    #[serde(default)]
    pub context: String,
    pub description: Option<String>,
    /// An `https://` URL (`http://` only for localhost).
    pub target_url: Option<String>,
}

/// Reports a status on a commit (scope `statuses:write`). The latest
/// status per context wins.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/statuses/{oid}", tag = "statuses", security(("token" = ["statuses:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("oid" = String, Path)), request_body = NewStatus,
    responses((status = 201, body = CommitStatus), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_status(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, oid)): Path<(String, String, String)>,
    Json(b): Json<NewStatus>,
) -> Result<(StatusCode, Json<CommitStatus>), AppError> {
    let st = state
        .services
        .report_status(
            &s,
            (&ns, &repo),
            &oid,
            &StatusInput {
                state: b.state,
                context: b.context,
                description: b.description,
                target_url: b.target_url,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(st.into())))
}

/// The latest status of each context on a commit.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/commits/{oid}/statuses", tag = "statuses", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("oid" = String, Path)),
    responses((status = 200, body = Vec<CommitStatus>), (status = 404, body = super::Problem)))]
pub async fn list_statuses(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, oid)): Path<(String, String, String)>,
) -> Result<Json<Vec<CommitStatus>>, AppError> {
    let list = state
        .services
        .commit_statuses(&s, (&ns, &repo), &oid)
        .await?;
    Ok(Json(list.into_iter().map(Into::into).collect()))
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as(
            "/api/v1/repos/{ns}/{repo}/statuses/{oid}",
            Access::Resource,
            post(create_status),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/commits/{oid}/statuses",
            Access::Resource,
            get(list_statuses),
        )
}
