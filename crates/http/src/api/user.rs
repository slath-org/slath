// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/user`, `/user/keys`, `/user/tokens`.

use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ApiUser;
use super::support::{PageParams, page};
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

fn ts(t: time::OffsetDateTime) -> String {
    crate::ui::Time::from(t).iso
}

#[derive(Serialize, ToSchema)]
pub struct User {
    pub id: String,
    pub username: String,
    pub display_name: Option<String>,
    /// `human` or `bot`.
    pub kind: String,
    pub is_admin: bool,
    pub created_at: String,
}

/// The authenticated user (`user:read`).
#[utoipa::path(get, path = "/api/v1/user", tag = "user", security(("token" = ["user:read"])),
    responses((status = 200, body = User), (status = 401, body = super::Problem), (status = 403, body = super::Problem)))]
pub async fn current_user(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
) -> Result<Json<User>, AppError> {
    let u = state.services.current_user(&subject).await?;
    Ok(Json(User {
        id: u.id.to_string(),
        username: u.name,
        display_name: u.display_name,
        kind: match u.kind {
            slath_core::authz::UserKind::Human => "human",
            slath_core::authz::UserKind::Bot => "bot",
        }
        .to_owned(),
        is_admin: u.is_admin,
        created_at: ts(u.created_at),
    }))
}

#[derive(Serialize, ToSchema)]
pub struct SshKey {
    pub id: String,
    pub title: String,
    pub key_type: String,
    /// `SHA256:` fingerprint as `ssh-keygen -l` prints it.
    pub fingerprint: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

impl From<slath_service::keys::KeyInfo> for SshKey {
    fn from(k: slath_service::keys::KeyInfo) -> Self {
        Self {
            id: k.id.to_string(),
            title: k.title,
            key_type: k.key_type,
            fingerprint: k.fingerprint,
            created_at: ts(k.created_at),
            last_used_at: k.last_used_at.map(ts),
        }
    }
}

/// A page of SSH keys.
#[derive(Serialize, ToSchema)]
pub struct SshKeyPage {
    pub items: Vec<SshKey>,
    pub next_cursor: Option<String>,
}

/// The user's SSH keys (`user:read`).
#[utoipa::path(get, path = "/api/v1/user/keys", tag = "user", security(("token" = ["user:read"])), params(PageParams),
    responses((status = 200, body = SshKeyPage), (status = 401, body = super::Problem)))]
pub async fn list_keys(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let keys: Vec<SshKey> = state
        .services
        .list_keys(&subject)
        .await?
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .map(Into::into)
        .collect();
    Ok(page(&state, &uri, keys, offset, limit))
}

#[derive(Deserialize, ToSchema)]
pub struct NewSshKey {
    /// Defaults to the key's comment.
    pub title: Option<String>,
    /// One `authorized_keys` line: `<type> <base64> [comment]`.
    pub key: String,
}

/// Adds an SSH key (`user:write`).
#[utoipa::path(post, path = "/api/v1/user/keys", tag = "user", security(("token" = ["user:write"])), request_body = NewSshKey,
    responses((status = 201, body = SshKey), (status = 401, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn add_key(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Json(k): Json<NewSshKey>,
) -> Result<Response, AppError> {
    let key = state
        .services
        .add_key(&subject, false, k.title.as_deref().unwrap_or(""), &k.key)
        .await?;
    Ok((StatusCode::CREATED, Json(SshKey::from(key))).into_response())
}

/// Removes one of the user's SSH keys (`user:write`).
#[utoipa::path(delete, path = "/api/v1/user/keys/{id}", tag = "user", security(("token" = ["user:write"])), params(("id" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn remove_key(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state.services.remove_key(&subject, false, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct Token {
    pub id: String,
    pub name: String,
    /// The first characters after the prefix (`slath_pat_` or `slath_bot_`),
    /// for recognizing the token.
    pub prefix: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub created_at: String,
    pub revoked: bool,
    pub expired: bool,
}

impl From<slath_service::tokens::TokenInfo> for Token {
    fn from(t: slath_service::tokens::TokenInfo) -> Self {
        Self {
            id: t.id.to_string(),
            name: t.name,
            prefix: t.prefix,
            scopes: t.scopes.iter().map(|s| s.as_str().to_owned()).collect(),
            expires_at: t.expires_at.map(ts),
            last_used_at: t.last_used_at.map(ts),
            created_at: ts(t.created_at),
            revoked: t.revoked,
            expired: t.expired,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct TokenPage {
    pub items: Vec<Token>,
    pub next_cursor: Option<String>,
}

/// The user's personal access tokens (never the secrets; `user:read`).
#[utoipa::path(get, path = "/api/v1/user/tokens", tag = "user", security(("token" = ["user:read"])), params(PageParams),
    responses((status = 200, body = TokenPage), (status = 401, body = super::Problem)))]
pub async fn list_tokens(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let tokens: Vec<Token> = state
        .services
        .list_tokens(&subject)
        .await?
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .map(Into::into)
        .collect();
    Ok(page(&state, &uri, tokens, offset, limit))
}

/// Tokens can't be created through the API, only in the web UI in sudo mode.
#[utoipa::path(post, path = "/api/v1/user/tokens", tag = "user", security(("token" = [])),
    responses((status = 403, body = super::Problem), (status = 401, body = super::Problem)))]
pub async fn create_token(ApiUser(_subject): ApiUser) -> AppError {
    AppError::forbidden()
        .detail("Tokens can only be created in the web UI (Settings > Access tokens).")
}

/// Revokes one of the user's tokens (`user:write`).
#[utoipa::path(delete, path = "/api/v1/user/tokens/{id}", tag = "user", security(("token" = ["user:write"])), params(("id" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn revoke_token(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state.services.revoke_token(&subject, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    use axum::routing::{delete, get};
    Router::new()
        .route_as("/api/v1/user", Access::Account, get(current_user))
        .route_as(
            "/api/v1/user/keys",
            Access::Account,
            get(list_keys).post(add_key),
        )
        .route_as(
            "/api/v1/user/keys/{id}",
            Access::Account,
            delete(remove_key),
        )
        .route_as(
            "/api/v1/user/tokens",
            Access::Account,
            get(list_tokens).post(create_token),
        )
        .route_as(
            "/api/v1/user/tokens/{id}",
            Access::Account,
            delete(revoke_token),
        )
}
