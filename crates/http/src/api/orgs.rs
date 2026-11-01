// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/orgs`, `/orgs/{org}`, `/orgs/{org}/members`.

use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use slath_core::authz::Subject;
use slath_core::ids::OrgId;
use slath_db::orgs::{NamespaceKind, OrgRole};
use slath_service::ServiceError;
use utoipa::ToSchema;

use super::support::{PageParams, page};
use super::{ApiSubject, ApiUser};
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

#[derive(Serialize, ToSchema)]
pub struct Org {
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub created_at: String,
}

#[derive(Serialize, ToSchema)]
pub struct OrgPage {
    pub items: Vec<Org>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct Member {
    pub username: String,
    /// `owner` or `member`.
    pub role: String,
}

#[derive(Serialize, ToSchema)]
pub struct MemberPage {
    pub items: Vec<Member>,
    pub next_cursor: Option<String>,
}

async fn org_id(state: &AppState, name: &str) -> Result<OrgId, AppError> {
    match state.services.resolve_namespace(name).await? {
        Some((_, NamespaceKind::Org(id))) => Ok(id),
        _ => Err(AppError::not_found()),
    }
}

async fn org_json(state: &AppState, subject: &Subject, id: OrgId) -> Result<Org, AppError> {
    let v = state.services.org_view(subject, id).await?;
    Ok(Org {
        name: v.org.name,
        display_name: v.org.display_name,
        description: v.org.description,
        created_at: crate::ui::Time::from(v.org.created_at).iso,
    })
}

/// Organizations the authenticated user belongs to.
#[utoipa::path(get, path = "/api/v1/orgs", tag = "orgs", security(("token" = ["user:read"])), params(PageParams),
    responses((status = 200, body = OrgPage), (status = 401, body = super::Problem)))]
pub async fn list_orgs(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    // Memberships are account data: tokens need `user:read`.
    let id = slath_service::tokens::user_with_scope(&subject, slath_core::authz::Scope::UserRead)?;
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let names = slath_db::orgs::for_user(&state.pool, id)
        .await
        .map_err(|e| AppError::internal(&e))?;
    let mut out = Vec::new();
    for (name, _) in names.into_iter().skip(offset).take(limit + 1) {
        out.push(org_json(&state, &subject, org_id(&state, &name).await?).await?);
    }
    Ok(page(&state, &uri, out, offset, limit))
}

#[derive(Deserialize, ToSchema)]
pub struct NewOrg {
    pub name: String,
    pub display_name: Option<String>,
}

/// Creates an organization; the caller becomes its owner (`user:write`).
#[utoipa::path(post, path = "/api/v1/orgs", tag = "orgs", security(("token" = ["user:write"])), request_body = NewOrg,
    responses((status = 201, body = Org), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_org(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Json(o): Json<NewOrg>,
) -> Result<Response, AppError> {
    let id = state
        .services
        .create_org(&subject, &o.name, o.display_name.as_deref().unwrap_or(""))
        .await?;
    let org = org_json(&state, &subject, id).await?;
    let loc = format!("/api/v1/orgs/{}", org.name);
    Ok((StatusCode::CREATED, [(header::LOCATION, loc)], Json(org)).into_response())
}

/// An organization's profile (public).
#[utoipa::path(get, path = "/api/v1/orgs/{org}", tag = "orgs", params(("org" = String, Path)),
    responses((status = 200, body = Org), (status = 404, body = super::Problem)))]
pub async fn get_org(
    State(state): State<AppState>,
    ApiSubject(subject): ApiSubject,
    Path(org): Path<String>,
) -> Result<Json<Org>, AppError> {
    let id = org_id(&state, &org).await?;
    Ok(Json(org_json(&state, &subject, id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateOrg {
    pub display_name: Option<String>,
    pub description: Option<String>,
}

/// Changes an organization's profile (owners, `user:write`).
#[utoipa::path(patch, path = "/api/v1/orgs/{org}", tag = "orgs", security(("token" = ["user:write"])), params(("org" = String, Path)), request_body = UpdateOrg,
    responses((status = 200, body = Org), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn update_org(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path(org): Path<String>,
    Json(u): Json<UpdateOrg>,
) -> Result<Json<Org>, AppError> {
    let id = org_id(&state, &org).await?;
    let current = state.services.org_view(&subject, id).await?.org;
    let display = u.display_name.or(current.display_name).unwrap_or_default();
    let desc = u.description.or(current.description).unwrap_or_default();
    state
        .services
        .update_org(&subject, id, &display, &desc)
        .await?;
    Ok(Json(org_json(&state, &subject, id).await?))
}

async fn members_of(
    state: &AppState,
    subject: &Subject,
    id: OrgId,
) -> Result<Vec<Member>, AppError> {
    match state.services.org_members(subject, id).await {
        Ok(m) => Ok(m
            .into_iter()
            .map(|m| Member {
                username: m.name,
                role: m.role.as_str().to_owned(),
            })
            .collect()),
        // Member lists are for members: everyone else gets "not found".
        Err(ServiceError::Forbidden | ServiceError::Unauthenticated) => Err(AppError::not_found()),
        Err(e) => Err(e.into()),
    }
}

/// An organization's members (members only).
#[utoipa::path(get, path = "/api/v1/orgs/{org}/members", tag = "orgs", security(("token" = ["user:read"])), params(("org" = String, Path), PageParams),
    responses((status = 200, body = MemberPage), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn list_members(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path(org): Path<String>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let id = org_id(&state, &org).await?;
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let m: Vec<Member> = members_of(&state, &subject, id)
        .await?
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .collect();
    Ok(page(&state, &uri, m, offset, limit))
}

/// One member's role (members only).
#[utoipa::path(get, path = "/api/v1/orgs/{org}/members/{user}", tag = "orgs", security(("token" = ["user:read"])), params(("org" = String, Path), ("user" = String, Path)),
    responses((status = 200, body = Member), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn get_member(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path((org, user)): Path<(String, String)>,
) -> Result<Json<Member>, AppError> {
    let id = org_id(&state, &org).await?;
    members_of(&state, &subject, id)
        .await?
        .into_iter()
        .find(|m| m.username.eq_ignore_ascii_case(&user))
        .map(Json)
        .ok_or_else(AppError::not_found)
}

#[derive(Deserialize, ToSchema)]
pub struct PutMember {
    /// `owner` or `member`.
    pub role: String,
}

/// Adds a member or changes their role (owners, `user:write`).
#[utoipa::path(put, path = "/api/v1/orgs/{org}/members/{user}", tag = "orgs", security(("token" = ["user:write"])), params(("org" = String, Path), ("user" = String, Path)), request_body = PutMember,
    responses((status = 200, body = Member), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn put_member(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path((org, user)): Path<(String, String)>,
    Json(m): Json<PutMember>,
) -> Result<Json<Member>, AppError> {
    let id = org_id(&state, &org).await?;
    let role = OrgRole::parse(&m.role).ok_or_else(|| {
        AppError::validation(vec![crate::error::FieldError {
            field: "role".to_owned(),
            code: "invalid".to_owned(),
            message: "Use owner or member.".to_owned(),
        }])
    })?;
    state
        .services
        .set_org_member(&subject, id, &user, role)
        .await?;
    Ok(Json(Member {
        username: user,
        role: role.as_str().to_owned(),
    }))
}

/// Removes a member, or leaves (`user:write`). The last owner can't leave.
#[utoipa::path(delete, path = "/api/v1/orgs/{org}/members/{user}", tag = "orgs", security(("token" = ["user:write"])), params(("org" = String, Path), ("user" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem)))]
pub async fn delete_member(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path((org, user)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let id = org_id(&state, &org).await?;
    state
        .services
        .remove_org_member(&subject, id, &user)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as(
            "/api/v1/orgs",
            Access::PublicRead,
            get(list_orgs).post(create_org),
        )
        .route_as(
            "/api/v1/orgs/{org}",
            Access::PublicRead,
            get(get_org).patch(update_org),
        )
        .route_as(
            "/api/v1/orgs/{org}/members",
            Access::Resource,
            get(list_members),
        )
        .route_as(
            "/api/v1/orgs/{org}/members/{user}",
            Access::Resource,
            get(get_member).put(put_member).delete(delete_member),
        )
}
