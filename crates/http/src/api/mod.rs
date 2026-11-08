// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The REST API under `/api/v1`. It authenticates with bearer tokens only
//! and never reads cookies, so it has no CSRF surface. The OpenAPI document
//! is generated from these handlers ([`openapi`]) and committed as
//! `docs/api/openapi.json`.

pub mod issues;
pub mod labels;
pub mod merge_requests;
pub mod orgs;
pub mod repos;
pub mod statuses;
pub mod support;
pub mod user;

use axum::extract::FromRequestParts;
use axum::http::header;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::Serialize;
use slath_core::authz::Subject;
use utoipa::{OpenApi, ToSchema};

use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// The subject of an API request: anonymous without `Authorization`, the
/// token's user with a valid bearer token. Any other `Authorization` value
/// (wrong scheme, unknown, revoked, or expired token) is rejected with 401.
#[derive(Debug, Clone)]
pub struct ApiSubject(pub Subject);

impl FromRequestParts<AppState> for ApiSubject {
    type Rejection = AppError;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(cached) = parts.extensions.get::<Self>() {
            return Ok(cached.clone());
        }
        let Some(value) = parts.headers.get(header::AUTHORIZATION) else {
            return Ok(Self(Subject::Anonymous));
        };
        let token = value
            .to_str()
            .ok()
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .ok_or_else(|| {
                AppError::unauthenticated().detail("Use \"Authorization: Bearer <token>\".")
            })?;
        let auth = state
            .services
            .authenticate_token(token)
            .await?
            .ok_or_else(|| {
                AppError::unauthenticated().detail("The token is invalid, expired, or revoked.")
            })?;
        let s = Self(auth.subject);
        parts.extensions.insert(s.clone());
        Ok(s)
    }
}

/// A signed-in API subject (401 for anonymous requests).
#[derive(Debug, Clone)]
pub struct ApiUser(pub Subject);

impl FromRequestParts<AppState> for ApiUser {
    type Rejection = AppError;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match ApiSubject::from_request_parts(parts, state).await?.0 {
            Subject::Anonymous => Err(AppError::unauthenticated()),
            s => Ok(Self(s)),
        }
    }
}

/// An RFC 9457 problem document.
#[derive(Serialize, ToSchema)]
pub struct Problem {
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub status: u16,
    /// Stable machine-readable code, e.g. `not_found`, `validation_failed`.
    pub code: String,
    pub detail: Option<String>,
    pub errors: Option<Vec<ProblemField>>,
    pub request_id: String,
}

#[derive(Serialize, ToSchema)]
pub struct ProblemField {
    pub field: String,
    pub code: String,
    pub message: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct Version {
    pub version: String,
    pub api: String,
}

/// Server version and API generation.
#[utoipa::path(get, path = "/api/v1/version", tag = "meta", responses((status = 200, body = Version)))]
pub async fn version() -> Json<Version> {
    Json(Version {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        api: "v1".to_owned(),
    })
}

async fn fallback() -> AppError {
    AppError::not_found()
}

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
        let c = openapi.components.get_or_insert_with(Default::default);
        c.add_security_scheme(
            "token",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some("A personal access token (slath_pat_…)"))
                    .build(),
            ),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(title = "Slath API", description = "The Slath REST API. Authenticate with `Authorization: Bearer <personal access token>`. Errors are RFC 9457 problem documents. Mutating requests accept an `Idempotency-Key` header."),
    paths(
        version,
        user::current_user,
        user::list_keys, user::add_key, user::remove_key,
        user::list_tokens, user::create_token, user::revoke_token,
        orgs::list_orgs, orgs::create_org, orgs::get_org, orgs::update_org,
        orgs::list_members, orgs::get_member, orgs::put_member, orgs::delete_member,
        repos::list_repos, repos::create_repo, repos::get_repo, repos::update_repo, repos::delete_repo,
        repos::list_branches, repos::list_tags, repos::list_commits, repos::get_commit, repos::get_tree, repos::get_blob,
        repos::list_collaborators, repos::put_collaborator, repos::delete_collaborator,
        repos::list_protections, repos::post_protection, repos::delete_protection,
        issues::list_issues, issues::create_issue, issues::get_issue, issues::update_issue,
        issues::list_comments, issues::create_comment, issues::update_comment, issues::delete_comment,
        merge_requests::list_mrs, merge_requests::create_mr, merge_requests::get_mr, merge_requests::update_mr,
        merge_requests::list_mr_comments, merge_requests::create_mr_comment,
        merge_requests::get_stack, merge_requests::bind_change,
        merge_requests::list_reviews, merge_requests::create_review, merge_requests::put_reviewers,
        merge_requests::get_mergeability, merge_requests::merge,
        statuses::create_status, statuses::list_statuses,
        labels::list_labels, labels::create_label, labels::update_label, labels::delete_label,
        labels::list_milestones, labels::create_milestone, labels::update_milestone, labels::delete_milestone,
        crate::attachments::api_upload, crate::references::api_references,
        crate::ui::notifications::api_list, crate::ui::notifications::api_patch,
        crate::ui::search::api_issues, crate::ui::search::api_repos,
        crate::ui::import::api_import, crate::ui::import::api_report,
    ),
    components(schemas(Problem, ProblemField)),
    modifiers(&SecurityAddon),
    tags((name = "meta"), (name = "user"), (name = "orgs"), (name = "repos"), (name = "issues"), (name = "merge requests"), (name = "statuses"))
)]
pub struct ApiDoc;

/// The OpenAPI document.
#[must_use]
pub fn openapi() -> utoipa::openapi::OpenApi {
    let mut doc = ApiDoc::openapi();
    doc.info.version = "v1".to_owned();
    doc
}

/// `GET /api/v1/openapi.json`.
async fn openapi_json() -> Response {
    Json(openapi()).into_response()
}

pub fn router() -> Router<AppState> {
    use axum::routing::{any, get};
    Router::new()
        .route_as("/api/v1/version", Access::Public, get(version))
        .route_as("/api/v1/openapi.json", Access::Public, get(openapi_json))
        .merge(user::router())
        .merge(orgs::router())
        .merge(repos::router())
        .merge(issues::router())
        .merge(merge_requests::router())
        .merge(statuses::router())
        .merge(labels::router())
        .merge(crate::attachments::api_router())
        .merge(crate::references::api_router())
        .merge(crate::ui::notifications::api_router())
        .merge(crate::ui::search::api_router())
        .merge(crate::ui::import::api_router())
        .route_as("/api/{*rest}", Access::Public, any(fallback))
}
