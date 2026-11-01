// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Profiles (`/{ns}`), organization creation, members, and org settings.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_db::orgs::{NamespaceKind, OrgRole};
use slath_service::{FieldError, ServiceError};

use super::settings::error_for;
use super::{PageCtx, Time, render};
use crate::AppState;
use crate::auth::{MaybeSession, RequireSession};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

#[derive(Deserialize, Default)]
pub struct StatusQuery {
    status: Option<String>,
}

fn status_message(s: Option<&str>) -> Option<&'static str> {
    Some(match s? {
        "created" => "Organization created.",
        "saved" => "Saved.",
        "member-saved" => "Member saved.",
        "member-removed" => "Member removed.",
        _ => return None,
    })
}

#[derive(Template)]
#[template(path = "profile_user.html")]
struct UserProfile {
    ctx: PageCtx,
    name: String,
    display_name: Option<String>,
    joined: Time,
    orgs: Vec<String>,
    repos: Vec<super::repos::RepoCard>,
}

#[derive(Template)]
#[template(path = "profile_org.html")]
struct OrgProfile<'a> {
    ctx: PageCtx,
    name: String,
    display_name: Option<String>,
    description: Option<String>,
    is_member: bool,
    can_manage: bool,
    message: Option<&'a str>,
    repos: Vec<super::repos::RepoCard>,
}

/// `GET /{ns}`: a user's or an organization's profile.
pub async fn profile(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path(ns): Path<String>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let Some((name, kind)) = state.services.resolve_namespace(&ns).await? else {
        return Err(AppError::not_found());
    };
    let repos = super::repos::cards_for_namespace(&state, &subject, &name).await?;
    match kind {
        NamespaceKind::User(id) => {
            let user = slath_db::users::get(&state.pool, id)
                .await
                .map_err(|e| AppError::internal(&e))?
                .ok_or_else(AppError::not_found)?;
            let orgs = slath_db::orgs::for_user(&state.pool, id)
                .await
                .map_err(|e| AppError::internal(&e))?
                .into_iter()
                .map(|(n, _)| n)
                .collect();
            Ok(render(&UserProfile {
                ctx,
                name,
                display_name: user.display_name,
                joined: user.created_at.into(),
                orgs,
                repos,
            }))
        }
        NamespaceKind::Org(id) => {
            let view = state.services.org_view(&subject, id).await?;
            Ok(render(&OrgProfile {
                ctx,
                name,
                display_name: view.org.display_name,
                description: view.org.description,
                is_member: view.viewer_role.is_some(),
                can_manage: view.can_manage,
                message: status_message(q.status.as_deref()),
                repos,
            }))
        }
    }
}

#[derive(Template)]
#[template(path = "org_new.html")]
struct NewOrgPage {
    ctx: PageCtx,
    errors: Vec<FieldError>,
    name: String,
    display_name: String,
}

impl NewOrgPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn new_org(ctx: PageCtx, _s: RequireSession) -> Response {
    render(&NewOrgPage {
        ctx,
        errors: Vec::new(),
        name: String::new(),
        display_name: String::new(),
    })
}

#[derive(Deserialize)]
struct NewOrgForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    display_name: String,
}

async fn create_org(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Form(f): Form<NewOrgForm>,
) -> Result<Response, AppError> {
    match state
        .services
        .create_org(&session.subject, &f.name, &f.display_name)
        .await
    {
        Ok(_) => Ok(Redirect::to(&format!("/{}?status=created", f.name.trim())).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            let mut res = render(&NewOrgPage {
                ctx,
                errors,
                name: f.name,
                display_name: f.display_name,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        Err(e) => Err(e.into()),
    }
}

/// Resolves `/{org}/-/…` to an organization (404 for users and unknown names).
async fn org_of(
    state: &AppState,
    name: &str,
) -> Result<(String, slath_core::ids::OrgId), AppError> {
    match state.services.resolve_namespace(name).await? {
        Some((n, NamespaceKind::Org(id))) => Ok((n, id)),
        _ => Err(AppError::not_found()),
    }
}

struct MemberView {
    name: String,
    display_name: Option<String>,
    role: &'static str,
}

#[derive(Template)]
#[template(path = "org_members.html")]
struct MembersPage<'a> {
    ctx: PageCtx,
    org: String,
    members: Vec<MemberView>,
    can_manage: bool,
    viewer: Option<String>,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
    username: String,
}

impl MembersPage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn members_page(
    state: &AppState,
    ctx: PageCtx,
    session: &MaybeSession,
    org_name: &str,
    message: Option<&str>,
    errors: Vec<FieldError>,
    username: String,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let (name, id) = org_of(state, org_name).await?;
    let view = state.services.org_view(&subject, id).await?;
    let members = match state.services.org_members(&subject, id).await {
        Ok(m) => m,
        Err(ServiceError::Forbidden | ServiceError::Unauthenticated) => {
            return Err(AppError::not_found());
        }
        Err(e) => return Err(e.into()),
    };
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&MembersPage {
        ctx,
        org: name,
        members: members
            .into_iter()
            .map(|m| MemberView {
                name: m.name,
                display_name: m.display_name,
                role: m.role.as_str(),
            })
            .collect(),
        can_manage: view.can_manage,
        viewer: session.0.as_ref().map(|s| s.row.user_name.clone()),
        message,
        errors,
        username,
    });
    *res.status_mut() = status;
    Ok(res)
}

async fn members(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path(org): Path<String>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    members_page(
        &state,
        ctx,
        &session,
        &org,
        status_message(q.status.as_deref()),
        Vec::new(),
        String::new(),
    )
    .await
}

#[derive(Deserialize)]
struct MemberForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    role: String,
}

async fn add_member(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    RequireSession(s): RequireSession,
    Path(org): Path<String>,
    Form(f): Form<MemberForm>,
) -> Result<Response, AppError> {
    let (name, id) = org_of(&state, &org).await?;
    let role = OrgRole::parse(&f.role).unwrap_or(OrgRole::Member);
    match state
        .services
        .set_org_member(&s.subject, id, &f.username, role)
        .await
    {
        Ok(()) => {
            Ok(Redirect::to(&format!("/{name}/-/members?status=member-saved")).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            members_page(&state, ctx, &session, &org, None, errors, f.username).await
        }
        Err(ServiceError::Conflict(msg)) => {
            members_page(
                &state,
                ctx,
                &session,
                &org,
                None,
                vec![FieldError::new("username", "last_owner", msg)],
                f.username,
            )
            .await
        }
        Err(e) => Err(e.into()),
    }
}

async fn set_role(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    RequireSession(s): RequireSession,
    Path((org, user)): Path<(String, String)>,
    Form(f): Form<MemberForm>,
) -> Result<Response, AppError> {
    let (name, id) = org_of(&state, &org).await?;
    let role = OrgRole::parse(&f.role).ok_or_else(|| AppError::bad_request("unknown role"))?;
    match state
        .services
        .set_org_member(&s.subject, id, &user, role)
        .await
    {
        Ok(()) => {
            Ok(Redirect::to(&format!("/{name}/-/members?status=member-saved")).into_response())
        }
        Err(ServiceError::Conflict(msg)) => {
            members_page(
                &state,
                ctx,
                &session,
                &org,
                None,
                vec![FieldError::new("members", "last_owner", msg)],
                String::new(),
            )
            .await
        }
        Err(e) => Err(e.into()),
    }
}

async fn remove_member(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    RequireSession(s): RequireSession,
    Path((org, user)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let (name, id) = org_of(&state, &org).await?;
    let leaving = s.row.user_name.eq_ignore_ascii_case(&user);
    match state
        .services
        .remove_org_member(&s.subject, id, &user)
        .await
    {
        Ok(()) if leaving => Ok(Redirect::to(&format!("/{name}")).into_response()),
        Ok(()) => {
            Ok(Redirect::to(&format!("/{name}/-/members?status=member-removed")).into_response())
        }
        Err(ServiceError::Conflict(msg)) => {
            members_page(
                &state,
                ctx,
                &session,
                &org,
                None,
                vec![FieldError::new("members", "last_owner", msg)],
                String::new(),
            )
            .await
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Template)]
#[template(path = "org_settings.html")]
struct OrgSettingsPage {
    ctx: PageCtx,
    org: String,
    display_name: String,
    description: String,
    errors: Vec<FieldError>,
}

impl OrgSettingsPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn org_settings(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path(org): Path<String>,
) -> Result<Response, AppError> {
    let (name, id) = org_of(&state, &org).await?;
    let view = state.services.org_view(&s.subject, id).await?;
    if !view.can_manage {
        return Err(AppError::not_found());
    }
    Ok(render(&OrgSettingsPage {
        ctx,
        org: name,
        display_name: view.org.display_name.unwrap_or_default(),
        description: view.org.description.unwrap_or_default(),
        errors: Vec::new(),
    }))
}

#[derive(Deserialize)]
struct OrgSettingsForm {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    description: String,
}

async fn org_settings_save(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path(org): Path<String>,
    Form(f): Form<OrgSettingsForm>,
) -> Result<Response, AppError> {
    let (name, id) = org_of(&state, &org).await?;
    match state
        .services
        .update_org(&s.subject, id, &f.display_name, &f.description)
        .await
    {
        Ok(()) => Ok(Redirect::to(&format!("/{name}?status=saved")).into_response()),
        Err(ServiceError::Forbidden) => Err(AppError::not_found()),
        Err(ServiceError::Invalid(errors)) => {
            let mut res = render(&OrgSettingsPage {
                ctx,
                org: name,
                display_name: f.display_name,
                description: f.description,
                errors,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        Err(e) => Err(e.into()),
    }
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as("/-/orgs/new", Access::Account, get(new_org))
        .route_as("/-/orgs", Access::Account, post(create_org))
        .route_as("/{ns}", Access::Public, get(profile))
        .route_as(
            "/{org}/-/members",
            Access::Resource,
            get(members).post(add_member),
        )
        .route_as(
            "/{org}/-/members/{user}/role",
            Access::Resource,
            post(set_role),
        )
        .route_as(
            "/{org}/-/members/{user}/remove",
            Access::Resource,
            post(remove_member),
        )
        .route_as(
            "/{org}/-/settings",
            Access::Resource,
            get(org_settings).post(org_settings_save),
        )
}
