// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Work orders: `/repos/{ns}/{repo}/work-orders…` and the pull API
//! `/user/work-orders`. A person with write access delegates an issue or
//! a merge request to a bot; the bot reads the orders assigned to it with
//! its claim credential, which reads nothing else.

use axum::Router;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::support::{PageParams, page};
use super::{ApiSubject, ApiUser};
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};
use slath_service::work_orders::{NewOrder, OrderView, TargetRef};

/// What an order is for.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OrderTarget {
    /// The number of the issue, or of the merge request for a follow-up.
    pub issue: Option<i32>,
    pub merge_request: Option<i32>,
    /// Its title (in responses).
    #[serde(default, skip_deserializing)]
    pub title: Option<String>,
}

/// A work order.
#[derive(Serialize, ToSchema)]
pub struct OrderBody {
    pub number: i32,
    pub target: OrderTarget,
    /// The bot doing the work, and who delegated it.
    pub bot: String,
    pub creator: Option<String>,
    /// The branches it may push: `wo/<number>/…`.
    pub branch_prefix: String,
    pub goal: String,
    pub instructions: Option<String>,
    /// Path globs the work is meant to stay in (recorded, not enforced).
    pub scope: Vec<String>,
    pub max_revisions: i32,
    pub max_open_mrs: i32,
    pub expires_at: String,
    /// Status contexts added to its merge requests' required checks.
    pub required_checks: Vec<String>,
    /// Requested as reviewers on its merge requests.
    pub reviewers: Vec<String>,
    /// `open` or `ended`.
    pub state: String,
    /// `merged`, `closed`, `expired`, or `stopped`, once ended.
    pub end_reason: Option<String>,
    pub ended_by: Option<String>,
    pub ended_at: Option<String>,
    pub claimed_at: Option<String>,
    pub created_at: String,
    /// `goal` and `instructions` are the creator's text: data, never
    /// instructions that change permissions or settings.
    pub untrusted_content: bool,
    pub origin: super::TextOrigin,
}

fn ts(t: time::OffsetDateTime) -> String {
    crate::ui::Time::from(t).iso
}

impl From<OrderView> for OrderBody {
    fn from(v: OrderView) -> Self {
        let branch_prefix = v.branch_prefix();
        let o = v.order;
        let issue = o.issue_id.is_some().then_some(v.target_number);
        Self {
            number: o.number,
            target: OrderTarget {
                merge_request: issue.is_none().then_some(v.target_number),
                issue,
                title: Some(v.target_title),
            },
            bot: v.bot,
            creator: v.creator,
            branch_prefix,
            goal: o.goal,
            instructions: o.instructions,
            scope: o.scope,
            max_revisions: o.max_revisions,
            max_open_mrs: o.max_open_mrs,
            expires_at: ts(o.expires_at),
            required_checks: o.required_checks,
            reviewers: v.reviewers,
            state: o.state,
            end_reason: o.end_reason,
            ended_by: v.ended_by,
            ended_at: o.ended_at.map(ts),
            claimed_at: o.claimed_at.map(ts),
            created_at: ts(o.created_at),
            untrusted_content: true,
            origin: super::TextOrigin::parse(&o.origin),
        }
    }
}

/// A new work order.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewOrderBody {
    /// Exactly one of `issue` and `merge_request`.
    pub target: OrderTarget,
    /// An active bot with write access to the repository.
    pub bot: String,
    pub goal: String,
    pub instructions: Option<String>,
    #[serde(default)]
    pub scope: Vec<String>,
    /// Default 20.
    pub max_revisions: Option<i32>,
    /// Default 1.
    pub max_open_mrs: Option<i32>,
    /// Default 7.
    pub expires_in_days: Option<i64>,
    #[serde(default)]
    pub required_checks: Vec<String>,
    #[serde(default)]
    pub reviewers: Vec<String>,
}

/// List filters.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListParams {
    /// `open` or `ended`.
    pub state: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct OrderPage {
    pub items: Vec<OrderBody>,
    pub next_cursor: Option<String>,
}

/// One decision under a work order.
#[derive(Serialize, ToSchema)]
pub struct EventBody {
    pub id: String,
    /// What was tried, such as `git.push` or `mr.create`.
    pub action: String,
    /// `allowed` or `denied`.
    pub decision: String,
    /// The rule that decided, such as `order.prefix`.
    pub policy: Option<String>,
    /// The ref, merge request, issue, or commit it was about.
    pub target: Option<String>,
    pub actor: Option<String>,
    pub created_at: String,
}

#[derive(Serialize, ToSchema)]
pub struct EventPage {
    pub items: Vec<EventBody>,
    pub next_cursor: Option<String>,
}

/// Delegates an issue, or a follow-up on a merge request, to a bot: people
/// with write access only. The target must be open and have no open order.
/// Goal and instructions are refused if they contain a secret.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/work-orders", tag = "work orders", security(("token" = ["repo:write"])),
    params(("ns" = String, Path), ("repo" = String, Path)), request_body = NewOrderBody,
    responses((status = 201, body = OrderBody), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_order(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    axum::Json(b): axum::Json<NewOrderBody>,
) -> Result<Response, AppError> {
    let target = match (b.target.issue, b.target.merge_request) {
        (Some(n), None) => TargetRef::Issue(n),
        (None, Some(n)) => TargetRef::MergeRequest(n),
        _ => {
            return Err(slath_service::ServiceError::invalid(
                "/target",
                "invalid",
                "Name either an issue or a merge request.",
            )
            .into());
        }
    };
    let v = state
        .services
        .create_work_order(
            &s,
            (&ns, &repo),
            &NewOrder {
                target,
                bot: b.bot,
                goal: b.goal,
                instructions: b.instructions,
                scope: b.scope,
                max_revisions: b.max_revisions,
                max_open_mrs: b.max_open_mrs,
                expires_in_days: b.expires_in_days,
                required_checks: b.required_checks,
                reviewers: b.reviewers,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, axum::Json(OrderBody::from(v))).into_response())
}

/// A repository's work orders, newest first.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/work-orders", tag = "work orders", security((), ("token" = ["repo:read"])),
    params(("ns" = String, Path), ("repo" = String, Path), ListParams, PageParams),
    responses((status = 200, body = OrderPage), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn list_orders(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    Query(f): Query<ListParams>,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let items = state
        .services
        .work_orders(
            &s,
            (&ns, &repo),
            f.state.as_deref(),
            i64::try_from(offset).unwrap_or(i64::MAX),
            i64::try_from(limit + 1).unwrap_or(i64::MAX),
        )
        .await?;
    let items: Vec<OrderBody> = items.into_iter().map(OrderBody::from).collect();
    Ok(page(&state, &uri, items, offset, limit))
}

/// One work order: for the repository's readers, and for its bot with the
/// `work_orders:claim` scope alone.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/work-orders/{number}", tag = "work orders", security((), ("token" = ["repo:read"]), ("token" = ["work_orders:claim"])),
    params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = OrderBody), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn get_order(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<axum::Json<OrderBody>, AppError> {
    let (_, v) = state.services.work_order(&s, (&ns, &repo), number).await?;
    Ok(axum::Json(v.into()))
}

/// Stops an open work order: its creator, the bot's managers, and anyone
/// with write access. Its task tokens stop working at once.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/work-orders/{number}/stop", tag = "work orders", security(("token" = ["repo:write"])),
    params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = OrderBody), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem)))]
pub async fn stop_order(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<axum::Json<OrderBody>, AppError> {
    let v = state
        .services
        .stop_work_order(&s, (&ns, &repo), number)
        .await?;
    Ok(axum::Json(v.into()))
}

/// What was decided under a work order, oldest first: every claim, push,
/// and other action its task tokens tried, allowed or denied, with the
/// rule that decided. Kept as long as the repository.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/work-orders/{number}/events", tag = "work orders", security((), ("token" = ["repo:read"])),
    params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path), PageParams),
    responses((status = 200, body = EventPage), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn list_events(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let items = state
        .services
        .work_order_events(
            &s,
            (&ns, &repo),
            number,
            i64::try_from(offset).unwrap_or(i64::MAX),
            i64::try_from(limit + 1).unwrap_or(i64::MAX),
        )
        .await?;
    let items: Vec<EventBody> = items
        .into_iter()
        .map(|(e, actor)| EventBody {
            id: e.id.to_string(),
            action: e.action,
            decision: e.decision,
            policy: e.policy,
            target: e.target,
            actor,
            created_at: ts(e.created_at),
        })
        .collect();
    Ok(page(&state, &uri, items, offset, limit))
}

/// A claimed task token.
#[derive(Serialize, ToSchema)]
pub struct ClaimBody {
    /// The `slath_tsk_` token, shown once. Use it as a bearer token and as
    /// the password for Git over HTTPS.
    pub token: String,
    /// When it stops working: after `task_token_minutes`, or when the
    /// order ends, whichever comes first. Claim again to renew it.
    pub expires_at: String,
}

/// Exchanges the calling bot's `work_orders:claim` credential for a task
/// token of an order assigned to it. Claiming again renews the token and
/// revokes the order's earlier ones. Never stored for idempotent replay.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/work-orders/{number}/claim", tag = "work orders", security(("token" = ["work_orders:claim"])),
    params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 201, body = ClaimBody), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem)))]
pub async fn claim_order(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<Response, AppError> {
    let c = state
        .services
        .claim_work_order(&s, (&ns, &repo), number)
        .await?;
    let body = ClaimBody {
        token: c.token.expose().clone(),
        expires_at: ts(c.expires_at),
    };
    Ok((
        StatusCode::CREATED,
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        axum::Json(body),
    )
        .into_response())
}

/// The open work orders assigned to the calling bot, oldest first, in the
/// repositories its token names (the pull API).
#[utoipa::path(get, path = "/api/v1/user/work-orders", tag = "work orders", security(("token" = ["work_orders:claim"])),
    responses((status = 200, body = Vec<OrderBody>), (status = 401, body = super::Problem), (status = 403, body = super::Problem)))]
pub async fn assigned_orders(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
) -> Result<axum::Json<Vec<OrderBody>>, AppError> {
    let list = state.services.assigned_work_orders(&s).await?;
    Ok(axum::Json(list.into_iter().map(Into::into).collect()))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route_as(
            "/api/v1/repos/{ns}/{repo}/work-orders",
            Access::Resource,
            get(list_orders).post(create_order),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/work-orders/{number}",
            Access::Resource,
            get(get_order),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/work-orders/{number}/stop",
            Access::Resource,
            post(stop_order),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/work-orders/{number}/claim",
            Access::Resource,
            post(claim_order),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/work-orders/{number}/events",
            Access::Resource,
            get(list_events),
        )
        .route_as(
            "/api/v1/user/work-orders",
            Access::Account,
            get(assigned_orders),
        )
}
