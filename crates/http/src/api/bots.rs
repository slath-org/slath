// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/user/bots`, `/orgs/{org}/bots`, `/bots/{name}`, and a bot's tokens
//! and SSH keys.

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
    /// `active`, `stopped`, or `suspended`.
    pub state: String,
    pub created_at: String,
    /// When it was stopped, while it is.
    pub stopped_at: Option<String>,
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
            state: match (b.state, b.stopped_at) {
                (UserState::Active, _) => "active",
                (_, Some(_)) => "stopped",
                _ => "suspended",
            }
            .to_owned(),
            created_at: crate::ui::Time::from(b.created_at).iso,
            stopped_at: b.stopped_at.map(|t| crate::ui::Time::from(t).iso),
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

/// A bot's tokens, never the secrets (`user:read`).
#[utoipa::path(get, path = "/api/v1/bots/{name}/tokens", tag = "bots", security(("token" = ["user:read"])), params(("name" = String, Path), PageParams),
    responses((status = 200, body = super::user::TokenPage), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn list_bot_tokens(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path(name): Path<String>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let tokens: Vec<super::user::Token> = state
        .services
        .list_bot_tokens(&subject, &name)
        .await?
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .map(Into::into)
        .collect();
    Ok(page(&state, &uri, tokens, offset, limit))
}

/// Bot tokens can't be created through the API, only on the bot's settings
/// page in sudo mode or with `slath admin create-bot`.
#[utoipa::path(post, path = "/api/v1/bots/{name}/tokens", tag = "bots", security(("token" = [])), params(("name" = String, Path)),
    responses((status = 403, body = super::Problem), (status = 401, body = super::Problem)))]
pub async fn create_bot_token(ApiUser(_subject): ApiUser) -> AppError {
    AppError::forbidden().detail(
        "Bot tokens can only be created on the bot's settings page or with `slath admin create-bot`.",
    )
}

/// Revokes one of a bot's tokens (`user:write`).
#[utoipa::path(delete, path = "/api/v1/bots/{name}/tokens/{id}", tag = "bots", security(("token" = ["user:write"])), params(("name" = String, Path), ("id" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn revoke_bot_token(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path((name, id)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state.services.revoke_bot_token(&subject, &name, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A bot's SSH keys (`user:read`).
#[utoipa::path(get, path = "/api/v1/bots/{name}/keys", tag = "bots", security(("token" = ["user:read"])), params(("name" = String, Path), PageParams),
    responses((status = 200, body = super::user::SshKeyPage), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn list_bot_keys(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path(name): Path<String>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let keys: Vec<super::user::SshKey> = state
        .services
        .list_bot_keys(&subject, &name)
        .await?
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .map(Into::into)
        .collect();
    Ok(page(&state, &uri, keys, offset, limit))
}

/// Adds an SSH key to a bot (`user:write`).
#[utoipa::path(post, path = "/api/v1/bots/{name}/keys", tag = "bots", security(("token" = ["user:write"])), params(("name" = String, Path)), request_body = super::user::NewSshKey,
    responses((status = 201, body = super::user::SshKey), (status = 401, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn add_bot_key(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path(name): Path<String>,
    Json(k): Json<super::user::NewSshKey>,
) -> Result<Response, AppError> {
    let key = state
        .services
        .add_bot_key(
            &subject,
            false,
            &name,
            k.title.as_deref().unwrap_or(""),
            &k.key,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(super::user::SshKey::from(key))).into_response())
}

/// Removes one of a bot's SSH keys (`user:write`).
#[utoipa::path(delete, path = "/api/v1/bots/{name}/keys/{id}", tag = "bots", security(("token" = ["user:write"])), params(("name" = String, Path), ("id" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn remove_bot_key(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path((name, id)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state
        .services
        .remove_bot_key(&subject, false, &name, id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// An open merge request in a bot's overview.
#[derive(Serialize, ToSchema)]
pub struct OverviewMr {
    /// `namespace/name`.
    pub repo: String,
    pub number: i32,
    pub title: String,
}

/// A push in a bot's overview.
#[derive(Serialize, ToSchema)]
pub struct OverviewPush {
    pub repo: String,
    /// `http`, `ssh`, `internal`, or `reconcile`.
    pub transport: String,
    pub at: String,
}

/// The bot's token that expires next.
#[derive(Serialize, ToSchema)]
pub struct OverviewToken {
    pub name: String,
    pub expires_at: String,
}

/// API requests in the current hour, counted by this server process.
#[derive(Serialize, ToSchema)]
pub struct OverviewRateLimit {
    pub limit: u32,
    pub used: u32,
}

/// What a bot's managers see of it. Merge requests and pushes are only
/// those in repositories the caller can read.
#[derive(Serialize, ToSchema)]
pub struct BotOverview {
    pub bot: Bot,
    /// A freeze of its organization or of the instance applies to it.
    pub frozen: bool,
    pub open_merge_requests: u64,
    /// The newest open merge requests (at most 5).
    pub recent_merge_requests: Vec<OverviewMr>,
    /// The newest pushes (at most 5).
    pub recent_pushes: Vec<OverviewPush>,
    pub next_token_expiry: Option<OverviewToken>,
    pub rate_limit: OverviewRateLimit,
}

impl BotOverview {
    pub(crate) fn new(state: &AppState, o: slath_service::bot_control::BotOverview) -> Self {
        let limit = state.config.limits.bot_requests_per_hour;
        let used = state
            .api_limits
            .used(&super::support::RateLimiter::bot_key(o.bot.id), limit);
        Self {
            bot: o.bot.into(),
            frozen: o.frozen,
            open_merge_requests: u64::try_from(o.open_mr_count).unwrap_or(0),
            recent_merge_requests: o
                .open_mrs
                .into_iter()
                .map(|m| OverviewMr {
                    repo: m.repo,
                    number: m.number,
                    title: m.title,
                })
                .collect(),
            recent_pushes: o
                .pushes
                .into_iter()
                .map(|p| OverviewPush {
                    repo: p.repo,
                    transport: p.transport,
                    at: crate::ui::Time::from(p.at).iso,
                })
                .collect(),
            next_token_expiry: o.next_expiry.map(|(name, at)| OverviewToken {
                name,
                expires_at: crate::ui::Time::from(at).iso,
            }),
            rate_limit: OverviewRateLimit { limit, used },
        }
    }
}

/// A bot's open merge requests, recent pushes, next token expiry, and
/// rate-limit use (`user:read`; its managers and site administrators).
#[utoipa::path(get, path = "/api/v1/bots/{name}/overview", tag = "bots", security(("token" = ["user:read"])), params(("name" = String, Path)),
    responses((status = 200, body = BotOverview), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn bot_overview(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path(name): Path<String>,
) -> Result<Json<BotOverview>, AppError> {
    let o = state.services.bot_overview(&subject, &name).await?;
    Ok(Json(BotOverview::new(&state, o)))
}

/// What stopping a bot removed.
#[derive(Serialize, ToSchema)]
pub struct BotStopped {
    pub bot: Bot,
    /// False if it was stopped already.
    pub changed: bool,
    pub tokens_revoked: u64,
    pub keys_removed: u64,
}

/// Stops a bot at once: it's suspended, its tokens are revoked, its SSH
/// keys removed, and its pushes refused (`user:write`; its managers and
/// site administrators).
#[utoipa::path(post, path = "/api/v1/bots/{name}/stop", tag = "bots", security(("token" = ["user:write"])), params(("name" = String, Path)),
    responses((status = 200, body = BotStopped), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn stop_bot(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path(name): Path<String>,
) -> Result<Json<BotStopped>, AppError> {
    let done = state.services.stop_bot(&subject, &name).await?;
    let bot = state.services.controlled_bot(&subject, &name).await?;
    Ok(Json(BotStopped {
        bot: bot.into(),
        changed: done.changed,
        tokens_revoked: done.tokens_revoked,
        keys_removed: done.keys_removed,
    }))
}

/// Re-enables a stopped bot, without credentials: create new tokens and
/// keys for it (`user:write`). A bot suspended by an administrator or with
/// its owner stays suspended (409).
#[utoipa::path(post, path = "/api/v1/bots/{name}/start", tag = "bots", security(("token" = ["user:write"])), params(("name" = String, Path)),
    responses((status = 200, body = Bot), (status = 401, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem)))]
pub async fn start_bot(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path(name): Path<String>,
) -> Result<Json<Bot>, AppError> {
    state.services.start_bot(&subject, &name).await?;
    let bot = state.services.controlled_bot(&subject, &name).await?;
    Ok(Json(bot.into()))
}

pub fn router() -> Router<AppState> {
    use axum::routing::{delete, get, post};
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
        .route_as(
            "/api/v1/bots/{name}/tokens",
            Access::Resource,
            get(list_bot_tokens).post(create_bot_token),
        )
        .route_as(
            "/api/v1/bots/{name}/tokens/{id}",
            Access::Resource,
            delete(revoke_bot_token),
        )
        .route_as(
            "/api/v1/bots/{name}/keys",
            Access::Resource,
            get(list_bot_keys).post(add_bot_key),
        )
        .route_as(
            "/api/v1/bots/{name}/keys/{id}",
            Access::Resource,
            delete(remove_bot_key),
        )
        .route_as("/api/v1/bots/{name}/stop", Access::Resource, post(stop_bot))
        .route_as(
            "/api/v1/bots/{name}/overview",
            Access::Resource,
            get(bot_overview),
        )
        .route_as(
            "/api/v1/bots/{name}/start",
            Access::Resource,
            post(start_bot),
        )
}
