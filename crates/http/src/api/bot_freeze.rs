// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Freezing every bot of an organization or of the instance: their
//! credentials are refused until unfrozen, and nothing is revoked.

use axum::extract::{Path, State};
use axum::{Json, Router};
use serde::Serialize;
use slath_core::authz::Subject;
use slath_service::bot_control::FreezeScope;
use utoipa::ToSchema;

use super::ApiUser;
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// Whether bots are frozen, since when, and by whom.
#[derive(Serialize, ToSchema)]
pub struct BotFreeze {
    pub frozen: bool,
    /// Who froze them (`null` for `slath admin` or a deleted account).
    pub frozen_by: Option<String>,
    pub frozen_at: Option<String>,
}

async fn show(state: &AppState, s: &Subject, scope: FreezeScope) -> Result<BotFreeze, AppError> {
    let f = state.services.bot_freeze(s, scope).await?;
    Ok(BotFreeze {
        frozen: f.is_some(),
        frozen_by: f.as_ref().and_then(|f| f.frozen_by.clone()),
        frozen_at: f.map(|f| crate::ui::Time::from(f.frozen_at).iso),
    })
}

async fn set(
    state: &AppState,
    s: &Subject,
    scope: FreezeScope,
    frozen: bool,
) -> Result<Json<BotFreeze>, AppError> {
    state.services.set_bot_freeze(s, scope, frozen).await?;
    Ok(Json(show(state, s, scope).await?))
}

async fn org_scope(state: &AppState, org: &str) -> Result<FreezeScope, AppError> {
    Ok(FreezeScope::Org(super::orgs::org_id(state, org).await?))
}

/// Whether the organization's bots are frozen (its owners; `user:write`).
#[utoipa::path(get, path = "/api/v1/orgs/{org}/bot-freeze", tag = "bots", security(("token" = ["user:write"])), params(("org" = String, Path)),
    responses((status = 200, body = BotFreeze), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn get_org_freeze(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path(org): Path<String>,
) -> Result<Json<BotFreeze>, AppError> {
    let scope = org_scope(&state, &org).await?;
    Ok(Json(show(&state, &s, scope).await?))
}

/// Freezes every bot the organization owns: their tokens and SSH keys are
/// refused and their pushes rejected until unfrozen; nothing is revoked.
#[utoipa::path(put, path = "/api/v1/orgs/{org}/bot-freeze", tag = "bots", security(("token" = ["user:write"])), params(("org" = String, Path)),
    responses((status = 200, body = BotFreeze), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn freeze_org(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path(org): Path<String>,
) -> Result<Json<BotFreeze>, AppError> {
    let scope = org_scope(&state, &org).await?;
    set(&state, &s, scope, true).await
}

/// Unfreezes the organization's bots; their credentials work again.
#[utoipa::path(delete, path = "/api/v1/orgs/{org}/bot-freeze", tag = "bots", security(("token" = ["user:write"])), params(("org" = String, Path)),
    responses((status = 200, body = BotFreeze), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn unfreeze_org(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path(org): Path<String>,
) -> Result<Json<BotFreeze>, AppError> {
    let scope = org_scope(&state, &org).await?;
    set(&state, &s, scope, false).await
}

/// Whether every bot of the instance is frozen (site administrators;
/// `admin`).
#[utoipa::path(get, path = "/api/v1/admin/bot-freeze", tag = "bots", security(("token" = ["admin"])),
    responses((status = 200, body = BotFreeze), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn get_instance_freeze(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
) -> Result<Json<BotFreeze>, AppError> {
    Ok(Json(show(&state, &s, FreezeScope::Instance).await?))
}

/// Freezes every bot of the instance.
#[utoipa::path(put, path = "/api/v1/admin/bot-freeze", tag = "bots", security(("token" = ["admin"])),
    responses((status = 200, body = BotFreeze), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn freeze_instance(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
) -> Result<Json<BotFreeze>, AppError> {
    set(&state, &s, FreezeScope::Instance, true).await
}

/// Unfreezes the instance's bots.
#[utoipa::path(delete, path = "/api/v1/admin/bot-freeze", tag = "bots", security(("token" = ["admin"])),
    responses((status = 200, body = BotFreeze), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn unfreeze_instance(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
) -> Result<Json<BotFreeze>, AppError> {
    set(&state, &s, FreezeScope::Instance, false).await
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as(
            "/api/v1/orgs/{org}/bot-freeze",
            Access::Resource,
            get(get_org_freeze).put(freeze_org).delete(unfreeze_org),
        )
        .route_as(
            "/api/v1/admin/bot-freeze",
            Access::Resource,
            get(get_instance_freeze)
                .put(freeze_instance)
                .delete(unfreeze_instance),
        )
}
