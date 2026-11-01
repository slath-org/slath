// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The notification inbox, watch and subscribe buttons, email settings,
//! unsubscribe links, and the notifications API.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use slath_service::notifications::{INBOX_PAGE, InboxItem, Subscription};
use utoipa::{IntoParams, ToSchema};

use super::{PageCtx, Time, render};
use crate::AppState;
use crate::api::ApiUser;
use crate::auth::RequireSession;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

fn reason_text(r: &str) -> &'static str {
    match r {
        "assigned" => "Assigned to you",
        "mentioned" => "You were mentioned",
        "author" => "You opened it",
        "commented" => "You took part",
        "state_change" => "State changed",
        _ => "You're watching",
    }
}

struct Row {
    id: String,
    href: String,
    repo: String,
    number: i32,
    title: String,
    open: bool,
    reason: &'static str,
    unread: bool,
    updated: Time,
}

fn row(i: InboxItem) -> Row {
    Row {
        id: i.notification.id.to_string(),
        href: format!("/{}/issues/{}", i.repo, i.number),
        repo: i.repo,
        number: i.number,
        title: i.title,
        open: i.state == "open",
        reason: reason_text(&i.notification.reason),
        unread: i.notification.unread,
        updated: i.notification.updated_at.into(),
    }
}

#[derive(Template)]
#[template(path = "notifications.html")]
struct InboxPage {
    ctx: PageCtx,
    rows: Vec<Row>,
    all: bool,
    next: Option<String>,
}

#[derive(Deserialize, Default)]
struct InboxQuery {
    #[serde(default)]
    all: bool,
    page: Option<i64>,
}

/// `GET /-/notifications`: unread first view; `?all=true` shows read ones too.
async fn inbox(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Query(q): Query<InboxQuery>,
) -> Result<Response, AppError> {
    let page = q.page.unwrap_or(1).max(1);
    let items = state
        .services
        .inbox(&s.subject, !q.all, (page - 1) * INBOX_PAGE, INBOX_PAGE + 1)
        .await?;
    let more = items.len() as i64 > INBOX_PAGE;
    let rows: Vec<Row> = items
        .into_iter()
        .take(INBOX_PAGE as usize)
        .map(row)
        .collect();
    Ok(render(&InboxPage {
        ctx,
        rows,
        all: q.all,
        next: more.then(|| format!("/-/notifications?all={}&page={}", q.all, page + 1)),
    }))
}

#[derive(Deserialize)]
struct ReadForm {
    /// `read` or `unread`.
    #[serde(default)]
    mark: String,
}

async fn mark_one(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path(id): Path<String>,
    Form(f): Form<ReadForm>,
) -> Result<Response, AppError> {
    let id: uuid::Uuid = id.parse().map_err(|_| AppError::not_found())?;
    state
        .services
        .set_notification_unread(&s.subject, id, Some(f.mark == "unread"))
        .await?;
    Ok(Redirect::to("/-/notifications").into_response())
}

async fn mark_all(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
) -> Result<Response, AppError> {
    state
        .services
        .mark_all_notifications_read(&s.subject)
        .await?;
    Ok(Redirect::to("/-/notifications").into_response())
}

#[derive(Deserialize)]
struct SubscriptionForm {
    /// `watching`, `ignoring`, or `default`.
    state: String,
}

fn subscription(s: &str) -> Result<Subscription, AppError> {
    match s {
        "watching" => Ok(Subscription::Watching),
        "ignoring" => Ok(Subscription::Ignoring),
        "default" => Ok(Subscription::Default),
        _ => Err(AppError::bad_request("Unknown subscription state.")),
    }
}

async fn repo_subscription(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<SubscriptionForm>,
) -> Result<Response, AppError> {
    let to = subscription(&f.state)?;
    state
        .services
        .set_repo_subscription(&s.subject, &ns, &repo, to)
        .await?;
    Ok(Redirect::to(&format!("/{ns}/{repo}")).into_response())
}

async fn issue_subscription(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, i32)>,
    Form(f): Form<SubscriptionForm>,
) -> Result<Response, AppError> {
    let to = subscription(&f.state)?;
    state
        .services
        .set_issue_subscription(&s.subject, &ns, &repo, n, to)
        .await?;
    Ok(Redirect::to(&format!("/{ns}/{repo}/issues/{n}")).into_response())
}

#[derive(Template)]
#[template(path = "settings/notifications.html")]
struct SettingsPage {
    ctx: PageCtx,
    tab: &'static str,
    message: Option<&'static str>,
    email_on: bool,
    email_enabled: bool,
}

#[derive(Deserialize, Default)]
struct StatusQuery {
    status: Option<String>,
}

async fn settings(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    Ok(render(&SettingsPage {
        ctx,
        tab: "notifications",
        message: (q.status.as_deref() == Some("saved")).then_some("Notification settings saved."),
        email_on: state.services.email_notifications(&s.subject).await?,
        email_enabled: state.services.email_enabled(),
    }))
}

#[derive(Deserialize)]
struct SettingsForm {
    /// Present when the checkbox is checked.
    email: Option<String>,
}

async fn settings_save(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Form(f): Form<SettingsForm>,
) -> Result<Response, AppError> {
    state
        .services
        .set_email_notifications(&s.subject, f.email.is_some())
        .await?;
    Ok(Redirect::to("/-/settings/notifications?status=saved").into_response())
}

#[derive(Template)]
#[template(path = "unsubscribe.html")]
struct UnsubscribePage {
    ctx: PageCtx,
    token: String,
    target: Option<String>,
    done: bool,
}

/// `GET /-/unsubscribe/{token}`: asks for confirmation (mail scanners follow
/// links; only the POST changes anything).
async fn unsubscribe_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let target = state.services.unsubscribe_target(&token).await?;
    Ok(render(&UnsubscribePage {
        ctx,
        token,
        target,
        done: false,
    }))
}

/// `POST /-/unsubscribe/{token}`: the form above, and one-click
/// unsubscribe from mail clients (`List-Unsubscribe-Post`). The signed
/// token authorizes it, so it needs neither a session nor a CSRF token.
async fn unsubscribe(
    State(state): State<AppState>,
    ctx: PageCtx,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let target = state.services.unsubscribe_target(&token).await?;
    state.services.unsubscribe(&token).await?;
    Ok(render(&UnsubscribePage {
        ctx,
        token,
        target,
        done: true,
    }))
}

/// A notification in the API.
#[derive(Serialize, ToSchema)]
pub struct NotificationJson {
    pub id: String,
    /// `owner/repo`.
    pub repository: String,
    /// `issue` (merge requests later).
    pub subject_type: String,
    pub number: i32,
    pub title: String,
    /// The issue's state.
    pub state: String,
    /// Why you got it: `assigned`, `mentioned`, `author`, `commented`, `watching`, ….
    pub reason: String,
    pub unread: bool,
    pub updated_at: String,
}

#[derive(Serialize, ToSchema)]
pub struct NotificationList {
    pub items: Vec<NotificationJson>,
}

#[derive(Deserialize, IntoParams)]
pub struct NotificationQuery {
    /// Include read notifications.
    #[serde(default)]
    pub all: bool,
    /// Page number (50 per page).
    pub page: Option<i64>,
}

/// Your notifications, newest first; things you can no longer read are left out.
#[utoipa::path(get, path = "/api/v1/notifications", tag = "user", security(("token" = ["user:read"])), params(NotificationQuery),
    responses((status = 200, body = NotificationList), (status = 401, body = crate::api::Problem), (status = 403, body = crate::api::Problem)))]
pub async fn api_list(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Query(q): Query<NotificationQuery>,
) -> Result<Json<NotificationList>, AppError> {
    let page = q.page.unwrap_or(1).max(1);
    let items = state
        .services
        .inbox(&s, !q.all, (page - 1) * INBOX_PAGE, INBOX_PAGE)
        .await?;
    Ok(Json(NotificationList {
        items: items
            .into_iter()
            .map(|i| NotificationJson {
                id: i.notification.id.to_string(),
                repository: i.repo,
                subject_type: i.notification.subject_kind,
                number: i.number,
                title: i.title,
                state: i.state,
                reason: i.notification.reason,
                unread: i.notification.unread,
                updated_at: Time::from(i.notification.updated_at).iso,
            })
            .collect(),
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct NotificationPatch {
    /// Required.
    #[serde(default)]
    pub unread: Option<bool>,
}

/// Marks a notification read or unread.
#[utoipa::path(patch, path = "/api/v1/notifications/{id}", tag = "user", security(("token" = ["user:write"])), params(("id" = String, Path)), request_body = NotificationPatch,
    responses((status = 204), (status = 401, body = crate::api::Problem), (status = 403, body = crate::api::Problem), (status = 404, body = crate::api::Problem)))]
pub async fn api_patch(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path(id): Path<String>,
    Json(p): Json<NotificationPatch>,
) -> Result<StatusCode, AppError> {
    let id: uuid::Uuid = id.parse().map_err(|_| AppError::not_found())?;
    state
        .services
        .set_notification_unread(&s, id, p.unread)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as("/-/notifications", Access::Account, get(inbox))
        .route_as("/-/notifications/read", Access::Account, post(mark_all))
        .route_as(
            "/-/notifications/{id}/mark",
            Access::Account,
            post(mark_one),
        )
        .route_as(
            "/-/settings/notifications",
            Access::Account,
            get(settings).post(settings_save),
        )
        .route_as(
            "/{ns}/{repo}/subscription",
            Access::Resource,
            post(repo_subscription),
        )
        .route_as(
            "/{ns}/{repo}/issues/{n}/subscription",
            Access::Resource,
            post(issue_subscription),
        )
        .route_as(
            "/-/unsubscribe/{token}",
            Access::Public,
            get(unsubscribe_page).post(unsubscribe),
        )
}

pub fn api_router() -> Router<AppState> {
    use axum::routing::{get, patch};
    Router::new()
        .route_as("/api/v1/notifications", Access::Account, get(api_list))
        .route_as(
            "/api/v1/notifications/{id}",
            Access::Account,
            patch(api_patch),
        )
}
