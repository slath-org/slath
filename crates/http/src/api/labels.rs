// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/repos/{ns}/{repo}/labels` and `/milestones`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiSubject, ApiUser};
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

#[derive(Serialize, ToSchema)]
pub struct Label {
    pub name: String,
    /// Six hex digits, without `#`.
    pub color: String,
    pub description: Option<String>,
    pub open_issues: i64,
}

fn label_json(l: slath_db::labels::Label) -> Label {
    Label {
        name: l.name,
        color: l.color,
        description: l.description,
        open_issues: l.open_issues,
    }
}

/// A repository's labels.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/labels", tag = "issues", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path)),
    responses((status = 200, body = Vec<Label>), (status = 404, body = super::Problem)))]
pub async fn list_labels(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo)): Path<(String, String)>,
) -> Result<Json<Vec<Label>>, AppError> {
    let (_, l) = state.services.labels(&s, &ns, &repo).await?;
    Ok(Json(l.into_iter().map(label_json).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct LabelBody {
    pub name: Option<String>,
    /// For example `1f6feb` or `#1f6feb`.
    pub color: Option<String>,
    pub description: Option<String>,
}

/// Creates a label (maintainers).
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/labels", tag = "issues", security(("token" = ["issues:write"])), params(("ns" = String, Path), ("repo" = String, Path)), request_body = LabelBody,
    responses((status = 201, body = Label), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_label(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    Json(b): Json<LabelBody>,
) -> Result<(StatusCode, Json<Label>), AppError> {
    let name = b.name.unwrap_or_default();
    state
        .services
        .save_label(
            &s,
            &ns,
            &repo,
            None,
            &name,
            b.color.as_deref().unwrap_or("ededed"),
            b.description.as_deref().unwrap_or(""),
        )
        .await?;
    let l = find_label(&state, &s, &ns, &repo, name.trim()).await?;
    Ok((StatusCode::CREATED, Json(label_json(l))))
}

async fn find_label(
    state: &AppState,
    s: &slath_core::authz::Subject,
    ns: &str,
    repo: &str,
    name: &str,
) -> Result<slath_db::labels::Label, AppError> {
    let (_, all) = state.services.labels(s, ns, repo).await?;
    all.into_iter()
        .find(|l| l.name.eq_ignore_ascii_case(name))
        .ok_or_else(AppError::not_found)
}

/// Changes a label (maintainers).
#[utoipa::path(patch, path = "/api/v1/repos/{ns}/{repo}/labels/{name}", tag = "issues", security(("token" = ["issues:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("name" = String, Path)), request_body = LabelBody,
    responses((status = 200, body = Label), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn update_label(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, name)): Path<(String, String, String)>,
    Json(b): Json<LabelBody>,
) -> Result<Json<Label>, AppError> {
    let cur = find_label(&state, &s, &ns, &repo, &name).await?;
    let new_name = b.name.unwrap_or(cur.name);
    state
        .services
        .save_label(
            &s,
            &ns,
            &repo,
            Some(cur.id),
            &new_name,
            &b.color.unwrap_or(cur.color),
            &b.description.or(cur.description).unwrap_or_default(),
        )
        .await?;
    let l = find_label(&state, &s, &ns, &repo, new_name.trim()).await?;
    Ok(Json(label_json(l)))
}

/// Deletes a label and removes it from all issues (maintainers).
#[utoipa::path(delete, path = "/api/v1/repos/{ns}/{repo}/labels/{name}", tag = "issues", security(("token" = ["issues:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("name" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn delete_label(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, name)): Path<(String, String, String)>,
) -> Result<StatusCode, AppError> {
    let cur = find_label(&state, &s, &ns, &repo, &name).await?;
    state.services.delete_label(&s, &ns, &repo, cur.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct Milestone {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    /// `YYYY-MM-DD`.
    pub due_on: Option<String>,
    /// `open` or `closed`.
    pub state: String,
    pub open_issues: i64,
    pub closed_issues: i64,
}

fn date(d: time::Date) -> String {
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    d.format(&fmt).unwrap_or_default()
}

fn milestone_json(m: slath_db::labels::Milestone) -> Milestone {
    Milestone {
        id: m.id.to_string(),
        title: m.title,
        description: m.description,
        due_on: m.due_on.map(date),
        state: m.state,
        open_issues: m.open_issues,
        closed_issues: m.closed_issues,
    }
}

/// A repository's milestones, open ones first.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/milestones", tag = "issues", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path)),
    responses((status = 200, body = Vec<Milestone>), (status = 404, body = super::Problem)))]
pub async fn list_milestones(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo)): Path<(String, String)>,
) -> Result<Json<Vec<Milestone>>, AppError> {
    let (_, m) = state.services.milestones(&s, &ns, &repo).await?;
    Ok(Json(m.into_iter().map(milestone_json).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct MilestoneBody {
    pub title: Option<String>,
    pub description: Option<String>,
    /// `YYYY-MM-DD`; an empty string clears it.
    pub due_on: Option<String>,
    /// `open` or `closed`.
    pub state: Option<String>,
}

async fn find_milestone(
    state: &AppState,
    s: &slath_core::authz::Subject,
    ns: &str,
    repo: &str,
    pick: impl Fn(&slath_db::labels::Milestone) -> bool,
) -> Result<slath_db::labels::Milestone, AppError> {
    let (_, all) = state.services.milestones(s, ns, repo).await?;
    all.into_iter().find(pick).ok_or_else(AppError::not_found)
}

/// Creates a milestone (maintainers).
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/milestones", tag = "issues", security(("token" = ["issues:write"])), params(("ns" = String, Path), ("repo" = String, Path)), request_body = MilestoneBody,
    responses((status = 201, body = Milestone), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_milestone(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    Json(b): Json<MilestoneBody>,
) -> Result<(StatusCode, Json<Milestone>), AppError> {
    let title = b.title.unwrap_or_default();
    state
        .services
        .save_milestone(
            &s,
            &ns,
            &repo,
            None,
            &title,
            b.description.as_deref().unwrap_or(""),
            b.due_on.as_deref().unwrap_or(""),
            true,
        )
        .await?;
    let t = title.trim().to_owned();
    let m = find_milestone(&state, &s, &ns, &repo, |m| m.title.eq_ignore_ascii_case(&t)).await?;
    Ok((StatusCode::CREATED, Json(milestone_json(m))))
}

fn milestone_id(s: &str) -> Result<uuid::Uuid, AppError> {
    s.parse().map_err(|_| AppError::not_found())
}

/// Changes, closes, or reopens a milestone (maintainers).
#[utoipa::path(patch, path = "/api/v1/repos/{ns}/{repo}/milestones/{id}", tag = "issues", security(("token" = ["issues:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("id" = String, Path)), request_body = MilestoneBody,
    responses((status = 200, body = Milestone), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn update_milestone(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, id)): Path<(String, String, String)>,
    Json(b): Json<MilestoneBody>,
) -> Result<Json<Milestone>, AppError> {
    let id = milestone_id(&id)?;
    let cur = find_milestone(&state, &s, &ns, &repo, |m| m.id == id).await?;
    let open = match b.state.as_deref() {
        None => cur.state == "open",
        Some("open") => true,
        Some("closed") => false,
        Some(_) => {
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
        .save_milestone(
            &s,
            &ns,
            &repo,
            Some(id),
            &b.title.unwrap_or(cur.title),
            &b.description.or(cur.description).unwrap_or_default(),
            &b.due_on
                .unwrap_or_else(|| cur.due_on.map(date).unwrap_or_default()),
            open,
        )
        .await?;
    let m = find_milestone(&state, &s, &ns, &repo, |m| m.id == id).await?;
    Ok(Json(milestone_json(m)))
}

/// Deletes a milestone; its issues keep existing without one (maintainers).
#[utoipa::path(delete, path = "/api/v1/repos/{ns}/{repo}/milestones/{id}", tag = "issues", security(("token" = ["issues:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("id" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn delete_milestone(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, id)): Path<(String, String, String)>,
) -> Result<StatusCode, AppError> {
    state
        .services
        .delete_milestone(&s, &ns, &repo, milestone_id(&id)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as(
            "/api/v1/repos/{ns}/{repo}/labels",
            Access::Resource,
            get(list_labels).post(create_label),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/labels/{name}",
            Access::Resource,
            axum::routing::patch(update_label).delete(delete_label),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/milestones",
            Access::Resource,
            get(list_milestones).post(create_milestone),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/milestones/{id}",
            Access::Resource,
            axum::routing::patch(update_milestone).delete(delete_milestone),
        )
}
