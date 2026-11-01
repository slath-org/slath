// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The REST API under `/api/v1`. It authenticates with bearer tokens only
//! and never reads cookies, so it has no CSRF surface.

use axum::extract::{FromRequestParts, State};
use axum::http::header;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::Serialize;
use slath_core::authz::Subject;

use crate::AppState;
use crate::error::AppError;

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

#[derive(Serialize)]
struct Version {
    version: &'static str,
    api: &'static str,
}

async fn version() -> Json<Version> {
    Json(Version {
        version: env!("CARGO_PKG_VERSION"),
        api: "v1",
    })
}

#[derive(Serialize)]
struct UserJson {
    id: String,
    username: String,
    display_name: Option<String>,
    kind: &'static str,
    is_admin: bool,
    created_at: String,
}

async fn current_user(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
) -> Result<Response, AppError> {
    let user = state.services.current_user(&subject).await?;
    Ok(Json(UserJson {
        id: user.id.to_string(),
        username: user.name,
        display_name: user.display_name,
        kind: match user.kind {
            slath_core::authz::UserKind::Human => "human",
            slath_core::authz::UserKind::Bot => "bot",
        },
        is_admin: user.is_admin,
        created_at: crate::ui::Time::from(user.created_at).iso,
    })
    .into_response())
}

async fn fallback() -> AppError {
    AppError::not_found()
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route("/api/v1/version", get(version))
        .route("/api/v1/user", get(current_user))
        .route("/api/{*rest}", axum::routing::any(fallback))
}
