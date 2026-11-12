// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/user/bots`, `/orgs/{org}/bots`, `/bots/{name}`.

use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use slath_core::authz::{Scope, UserState};
use slath_db::bots::BotOwner;
use slath_service::bots::Owner;
use utoipa::ToSchema;

use super::ApiUser;
use super::support::{PageParams, page};
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// Who owns a bot.
#[derive(Serialize, ToSchema)]
pub struct BotOwnerRef {
    /// `user` or `org`.
    pub kind: String,
    pub name: String,
}

#[derive(Serialize, ToSchema)]
pub struct Bot {
    pub id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub owner: BotOwnerRef,
    /// `active` or `suspended`.
    pub state: String,
    pub created_at: String,
}

impl From<slath_db::bots::Bot> for Bot {
    fn from(b: slath_db::bots::Bot) -> Self {
        Self {
            id: b.id.to_string(),
            username: b.name,
            display_name: b.display_name,
            owner: BotOwnerRef {
                kind: match b.owner {
                    BotOwner::User(_) => "user",
                    BotOwner::Org(_) => "org",
                }
                .to_owned(),
                name: b.owner_name,
            },
            state: match b.state {
                UserState::Active => "active",
                _ => "suspended",
            }
            .to_owned(),
            created_at: crate::ui::Time::from(b.created_at).iso,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct BotPage {
    pub items: Vec<Bot>,
    pub next_cursor: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct NewBot {
    /// The bot's username; it shares the namespace of users and organizations.
    pub username: String,
    pub display_name: Option<String>,
}

async fn list(
    state: &AppState,
    subject: &slath_core::authz::Subject,
    owner: Owner<'_>,
    uri: &axum::http::Uri,
    p: &PageParams,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(state)?, p.limit(state.config.limits.api_page_max));
    let bots: Vec<Bot> = state
        .services
        .list_bots(subject, owner)
        .await?
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .map(Into::into)
        .collect();
    Ok(page(state, uri, bots, offset, limit))
}

async fn create(
    state: &AppState,
    subject: &slath_core::authz::Subject,
    owner: Owner<'_>,
    b: NewBot,
) -> Result<Response, AppError> {
    let bot = state
        .services
        .create_bot(
            subject,
            owner,
            &b.username,
            b.display_name.as_deref().unwrap_or(""),
        )
        .await?;
    let loc = format!("/api/v1/bots/{}", bot.name);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, loc)],
        Json(Bot::from(bot)),
    )
        .into_response())
}

/// The authenticated person's bots (`user:read`).
#[utoipa::path(get, path = "/api/v1/user/bots", tag = "bots", security(("token" = ["user:read"])), params(PageParams),
    responses((status = 200, body = BotPage), (status = 401, body = super::Problem), (status = 403, body = super::Problem)))]
pub async fn list_user_bots(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    list(&state, &subject, Owner::Me, &uri, &p).await
}

/// Creates a bot owned by the authenticated person (`user:write`).
#[utoipa::path(post, path = "/api/v1/user/bots", tag = "bots", security(("token" = ["user:write"])), request_body = NewBot,
    responses((status = 201, body = Bot), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_user_bot(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Json(b): Json<NewBot>,
) -> Result<Response, AppError> {
    create(&state, &subject, Owner::Me, b).await
}

/// An organization's bots (organization owners, `user:read`).
#[utoipa::path(get, path = "/api/v1/orgs/{org}/bots", tag = "bots", security(("token" = ["user:read"])), params(("org" = String, Path), PageParams),
    responses((status = 200, body = BotPage), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn list_org_bots(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path(org): Path<String>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    list(&state, &subject, Owner::Org(&org), &uri, &p).await
}

/// Creates a bot owned by an organization (organization owners, `user:write`).
#[utoipa::path(post, path = "/api/v1/orgs/{org}/bots", tag = "bots", security(("token" = ["user:write"])), params(("org" = String, Path)), request_body = NewBot,
    responses((status = 201, body = Bot), (status = 401, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_org_bot(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path(org): Path<String>,
    Json(b): Json<NewBot>,
) -> Result<Response, AppError> {
    create(&state, &subject, Owner::Org(&org), b).await
}

/// A bot you manage (`user:read`).
#[utoipa::path(get, path = "/api/v1/bots/{name}", tag = "bots", security(("token" = ["user:read"])), params(("name" = String, Path)),
    responses((status = 200, body = Bot), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn get_bot(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path(name): Path<String>,
) -> Result<Json<Bot>, AppError> {
    let bot = state
        .services
        .managed_bot(&subject, &name, Scope::UserRead)
        .await?;
    Ok(Json(bot.into()))
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as(
            "/api/v1/user/bots",
            Access::Account,
            get(list_user_bots).post(create_user_bot),
        )
        .route_as(
            "/api/v1/orgs/{org}/bots",
            Access::Resource,
            get(list_org_bots).post(create_org_bot),
        )
        .route_as("/api/v1/bots/{name}", Access::Resource, get(get_bot))
}
