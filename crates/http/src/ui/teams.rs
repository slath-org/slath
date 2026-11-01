// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Organization teams (`/{org}/-/teams`) and member policies.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::ids::OrgId;
use slath_db::orgs::NamespaceKind;
use slath_service::{FieldError, ServiceError};

use super::settings::error_for;
use super::{PageCtx, render};
use crate::AppState;
use crate::auth::{MaybeSession, RequireSession};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

async fn org_of(state: &AppState, name: &str) -> Result<(String, OrgId), AppError> {
    match state.services.resolve_namespace(name).await? {
        Some((n, NamespaceKind::Org(id))) => Ok((n, id)),
        _ => Err(AppError::not_found()),
    }
}

/// Forbidden for teams means "not a member": the same as missing.
fn hide(e: ServiceError) -> AppError {
    match e {
        ServiceError::Forbidden | ServiceError::Unauthenticated => AppError::not_found(),
        other => other.into(),
    }
}

#[derive(Deserialize, Default)]
struct StatusQuery {
    status: Option<String>,
}

fn message(s: Option<&str>) -> Option<&'static str> {
    Some(match s? {
        "created" => "Team created.",
        "deleted" => "Team deleted.",
        "added" => "Member added.",
        "removed" => "Member removed.",
        _ => return None,
    })
}

#[derive(Template)]
#[template(path = "org_teams.html")]
struct TeamsPage {
    ctx: PageCtx,
    org: String,
    teams: Vec<slath_db::teams::Team>,
    can_manage: bool,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    name: String,
}

impl TeamsPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn teams_page(
    state: &AppState,
    ctx: PageCtx,
    session: &MaybeSession,
    org: &str,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    name: String,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let (org, id) = org_of(state, org).await?;
    let teams = state.services.teams(&subject, id).await.map_err(hide)?;
    let can_manage = state.services.org_view(&subject, id).await?.can_manage;
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&TeamsPage {
        ctx,
        org,
        teams,
        can_manage,
        message,
        errors,
        name,
    });
    *res.status_mut() = status;
    Ok(res)
}

async fn teams(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path(org): Path<String>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    teams_page(
        &state,
        ctx,
        &session,
        &org,
        message(q.status.as_deref()),
        Vec::new(),
        String::new(),
    )
    .await
}

#[derive(Deserialize)]
struct TeamForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
}

async fn create(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path(org): Path<String>,
    Form(f): Form<TeamForm>,
) -> Result<Response, AppError> {
    let (name, id) = org_of(&state, &org).await?;
    match state
        .services
        .create_team(&session.subject(), id, &f.name, &f.description)
        .await
    {
        Ok(()) => Ok(Redirect::to(&format!("/{name}/-/teams?status=created")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            teams_page(&state, ctx, &session, &org, None, errors, f.name).await
        }
        Err(e) => Err(hide(e)),
    }
}

#[derive(Template)]
#[template(path = "org_team.html")]
struct TeamPage {
    ctx: PageCtx,
    org: String,
    team: String,
    description: Option<String>,
    members: Vec<String>,
    repos: Vec<(String, String)>,
    can_manage: bool,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
}

impl TeamPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn team_page(
    state: &AppState,
    ctx: PageCtx,
    session: &MaybeSession,
    org: &str,
    team: &str,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let (org, id) = org_of(state, org).await?;
    let d = state
        .services
        .team(&subject, id, team)
        .await
        .map_err(hide)?;
    let can_manage = state.services.org_view(&subject, id).await?.can_manage;
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&TeamPage {
        ctx,
        org,
        team: d.team.name,
        description: d.team.description,
        members: d.members,
        repos: d
            .repos
            .into_iter()
            .map(|(r, role)| (r, slath_db::repos::role_str(role).to_owned()))
            .collect(),
        can_manage,
        message,
        errors,
    });
    *res.status_mut() = status;
    Ok(res)
}

async fn team(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((org, team)): Path<(String, String)>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    team_page(
        &state,
        ctx,
        &session,
        &org,
        &team,
        message(q.status.as_deref()),
        Vec::new(),
    )
    .await
}

#[derive(Deserialize)]
struct MemberForm {
    #[serde(default)]
    username: String,
}

async fn add_member(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((org, team)): Path<(String, String)>,
    Form(f): Form<MemberForm>,
) -> Result<Response, AppError> {
    let (name, id) = org_of(&state, &org).await?;
    match state
        .services
        .add_team_member(&session.subject(), id, &team, &f.username)
        .await
    {
        Ok(()) => Ok(Redirect::to(&format!("/{name}/-/teams/{team}?status=added")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            team_page(&state, ctx, &session, &org, &team, None, errors).await
        }
        Err(e) => Err(hide(e)),
    }
}

async fn remove_member(
    State(state): State<AppState>,
    session: MaybeSession,
    Path((org, team, user)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let (name, id) = org_of(&state, &org).await?;
    state
        .services
        .remove_team_member(&session.subject(), id, &team, &user)
        .await
        .map_err(hide)?;
    Ok(Redirect::to(&format!("/{name}/-/teams/{team}?status=removed")).into_response())
}

async fn delete(
    State(state): State<AppState>,
    session: MaybeSession,
    Path((org, team)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let (name, id) = org_of(&state, &org).await?;
    state
        .services
        .delete_team(&session.subject(), id, &team)
        .await
        .map_err(hide)?;
    Ok(Redirect::to(&format!("/{name}/-/teams?status=deleted")).into_response())
}

#[derive(Deserialize)]
struct PoliciesForm {
    #[serde(default)]
    default_member_role: String,
    require_mfa: Option<String>,
}

async fn policies(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path(org): Path<String>,
    Form(f): Form<PoliciesForm>,
) -> Result<Response, AppError> {
    let (name, id) = org_of(&state, &org).await?;
    state
        .services
        .set_org_policies(
            &s.subject,
            id,
            &f.default_member_role,
            f.require_mfa.is_some(),
        )
        .await
        .map_err(hide)?;
    Ok(Redirect::to(&format!("/{name}?status=saved")).into_response())
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as("/{org}/-/teams", Access::Resource, get(teams).post(create))
        .route_as("/{org}/-/teams/{team}", Access::Resource, get(team))
        .route_as(
            "/{org}/-/teams/{team}/members",
            Access::Resource,
            post(add_member),
        )
        .route_as(
            "/{org}/-/teams/{team}/members/{user}/remove",
            Access::Resource,
            post(remove_member),
        )
        .route_as(
            "/{org}/-/teams/{team}/delete",
            Access::Resource,
            post(delete),
        )
        .route_as(
            "/{org}/-/settings/policies",
            Access::Resource,
            post(policies),
        )
}
