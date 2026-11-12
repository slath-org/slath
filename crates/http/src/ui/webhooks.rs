// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Webhook settings of repositories (Settings → Webhooks, administrators)
//! and organizations (Webhooks in the side menu, owners): the list, adding,
//! one webhook with its deliveries, and one delivery with its attempts.
//! Repositories and organizations share the page bodies.

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use slath_core::authz::{Action, Subject};
use slath_db::orgs::NamespaceKind;
use slath_service::repos::RepoAccess;
use slath_service::webhooks::{WebhookInput, WebhookOwner};
use slath_service::{FieldError, ServiceError};

use super::repos::RepoHeader;
use super::settings::error_for;
use super::{PageCtx, Time, render};
use crate::AppState;
use crate::auth::{MaybeSession, Sudo};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// The events with what they're about, in the order the form shows them.
const EVENT_CHOICES: &[(&str, &str)] = &[
    (
        "push",
        "Pushes: branches and tags created, updated, or deleted",
    ),
    (
        "merge_request",
        "Merge requests opened, updated, ready, closed, reopened, or merged",
    ),
    (
        "merge_request_comment",
        "Comments on merge requests and in their review threads",
    ),
    ("merge_request_review", "Reviews submitted"),
    ("issue", "Issues opened, closed, reopened, or assigned"),
    ("issue_comment", "Comments on issues"),
    ("status", "Statuses reported on commits"),
    ("fork", "Forks"),
    (
        "repo",
        "The repository created, renamed, deleted, or its visibility changed",
    ),
];

/// Whose webhooks a page is about.
enum Place {
    Repo(Box<RepoAccess>),
    Org {
        name: String,
        id: slath_core::ids::OrgId,
    },
}

impl Place {
    fn owner(&self) -> WebhookOwner<'_> {
        match self {
            Self::Repo(a) => WebhookOwner::Repo(a),
            Self::Org { id, .. } => WebhookOwner::Org(*id),
        }
    }

    /// Where the webhook pages are.
    fn base(&self) -> String {
        match self {
            Self::Repo(a) => format!("/{}/{}/settings/webhooks", a.repo.namespace, a.repo.name),
            Self::Org { name, .. } => format!("/{name}/-/webhooks"),
        }
    }
}

async fn repo_place(
    state: &AppState,
    s: &Subject,
    ns: &str,
    repo: &str,
) -> Result<Place, AppError> {
    match state
        .services
        .repo_access(s, ns, repo, Action::WebhooksManage)
        .await
    {
        Err(ServiceError::Forbidden) => Err(AppError::not_found()),
        other => Ok(Place::Repo(Box::new(other?))),
    }
}

async fn org_place(state: &AppState, s: &Subject, org: &str) -> Result<Place, AppError> {
    let Some((name, NamespaceKind::Org(id))) = state.services.resolve_namespace(org).await? else {
        return Err(AppError::not_found());
    };
    let place = Place::Org { name, id };
    // Only owners have these pages.
    state.services.webhooks(s, place.owner()).await?;
    Ok(place)
}

fn id(s: &str) -> Result<uuid::Uuid, AppError> {
    s.parse().map_err(|_| AppError::not_found())
}

/// One event checkbox.
pub struct EventChoice {
    pub name: &'static str,
    pub label: &'static str,
    pub checked: bool,
}

fn choices(selected: &[String]) -> Vec<EventChoice> {
    EVENT_CHOICES
        .iter()
        .map(|(name, label)| EventChoice {
            name,
            label,
            checked: selected.iter().any(|s| s == name),
        })
        .collect()
}

/// A delivery as the pages list it.
pub struct DeliveryRow {
    pub id: String,
    pub short: String,
    pub event: String,
    pub state: String,
    /// `done`, `waiting`, or `blocking` (the tone icons).
    pub tone: &'static str,
    pub status: Option<i32>,
    pub attempts: i32,
    pub created: Time,
    pub next: Option<Time>,
}

fn delivery_row(d: &slath_db::webhooks::Delivery) -> DeliveryRow {
    let id = d.id.to_string();
    DeliveryRow {
        short: id[id.len() - 12..].to_owned(),
        event: match &d.action {
            Some(a) => format!("{} · {a}", d.event),
            None => d.event.clone(),
        },
        tone: match d.state.as_str() {
            "delivered" => "done",
            "pending" => "waiting",
            _ => "blocking",
        },
        state: match d.state.as_str() {
            "delivered" => "Delivered".to_owned(),
            "pending" if d.attempts == 0 => "Queued".to_owned(),
            "pending" => "Failed, will retry".to_owned(),
            _ => "Failed".to_owned(),
        },
        status: d.last_status,
        attempts: d.attempts,
        created: d.created_at.into(),
        next: d.next_attempt_at.map(Into::into),
        id,
    }
}

/// A webhook in the list.
pub struct HookRow {
    pub id: String,
    pub url: String,
    pub events: String,
    pub active: bool,
    pub disabled_reason: Option<String>,
    pub failures: i32,
}

/// The list page's data.
pub struct ListView {
    pub base: String,
    pub hooks: Vec<HookRow>,
    pub message: Option<&'static str>,
    pub errors: Vec<FieldError>,
    pub url: String,
    pub events: Vec<EventChoice>,
    pub active: bool,
    pub max: usize,
}

/// One webhook's page data.
pub struct HookView {
    pub base: String,
    pub id: String,
    pub url: String,
    pub active: bool,
    pub disabled_reason: Option<String>,
    pub failures: i32,
    pub events: Vec<EventChoice>,
    /// The secret Slath just generated, shown this once.
    pub new_secret: Option<String>,
    pub message: Option<&'static str>,
    pub errors: Vec<FieldError>,
    pub deliveries: Vec<DeliveryRow>,
}

/// An attempt as the delivery page shows it.
pub struct AttemptRow {
    pub attempt: i32,
    pub started: Time,
    pub duration_ms: i32,
    pub status: Option<i32>,
    pub response: Option<String>,
    pub error: Option<String>,
}

/// One delivery's page data.
pub struct DeliveryView {
    pub base: String,
    pub hook: String,
    pub hook_url: String,
    pub d: DeliveryRow,
    pub redelivery_of: Option<String>,
    pub payload: String,
    pub attempts: Vec<AttemptRow>,
    pub message: Option<&'static str>,
}

macro_rules! pages {
    ($list:ident, $lp:literal, $hook:ident, $hp:literal, $delivery:ident, $dp:literal, $($ctx_field:ident : $ty:ty),*) => {
        #[derive(Template)]
        #[template(path = $lp)]
        struct $list {
            ctx: PageCtx,
            $($ctx_field: $ty,)*
            v: ListView,
        }

        impl $list {
            fn error(&self, field: &str) -> Option<&str> {
                error_for(&self.v.errors, field)
            }
        }

        #[derive(Template)]
        #[template(path = $hp)]
        struct $hook {
            ctx: PageCtx,
            $($ctx_field: $ty,)*
            v: HookView,
        }

        impl $hook {
            fn error(&self, field: &str) -> Option<&str> {
                error_for(&self.v.errors, field)
            }
        }

        #[derive(Template)]
        #[template(path = $dp)]
        struct $delivery {
            ctx: PageCtx,
            $($ctx_field: $ty,)*
            v: DeliveryView,
        }
    };
}

pages!(
    RepoList, "repo/settings_webhooks.html",
    RepoHook, "repo/settings_webhook.html",
    RepoDelivery, "repo/settings_webhook_delivery.html",
    header: RepoHeader
);
pages!(
    OrgList, "org_webhooks.html",
    OrgHook, "org_webhook.html",
    OrgDelivery, "org_webhook_delivery.html",
    org: String
);

fn message(status: Option<&str>) -> Option<&'static str> {
    Some(match status? {
        "saved" => "Webhook saved.",
        "deleted" => "Webhook deleted.",
        "ping" => "Ping sent. It shows in the deliveries below.",
        "redelivered" => "Sent again as a new delivery.",
        _ => return None,
    })
}

async fn render_list(
    state: &AppState,
    ctx: PageCtx,
    s: &Subject,
    place: &Place,
    (msg, errors, form): (Option<&'static str>, Vec<FieldError>, Option<WebhookInput>),
) -> Result<Response, AppError> {
    let hooks = state.services.webhooks(s, place.owner()).await?;
    let failed = !errors.is_empty();
    let form = form.unwrap_or(WebhookInput {
        active: true,
        events: vec!["push".to_owned()],
        ..WebhookInput::default()
    });
    let v = ListView {
        base: place.base(),
        hooks: hooks
            .into_iter()
            .map(|w| HookRow {
                id: w.id.to_string(),
                url: w.url,
                events: w.events.join(", "),
                active: w.active,
                disabled_reason: w.disabled_reason,
                failures: w.consecutive_failures,
            })
            .collect(),
        message: msg,
        errors,
        url: form.url,
        events: choices(&form.events),
        active: form.active,
        max: state.config.limits.webhooks_per_owner,
    };
    let mut res = match place {
        Place::Repo(a) => render(&RepoList {
            header: RepoHeader::load(state, a, s, "settings").await,
            ctx,
            v,
        }),
        Place::Org { name, .. } => render(&OrgList {
            org: name.clone(),
            ctx,
            v,
        }),
    };
    if failed {
        *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
    }
    Ok(res)
}

async fn render_hook(
    state: &AppState,
    ctx: PageCtx,
    s: &Subject,
    place: &Place,
    hook: uuid::Uuid,
    (msg, errors, new_secret): (Option<&'static str>, Vec<FieldError>, Option<String>),
) -> Result<Response, AppError> {
    let w = state.services.webhook(s, place.owner(), hook).await?;
    let deliveries = state
        .services
        .webhook_deliveries(s, place.owner(), hook, 50)
        .await?;
    let failed = !errors.is_empty();
    let v = HookView {
        base: place.base(),
        id: w.id.to_string(),
        url: w.url,
        active: w.active,
        disabled_reason: w.disabled_reason,
        failures: w.consecutive_failures,
        events: choices(&w.events),
        new_secret,
        message: msg,
        errors,
        deliveries: deliveries.iter().map(delivery_row).collect(),
    };
    let mut res = match place {
        Place::Repo(a) => render(&RepoHook {
            header: RepoHeader::load(state, a, s, "settings").await,
            ctx,
            v,
        }),
        Place::Org { name, .. } => render(&OrgHook {
            org: name.clone(),
            ctx,
            v,
        }),
    };
    if failed {
        *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
    }
    Ok(res)
}

async fn render_delivery(
    state: &AppState,
    ctx: PageCtx,
    s: &Subject,
    place: &Place,
    (hook, delivery): (uuid::Uuid, uuid::Uuid),
    msg: Option<&'static str>,
) -> Result<Response, AppError> {
    let w = state.services.webhook(s, place.owner(), hook).await?;
    let (d, attempts) = state
        .services
        .webhook_delivery(s, place.owner(), (hook, delivery))
        .await?;
    let v = DeliveryView {
        base: place.base(),
        hook: hook.to_string(),
        hook_url: w.url,
        d: delivery_row(&d),
        redelivery_of: d.redelivery_of.map(|i| i.to_string()),
        payload: serde_json::to_string_pretty(&d.payload).unwrap_or_default(),
        attempts: attempts
            .into_iter()
            .map(|a| AttemptRow {
                attempt: a.attempt,
                started: a.started_at.into(),
                duration_ms: a.duration_ms,
                status: a.status,
                response: a.response.filter(|r| !r.is_empty()),
                error: a.error,
            })
            .collect(),
        message: msg,
    };
    Ok(match place {
        Place::Repo(a) => render(&RepoDelivery {
            header: RepoHeader::load(state, a, s, "settings").await,
            ctx,
            v,
        }),
        Place::Org { name, .. } => render(&OrgDelivery {
            org: name.clone(),
            ctx,
            v,
        }),
    })
}

/// The form's fields: URL, events (repeated), secret, active.
fn form_input(body: &[u8]) -> WebhookInput {
    let pairs = crate::auth::form_pairs(body);
    let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    WebhookInput {
        url: get("url").unwrap_or_default(),
        events: pairs
            .iter()
            .filter(|(n, _)| n == "events")
            .map(|(_, v)| v.clone())
            .collect(),
        active: get("active").is_some(),
        secret: get("secret").filter(|s| !s.is_empty()),
    }
}

#[derive(serde::Deserialize, Default)]
struct StatusQuery {
    status: Option<String>,
}

// Handlers shared by repositories and organizations.

async fn list(
    state: AppState,
    ctx: PageCtx,
    s: Subject,
    place: Place,
    q: StatusQuery,
) -> Result<Response, AppError> {
    render_list(
        &state,
        ctx,
        &s,
        &place,
        (message(q.status.as_deref()), Vec::new(), None),
    )
    .await
}

async fn create(
    state: AppState,
    ctx: PageCtx,
    s: Subject,
    place: Place,
    body: &[u8],
) -> Result<Response, AppError> {
    let input = form_input(body);
    match state
        .services
        .create_webhook(&s, place.owner(), &input)
        .await
    {
        // A generated secret is shown on this answer only, so it's not a
        // redirect.
        Ok((w, Some(secret))) => {
            render_hook(
                &state,
                ctx,
                &s,
                &place,
                w.id,
                (Some("Webhook added."), Vec::new(), Some(secret)),
            )
            .await
        }
        Ok((w, None)) => {
            Ok(Redirect::to(&format!("{}/{}?status=saved", place.base(), w.id)).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            render_list(&state, ctx, &s, &place, (None, errors, Some(input))).await
        }
        Err(e) => Err(e.into()),
    }
}

async fn show(
    state: AppState,
    ctx: PageCtx,
    s: Subject,
    place: Place,
    hook: &str,
    q: StatusQuery,
) -> Result<Response, AppError> {
    render_hook(
        &state,
        ctx,
        &s,
        &place,
        id(hook)?,
        (message(q.status.as_deref()), Vec::new(), None),
    )
    .await
}

async fn update(
    state: AppState,
    ctx: PageCtx,
    s: Subject,
    place: Place,
    hook: &str,
    body: &[u8],
) -> Result<Response, AppError> {
    let hook = id(hook)?;
    let input = form_input(body);
    match state
        .services
        .update_webhook(&s, place.owner(), hook, &input)
        .await
    {
        Ok(_) => Ok(Redirect::to(&format!("{}/{hook}?status=saved", place.base())).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            render_hook(&state, ctx, &s, &place, hook, (None, errors, None)).await
        }
        Err(e) => Err(e.into()),
    }
}

async fn ping(state: AppState, s: Subject, place: Place, hook: &str) -> Result<Response, AppError> {
    let hook = id(hook)?;
    state.services.ping_webhook(&s, place.owner(), hook).await?;
    Ok(Redirect::to(&format!("{}/{hook}?status=ping", place.base())).into_response())
}

async fn delete(
    state: AppState,
    s: Subject,
    place: Place,
    hook: &str,
) -> Result<Response, AppError> {
    state
        .services
        .delete_webhook(&s, place.owner(), id(hook)?)
        .await?;
    Ok(Redirect::to(&format!("{}?status=deleted", place.base())).into_response())
}

async fn delivery(
    state: AppState,
    ctx: PageCtx,
    s: Subject,
    place: Place,
    (hook, d): (&str, &str),
    q: StatusQuery,
) -> Result<Response, AppError> {
    render_delivery(
        &state,
        ctx,
        &s,
        &place,
        (id(hook)?, id(d)?),
        message(q.status.as_deref()),
    )
    .await
}

async fn redeliver(
    state: AppState,
    s: Subject,
    place: Place,
    (hook, d): (&str, &str),
) -> Result<Response, AppError> {
    let hook = id(hook)?;
    let new = state
        .services
        .redeliver_webhook(&s, place.owner(), (hook, id(d)?))
        .await?;
    Ok(Redirect::to(&format!(
        "{}/{hook}/deliveries/{new}?status=redelivered",
        place.base()
    ))
    .into_response())
}

// Repository routes.

async fn repo_list(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<StatusQuery>,
) -> Result<Response, AppError> {
    let s = session.subject();
    let place = repo_place(&state, &s, &ns, &repo).await?;
    list(state, ctx, s, place, q).await
}

async fn repo_create(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Path((ns, repo)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    let s = session.subject;
    let place = repo_place(&state, &s, &ns, &repo).await?;
    create(state, ctx, s, place, &body).await
}

async fn repo_show(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo, hook)): Path<(String, String, String)>,
    axum::extract::Query(q): axum::extract::Query<StatusQuery>,
) -> Result<Response, AppError> {
    let s = session.subject();
    let place = repo_place(&state, &s, &ns, &repo).await?;
    show(state, ctx, s, place, &hook, q).await
}

async fn repo_update(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Path((ns, repo, hook)): Path<(String, String, String)>,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    let s = session.subject;
    let place = repo_place(&state, &s, &ns, &repo).await?;
    update(state, ctx, s, place, &hook, &body).await
}

async fn repo_ping(
    State(state): State<AppState>,
    Sudo(session): Sudo,
    Path((ns, repo, hook)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let s = session.subject;
    let place = repo_place(&state, &s, &ns, &repo).await?;
    ping(state, s, place, &hook).await
}

async fn repo_delete(
    State(state): State<AppState>,
    Sudo(session): Sudo,
    Path((ns, repo, hook)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let s = session.subject;
    let place = repo_place(&state, &s, &ns, &repo).await?;
    delete(state, s, place, &hook).await
}

async fn repo_delivery(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo, hook, d)): Path<(String, String, String, String)>,
    axum::extract::Query(q): axum::extract::Query<StatusQuery>,
) -> Result<Response, AppError> {
    let s = session.subject();
    let place = repo_place(&state, &s, &ns, &repo).await?;
    delivery(state, ctx, s, place, (&hook, &d), q).await
}

async fn repo_redeliver(
    State(state): State<AppState>,
    Sudo(session): Sudo,
    Path((ns, repo, hook, d)): Path<(String, String, String, String)>,
) -> Result<Response, AppError> {
    let s = session.subject;
    let place = repo_place(&state, &s, &ns, &repo).await?;
    redeliver(state, s, place, (&hook, &d)).await
}

// Organization routes.

async fn org_list(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path(org): Path<String>,
    axum::extract::Query(q): axum::extract::Query<StatusQuery>,
) -> Result<Response, AppError> {
    let s = session.subject();
    let place = org_place(&state, &s, &org).await?;
    list(state, ctx, s, place, q).await
}

async fn org_create(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Path(org): Path<String>,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    let s = session.subject;
    let place = org_place(&state, &s, &org).await?;
    create(state, ctx, s, place, &body).await
}

async fn org_show(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((org, hook)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<StatusQuery>,
) -> Result<Response, AppError> {
    let s = session.subject();
    let place = org_place(&state, &s, &org).await?;
    show(state, ctx, s, place, &hook, q).await
}

async fn org_update(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Path((org, hook)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    let s = session.subject;
    let place = org_place(&state, &s, &org).await?;
    update(state, ctx, s, place, &hook, &body).await
}

async fn org_ping(
    State(state): State<AppState>,
    Sudo(session): Sudo,
    Path((org, hook)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let s = session.subject;
    let place = org_place(&state, &s, &org).await?;
    ping(state, s, place, &hook).await
}

async fn org_delete(
    State(state): State<AppState>,
    Sudo(session): Sudo,
    Path((org, hook)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let s = session.subject;
    let place = org_place(&state, &s, &org).await?;
    delete(state, s, place, &hook).await
}

async fn org_delivery(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((org, hook, d)): Path<(String, String, String)>,
    axum::extract::Query(q): axum::extract::Query<StatusQuery>,
) -> Result<Response, AppError> {
    let s = session.subject();
    let place = org_place(&state, &s, &org).await?;
    delivery(state, ctx, s, place, (&hook, &d), q).await
}

async fn org_redeliver(
    State(state): State<AppState>,
    Sudo(session): Sudo,
    Path((org, hook, d)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let s = session.subject;
    let place = org_place(&state, &s, &org).await?;
    redeliver(state, s, place, (&hook, &d)).await
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as(
            "/{ns}/{repo}/settings/webhooks",
            Access::Resource,
            get(repo_list).post(repo_create),
        )
        .route_as(
            "/{ns}/{repo}/settings/webhooks/{id}",
            Access::Resource,
            get(repo_show).post(repo_update),
        )
        .route_as(
            "/{ns}/{repo}/settings/webhooks/{id}/ping",
            Access::Resource,
            post(repo_ping),
        )
        .route_as(
            "/{ns}/{repo}/settings/webhooks/{id}/delete",
            Access::Resource,
            post(repo_delete),
        )
        .route_as(
            "/{ns}/{repo}/settings/webhooks/{id}/deliveries/{delivery}",
            Access::Resource,
            get(repo_delivery),
        )
        .route_as(
            "/{ns}/{repo}/settings/webhooks/{id}/deliveries/{delivery}/redeliver",
            Access::Resource,
            post(repo_redeliver),
        )
        .route_as(
            "/{org}/-/webhooks",
            Access::Resource,
            get(org_list).post(org_create),
        )
        .route_as(
            "/{org}/-/webhooks/{id}",
            Access::Resource,
            get(org_show).post(org_update),
        )
        .route_as(
            "/{org}/-/webhooks/{id}/ping",
            Access::Resource,
            post(org_ping),
        )
        .route_as(
            "/{org}/-/webhooks/{id}/delete",
            Access::Resource,
            post(org_delete),
        )
        .route_as(
            "/{org}/-/webhooks/{id}/deliveries/{delivery}",
            Access::Resource,
            get(org_delivery),
        )
        .route_as(
            "/{org}/-/webhooks/{id}/deliveries/{delivery}/redeliver",
            Access::Resource,
            post(org_redeliver),
        )
}
