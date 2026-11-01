// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Importing a repository from another server: the `/-/import` page and
//! the API.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use slath_core::Secret;
use slath_core::authz::{Action, Visibility};
use slath_service::imports::ImportInput;
use slath_service::{FieldError, ServiceError};
use utoipa::ToSchema;

use super::settings::error_for;
use super::{PageCtx, Time, render};
use crate::AppState;
use crate::api::{ApiSubject, ApiUser};
use crate::auth::RequireSession;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

#[derive(Template)]
#[template(path = "repo/import.html")]
struct ImportPage {
    ctx: PageCtx,
    namespaces: Vec<String>,
    selected: String,
    name: String,
    description: String,
    url: String,
    username: String,
    visibility: &'static str,
    allow_http: bool,
    errors: Vec<FieldError>,
}

impl ImportPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn namespaces(
    state: &AppState,
    session: &slath_service::sessions::Session,
) -> Result<Vec<String>, AppError> {
    let mut out = vec![session.row.user_name.clone()];
    for (org, _) in slath_db::orgs::for_user(&state.pool, session.user_id())
        .await
        .map_err(|e| AppError::internal(&e))?
    {
        out.push(org);
    }
    Ok(out)
}

async fn page(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
) -> Result<Response, AppError> {
    if !state.config.import.enabled {
        return Err(AppError::not_found());
    }
    Ok(render(&ImportPage {
        ctx,
        namespaces: namespaces(&state, &s).await?,
        selected: s.row.user_name.clone(),
        name: String::new(),
        description: String::new(),
        url: String::new(),
        username: String::new(),
        visibility: "private",
        allow_http: state.config.import.allow_http,
        errors: Vec::new(),
    }))
}

#[derive(Deserialize)]
struct ImportForm {
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    visibility: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

async fn submit(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Form(f): Form<ImportForm>,
) -> Result<Response, AppError> {
    let visibility = super::repos::parse_visibility(&f.visibility)
        .unwrap_or(slath_core::authz::Visibility::Private);
    let input = ImportInput {
        namespace: f.namespace.clone(),
        name: f.name.clone(),
        description: f.description.clone(),
        visibility,
        url: f.url.clone(),
        username: f.username.clone(),
        password: Secret::new(f.password),
    };
    match state.services.import_repo(&s.subject, input).await {
        Ok(a) => {
            Ok(Redirect::to(&format!("/{}/{}", a.repo.namespace, a.repo.name)).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            let mut res = render(&ImportPage {
                ctx,
                namespaces: namespaces(&state, &s).await?,
                selected: f.namespace,
                name: f.name,
                description: f.description,
                url: f.url,
                username: f.username,
                visibility: super::repos::visibility_name(visibility),
                allow_http: state.config.import.allow_http,
                errors,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        Err(e) => Err(e.into()),
    }
}

/// A repository import request.
#[derive(Deserialize, ToSchema)]
pub struct NewImport {
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub name: String,
    pub description: Option<String>,
    /// `private` (default), `internal`, or `public`.
    pub visibility: Option<String>,
    /// An `https://` URL of a Git repository.
    #[serde(default)]
    pub url: String,
    /// Used once for the fetch and not stored.
    pub username: Option<String>,
    /// Used once for the fetch and not stored.
    pub password: Option<String>,
}

/// The state and result of an import.
#[derive(Serialize, ToSchema)]
pub struct ImportReport {
    /// `owner/name` of the new repository.
    pub repository: String,
    /// The source, without credentials.
    pub url: String,
    /// `queued`, `running`, `done`, or `failed`.
    pub state: String,
    pub error: Option<String>,
    pub branches: Option<i32>,
    pub tags: Option<i32>,
    pub size_bytes: Option<i64>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

fn report(full: String, i: slath_db::imports::Import) -> ImportReport {
    ImportReport {
        repository: full,
        url: i.url,
        state: i.state,
        error: i.error,
        branches: i.branches,
        tags: i.tags,
        size_bytes: i.size_bytes,
        created_at: Time::from(i.created_at).iso,
        finished_at: i.finished_at.map(|t| Time::from(t).iso),
    }
}

/// Creates a repository and imports branches and tags into it in the
/// background (`repo:write`). Poll the import's report for the result.
#[utoipa::path(post, path = "/api/v1/repos/import", tag = "repos", security(("token" = ["repo:write"])), request_body = NewImport,
    responses((status = 202, body = ImportReport), (status = 401, body = crate::api::Problem), (status = 403, body = crate::api::Problem), (status = 404, body = crate::api::Problem), (status = 422, body = crate::api::Problem)))]
pub async fn api_import(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Json(n): Json<NewImport>,
) -> Result<Response, AppError> {
    let visibility = match n.visibility.as_deref().map(super::repos::parse_visibility) {
        None => Visibility::Private,
        Some(Some(v)) => v,
        Some(None) => {
            return Err(ServiceError::invalid(
                "visibility",
                "invalid",
                "Use private, internal, or public.",
            )
            .into());
        }
    };
    let a = state
        .services
        .import_repo(
            &s,
            ImportInput {
                namespace: n.namespace,
                name: n.name,
                description: n.description.unwrap_or_default(),
                visibility,
                url: n.url,
                username: n.username.unwrap_or_default(),
                password: Secret::new(n.password.unwrap_or_default()),
            },
        )
        .await?;
    let full = format!("{}/{}", a.repo.namespace, a.repo.name);
    let i = state
        .services
        .import_report(&s, &a)
        .await?
        .ok_or_else(AppError::not_found)?;
    let loc = format!("/api/v1/repos/{full}/import");
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, loc)],
        Json(report(full, i)),
    )
        .into_response())
}

/// The import report of a repository (administrators of it; 404 if it
/// wasn't imported).
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/import", tag = "repos", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path)),
    responses((status = 200, body = ImportReport), (status = 401, body = crate::api::Problem), (status = 404, body = crate::api::Problem)))]
pub async fn api_report(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo)): Path<(String, String)>,
) -> Result<Json<ImportReport>, AppError> {
    let a = state
        .services
        .repo_access(&s, &ns, &repo, Action::RepoSettings)
        .await?;
    let i = state
        .services
        .import_report(&s, &a)
        .await?
        .ok_or_else(AppError::not_found)?;
    Ok(Json(report(
        format!("{}/{}", a.repo.namespace, a.repo.name),
        i,
    )))
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new().route_as("/-/import", Access::Account, get(page).post(submit))
}

pub fn api_router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as("/api/v1/repos/import", Access::Account, post(api_import))
        .route_as(
            "/api/v1/repos/{ns}/{repo}/import",
            Access::Resource,
            get(api_report),
        )
}
