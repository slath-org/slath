// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Webhooks of repositories (administrators) and organizations (owners):
//! managing them, their delivery log, redelivery, and ping.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use slath_core::authz::{Action, Subject};
use slath_core::ids::OrgId;
use slath_db::orgs::NamespaceKind;
use slath_service::webhooks::{WebhookInput, WebhookOwner};
use utoipa::ToSchema;

use super::ApiUser;
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};
use crate::ui::Time;

/// A webhook. Its secret is never returned, except once when Slath
/// generated it.
#[derive(Serialize, ToSchema)]
pub struct Webhook {
    pub id: String,
    pub url: String,
    /// The events it receives.
    pub events: Vec<String>,
    pub active: bool,
    /// Why Slath switched it off, if it did.
    pub disabled_reason: Option<String>,
    /// Failed attempts in a row (50 switch it off).
    pub consecutive_failures: i32,
    /// The generated secret, only in the answer that created it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<slath_db::webhooks::Webhook> for Webhook {
    fn from(w: slath_db::webhooks::Webhook) -> Self {
        Self {
            id: w.id.to_string(),
            url: w.url,
            events: w.events,
            active: w.active,
            disabled_reason: w.disabled_reason,
            consecutive_failures: w.consecutive_failures,
            secret: None,
            created_at: Time::from(w.created_at).iso,
            updated_at: Time::from(w.updated_at).iso,
        }
    }
}

/// A new webhook, or changes to one (missing fields stay as they are).
#[derive(Deserialize, ToSchema, Default)]
pub struct WebhookBody {
    #[serde(default)]
    pub url: Option<String>,
    /// One or more of `push`, `merge_request`, `merge_request_comment`,
    /// `merge_request_review`, `fork`, `issue`, `issue_comment`, `status`,
    /// `repo`.
    #[serde(default)]
    pub events: Option<Vec<String>>,
    /// Default for new webhooks: true.
    #[serde(default)]
    pub active: Option<bool>,
    /// 16 to 256 characters. Omitted on create: Slath generates one and
    /// returns it once. Omitted on update: unchanged.
    #[serde(default)]
    pub secret: Option<String>,
}

/// A delivery: one event for one webhook. Its ID (`X-Slath-Delivery`)
/// stays the same across retries.
#[derive(Serialize, ToSchema)]
pub struct Delivery {
    pub id: String,
    pub event: String,
    pub action: Option<String>,
    /// `pending`, `delivered`, or `failed`.
    pub state: String,
    pub attempts: i32,
    pub next_attempt_at: Option<String>,
    /// The delivery this one sends again.
    pub redelivery_of: Option<String>,
    /// The last attempt's HTTP status.
    pub last_status: Option<i32>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

impl From<slath_db::webhooks::Delivery> for Delivery {
    fn from(d: slath_db::webhooks::Delivery) -> Self {
        Self {
            id: d.id.to_string(),
            event: d.event,
            action: d.action,
            state: d.state,
            attempts: d.attempts,
            next_attempt_at: d.next_attempt_at.map(|t| Time::from(t).iso),
            redelivery_of: d.redelivery_of.map(|i| i.to_string()),
            last_status: d.last_status,
            created_at: Time::from(d.created_at).iso,
            finished_at: d.finished_at.map(|t| Time::from(t).iso),
        }
    }
}

/// One attempt of a delivery.
#[derive(Serialize, ToSchema)]
pub struct Attempt {
    pub attempt: i32,
    pub started_at: String,
    pub duration_ms: i32,
    /// The receiver's HTTP status, if it answered.
    pub status: Option<i32>,
    /// The response body, cut at 4 KiB.
    pub response: Option<String>,
    /// What went wrong, in words.
    pub error: Option<String>,
}

/// A delivery with its payload and attempts.
#[derive(Serialize, ToSchema)]
pub struct DeliveryDetail {
    #[serde(flatten)]
    pub delivery: Delivery,
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
    pub attempts: Vec<Attempt>,
}

#[derive(Serialize, ToSchema)]
pub struct Queued {
    /// The new delivery's ID.
    pub delivery_id: String,
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct DeliveryParams {
    /// At most this many, newest first (default 50, at most 100).
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Whose webhooks a request is about.
enum Owner {
    Repo(Box<slath_service::repos::RepoAccess>),
    Org(OrgId),
}

impl Owner {
    fn get(&self) -> WebhookOwner<'_> {
        match self {
            Self::Repo(a) => WebhookOwner::Repo(a),
            Self::Org(o) => WebhookOwner::Org(*o),
        }
    }
}

async fn repo_owner(
    state: &AppState,
    s: &Subject,
    ns: &str,
    repo: &str,
) -> Result<Owner, AppError> {
    Ok(Owner::Repo(Box::new(
        state
            .services
            .repo_access(s, ns, repo, Action::WebhooksManage)
            .await?,
    )))
}

async fn org_owner(state: &AppState, org: &str) -> Result<Owner, AppError> {
    match state.services.resolve_namespace(org).await? {
        Some((_, NamespaceKind::Org(id))) => Ok(Owner::Org(id)),
        _ => Err(AppError::not_found()),
    }
}

fn id(s: &str) -> Result<uuid::Uuid, AppError> {
    s.parse().map_err(|_| AppError::not_found())
}

fn input(b: WebhookBody, current: Option<&slath_db::webhooks::Webhook>) -> WebhookInput {
    WebhookInput {
        url: b
            .url
            .or_else(|| current.map(|w| w.url.clone()))
            .unwrap_or_default(),
        events: b
            .events
            .or_else(|| current.map(|w| w.events.clone()))
            .unwrap_or_default(),
        active: b.active.or(current.map(|w| w.active)).unwrap_or(true),
        secret: b.secret,
    }
}

async fn list(state: &AppState, s: &Subject, o: &Owner) -> Result<Json<Vec<Webhook>>, AppError> {
    let hooks = state.services.webhooks(s, o.get()).await?;
    Ok(Json(hooks.into_iter().map(Into::into).collect()))
}

async fn create(
    state: &AppState,
    s: &Subject,
    o: &Owner,
    b: WebhookBody,
) -> Result<Response, AppError> {
    let (w, secret) = state
        .services
        .create_webhook(s, o.get(), &input(b, None))
        .await?;
    let mut body = Webhook::from(w);
    body.secret = secret;
    Ok((StatusCode::CREATED, Json(body)).into_response())
}

async fn get(state: &AppState, s: &Subject, o: &Owner, w: &str) -> Result<Json<Webhook>, AppError> {
    Ok(Json(
        state.services.webhook(s, o.get(), id(w)?).await?.into(),
    ))
}

async fn update(
    state: &AppState,
    s: &Subject,
    o: &Owner,
    w: &str,
    b: WebhookBody,
) -> Result<Json<Webhook>, AppError> {
    let w = id(w)?;
    let current = state.services.webhook(s, o.get(), w).await?;
    let updated = state
        .services
        .update_webhook(s, o.get(), w, &input(b, Some(&current)))
        .await?;
    Ok(Json(updated.into()))
}

async fn delete(state: &AppState, s: &Subject, o: &Owner, w: &str) -> Result<StatusCode, AppError> {
    state.services.delete_webhook(s, o.get(), id(w)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn ping(state: &AppState, s: &Subject, o: &Owner, w: &str) -> Result<Response, AppError> {
    let d = state.services.ping_webhook(s, o.get(), id(w)?).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Queued {
            delivery_id: d.to_string(),
        }),
    )
        .into_response())
}

async fn deliveries(
    state: &AppState,
    s: &Subject,
    o: &Owner,
    w: &str,
    limit: Option<i64>,
) -> Result<Json<Vec<Delivery>>, AppError> {
    let ds = state
        .services
        .webhook_deliveries(s, o.get(), id(w)?, limit.unwrap_or(50))
        .await?;
    Ok(Json(ds.into_iter().map(Into::into).collect()))
}

async fn delivery(
    state: &AppState,
    s: &Subject,
    o: &Owner,
    w: &str,
    d: &str,
) -> Result<Json<DeliveryDetail>, AppError> {
    let (d, attempts) = state
        .services
        .webhook_delivery(s, o.get(), (id(w)?, id(d)?))
        .await?;
    let payload = d.payload.clone();
    Ok(Json(DeliveryDetail {
        delivery: d.into(),
        payload,
        attempts: attempts
            .into_iter()
            .map(|a| Attempt {
                attempt: a.attempt,
                started_at: Time::from(a.started_at).iso,
                duration_ms: a.duration_ms,
                status: a.status,
                response: a.response,
                error: a.error,
            })
            .collect(),
    }))
}

async fn redeliver(
    state: &AppState,
    s: &Subject,
    o: &Owner,
    w: &str,
    d: &str,
) -> Result<Response, AppError> {
    let new = state
        .services
        .redeliver_webhook(s, o.get(), (id(w)?, id(d)?))
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Queued {
            delivery_id: new.to_string(),
        }),
    )
        .into_response())
}

// Repository webhooks.

/// The repository's webhooks (administrators).
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/webhooks", tag = "webhooks", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path)),
    responses((status = 200, body = Vec<Webhook>), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn list_repo_webhooks(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
) -> Result<Json<Vec<Webhook>>, AppError> {
    let o = repo_owner(&state, &s, &ns, &repo).await?;
    list(&state, &s, &o).await
}

/// Creates a webhook (administrators). Without a secret, Slath generates
/// one and returns it in this answer only.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/webhooks", tag = "webhooks", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path)), request_body = WebhookBody,
    responses((status = 201, body = Webhook), (status = 401, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_repo_webhook(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    Json(b): Json<WebhookBody>,
) -> Result<Response, AppError> {
    let o = repo_owner(&state, &s, &ns, &repo).await?;
    create(&state, &s, &o, b).await
}

/// One webhook.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/webhooks/{id}", tag = "webhooks", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("id" = String, Path)),
    responses((status = 200, body = Webhook), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn get_repo_webhook(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, w)): Path<(String, String, String)>,
) -> Result<Json<Webhook>, AppError> {
    let o = repo_owner(&state, &s, &ns, &repo).await?;
    get(&state, &s, &o, &w).await
}

/// Changes a webhook; turning it on again clears its failure count. The
/// one who changes it becomes its principal.
#[utoipa::path(patch, path = "/api/v1/repos/{ns}/{repo}/webhooks/{id}", tag = "webhooks", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("id" = String, Path)), request_body = WebhookBody,
    responses((status = 200, body = Webhook), (status = 401, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn update_repo_webhook(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, w)): Path<(String, String, String)>,
    Json(b): Json<WebhookBody>,
) -> Result<Json<Webhook>, AppError> {
    let o = repo_owner(&state, &s, &ns, &repo).await?;
    update(&state, &s, &o, &w, b).await
}

/// Deletes a webhook and its delivery log.
#[utoipa::path(delete, path = "/api/v1/repos/{ns}/{repo}/webhooks/{id}", tag = "webhooks", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("id" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn delete_repo_webhook(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, w)): Path<(String, String, String)>,
) -> Result<StatusCode, AppError> {
    let o = repo_owner(&state, &s, &ns, &repo).await?;
    delete(&state, &s, &o, &w).await
}

/// Sends a `ping` event.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/webhooks/{id}/ping", tag = "webhooks", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("id" = String, Path)),
    responses((status = 202, body = Queued), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn ping_repo_webhook(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, w)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let o = repo_owner(&state, &s, &ns, &repo).await?;
    ping(&state, &s, &o, &w).await
}

/// A webhook's deliveries, newest first (kept 30 days).
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/webhooks/{id}/deliveries", tag = "webhooks", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("id" = String, Path), DeliveryParams),
    responses((status = 200, body = Vec<Delivery>), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn list_repo_deliveries(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, w)): Path<(String, String, String)>,
    Query(q): Query<DeliveryParams>,
) -> Result<Json<Vec<Delivery>>, AppError> {
    let o = repo_owner(&state, &s, &ns, &repo).await?;
    deliveries(&state, &s, &o, &w, q.limit).await
}

/// One delivery with its payload and every attempt.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/webhooks/{id}/deliveries/{delivery}", tag = "webhooks", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("id" = String, Path), ("delivery" = String, Path)),
    responses((status = 200, body = DeliveryDetail), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn get_repo_delivery(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, w, d)): Path<(String, String, String, String)>,
) -> Result<Json<DeliveryDetail>, AppError> {
    let o = repo_owner(&state, &s, &ns, &repo).await?;
    delivery(&state, &s, &o, &w, &d).await
}

/// Sends a delivery's payload again, as a new delivery.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/webhooks/{id}/deliveries/{delivery}/redeliver", tag = "webhooks", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("id" = String, Path), ("delivery" = String, Path)),
    responses((status = 202, body = Queued), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn redeliver_repo_delivery(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, w, d)): Path<(String, String, String, String)>,
) -> Result<Response, AppError> {
    let o = repo_owner(&state, &s, &ns, &repo).await?;
    redeliver(&state, &s, &o, &w, &d).await
}

// Organization webhooks.

/// The organization's webhooks (owners); they receive the events of every
/// repository of the organization.
#[utoipa::path(get, path = "/api/v1/orgs/{org}/webhooks", tag = "webhooks", security(("token" = ["user:write"])), params(("org" = String, Path)),
    responses((status = 200, body = Vec<Webhook>), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn list_org_webhooks(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path(org): Path<String>,
) -> Result<Json<Vec<Webhook>>, AppError> {
    let o = org_owner(&state, &org).await?;
    list(&state, &s, &o).await
}

/// Creates an organization webhook (owners).
#[utoipa::path(post, path = "/api/v1/orgs/{org}/webhooks", tag = "webhooks", security(("token" = ["user:write"])), params(("org" = String, Path)), request_body = WebhookBody,
    responses((status = 201, body = Webhook), (status = 401, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_org_webhook(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path(org): Path<String>,
    Json(b): Json<WebhookBody>,
) -> Result<Response, AppError> {
    let o = org_owner(&state, &org).await?;
    create(&state, &s, &o, b).await
}

/// One organization webhook.
#[utoipa::path(get, path = "/api/v1/orgs/{org}/webhooks/{id}", tag = "webhooks", security(("token" = ["user:write"])), params(("org" = String, Path), ("id" = String, Path)),
    responses((status = 200, body = Webhook), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn get_org_webhook(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((org, w)): Path<(String, String)>,
) -> Result<Json<Webhook>, AppError> {
    let o = org_owner(&state, &org).await?;
    get(&state, &s, &o, &w).await
}

/// Changes an organization webhook.
#[utoipa::path(patch, path = "/api/v1/orgs/{org}/webhooks/{id}", tag = "webhooks", security(("token" = ["user:write"])), params(("org" = String, Path), ("id" = String, Path)), request_body = WebhookBody,
    responses((status = 200, body = Webhook), (status = 401, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn update_org_webhook(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((org, w)): Path<(String, String)>,
    Json(b): Json<WebhookBody>,
) -> Result<Json<Webhook>, AppError> {
    let o = org_owner(&state, &org).await?;
    update(&state, &s, &o, &w, b).await
}

/// Deletes an organization webhook.
#[utoipa::path(delete, path = "/api/v1/orgs/{org}/webhooks/{id}", tag = "webhooks", security(("token" = ["user:write"])), params(("org" = String, Path), ("id" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn delete_org_webhook(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((org, w)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let o = org_owner(&state, &org).await?;
    delete(&state, &s, &o, &w).await
}

/// Sends a `ping` event to an organization webhook.
#[utoipa::path(post, path = "/api/v1/orgs/{org}/webhooks/{id}/ping", tag = "webhooks", security(("token" = ["user:write"])), params(("org" = String, Path), ("id" = String, Path)),
    responses((status = 202, body = Queued), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn ping_org_webhook(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((org, w)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let o = org_owner(&state, &org).await?;
    ping(&state, &s, &o, &w).await
}

/// An organization webhook's deliveries.
#[utoipa::path(get, path = "/api/v1/orgs/{org}/webhooks/{id}/deliveries", tag = "webhooks", security(("token" = ["user:write"])), params(("org" = String, Path), ("id" = String, Path), DeliveryParams),
    responses((status = 200, body = Vec<Delivery>), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn list_org_deliveries(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((org, w)): Path<(String, String)>,
    Query(q): Query<DeliveryParams>,
) -> Result<Json<Vec<Delivery>>, AppError> {
    let o = org_owner(&state, &org).await?;
    deliveries(&state, &s, &o, &w, q.limit).await
}

/// One delivery of an organization webhook.
#[utoipa::path(get, path = "/api/v1/orgs/{org}/webhooks/{id}/deliveries/{delivery}", tag = "webhooks", security(("token" = ["user:write"])), params(("org" = String, Path), ("id" = String, Path), ("delivery" = String, Path)),
    responses((status = 200, body = DeliveryDetail), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn get_org_delivery(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((org, w, d)): Path<(String, String, String)>,
) -> Result<Json<DeliveryDetail>, AppError> {
    let o = org_owner(&state, &org).await?;
    delivery(&state, &s, &o, &w, &d).await
}

/// Sends a delivery of an organization webhook again.
#[utoipa::path(post, path = "/api/v1/orgs/{org}/webhooks/{id}/deliveries/{delivery}/redeliver", tag = "webhooks", security(("token" = ["user:write"])), params(("org" = String, Path), ("id" = String, Path), ("delivery" = String, Path)),
    responses((status = 202, body = Queued), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn redeliver_org_delivery(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((org, w, d)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let o = org_owner(&state, &org).await?;
    redeliver(&state, &s, &o, &w, &d).await
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as(
            "/api/v1/repos/{ns}/{repo}/webhooks",
            Access::Resource,
            get(list_repo_webhooks).post(create_repo_webhook),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/webhooks/{id}",
            Access::Resource,
            get(get_repo_webhook)
                .patch(update_repo_webhook)
                .delete(delete_repo_webhook),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/webhooks/{id}/ping",
            Access::Resource,
            post(ping_repo_webhook),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/webhooks/{id}/deliveries",
            Access::Resource,
            get(list_repo_deliveries),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/webhooks/{id}/deliveries/{delivery}",
            Access::Resource,
            get(get_repo_delivery),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/webhooks/{id}/deliveries/{delivery}/redeliver",
            Access::Resource,
            post(redeliver_repo_delivery),
        )
        .route_as(
            "/api/v1/orgs/{org}/webhooks",
            Access::Resource,
            get(list_org_webhooks).post(create_org_webhook),
        )
        .route_as(
            "/api/v1/orgs/{org}/webhooks/{id}",
            Access::Resource,
            get(get_org_webhook)
                .patch(update_org_webhook)
                .delete(delete_org_webhook),
        )
        .route_as(
            "/api/v1/orgs/{org}/webhooks/{id}/ping",
            Access::Resource,
            post(ping_org_webhook),
        )
        .route_as(
            "/api/v1/orgs/{org}/webhooks/{id}/deliveries",
            Access::Resource,
            get(list_org_deliveries),
        )
        .route_as(
            "/api/v1/orgs/{org}/webhooks/{id}/deliveries/{delivery}",
            Access::Resource,
            get(get_org_delivery),
        )
        .route_as(
            "/api/v1/orgs/{org}/webhooks/{id}/deliveries/{delivery}/redeliver",
            Access::Resource,
            post(redeliver_org_delivery),
        )
}
