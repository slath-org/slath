// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Push protection pages behind the links in a rejected push: allowing one
//! finding or all findings of a push (whoever may allow findings), and
//! accepting all findings of a first push into an empty repository
//! (repository administrators).

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::authz::{Action, Subject};
use slath_service::push_protection::ALLOWANCE_DAYS;
use slath_service::repos::RepoAccess;
use slath_service::{FieldError, ServiceError};

use super::repos::RepoHeader;
use super::settings::error_for;
use super::{PageCtx, Time, render};
use crate::AppState;
use crate::auth::{MaybeSession, RequireSession, Sudo};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

struct AllowanceView {
    fingerprint: String,
    rule: String,
    by: String,
    reason: Option<String>,
    expires: Time,
}

#[derive(Template)]
#[template(path = "repo/secret_allow.html")]
struct AllowPage {
    ctx: PageCtx,
    header: RepoHeader,
    fingerprint: String,
    rule: String,
    days: Vec<i64>,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    allowances: Vec<AllowanceView>,
    /// The rejected push the finding came from, when it had several, and
    /// how many: they can be allowed at once.
    push: Option<(uuid::Uuid, usize)>,
}

impl AllowPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

#[derive(Template)]
#[template(path = "repo/secret_accept.html")]
struct AcceptPage {
    ctx: PageCtx,
    header: RepoHeader,
    id: uuid::Uuid,
    findings: Vec<(String, String)>,
    /// A first push into an empty repository (administrators, one day);
    /// otherwise a push with several findings (a period and a reason).
    first_push: bool,
    days: Vec<i64>,
    errors: Vec<FieldError>,
}

impl AcceptPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

#[derive(Deserialize, Default)]
struct AllowQuery {
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    rule: String,
    status: Option<String>,
    /// The rejected push, when it had several findings.
    push: Option<String>,
}

#[derive(Deserialize, Default)]
struct AcceptForm {
    #[serde(default)]
    days: String,
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize)]
struct AllowForm {
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    rule: String,
    #[serde(default)]
    days: String,
    #[serde(default)]
    reason: String,
}

async fn access(
    state: &AppState,
    subject: &Subject,
    ns: &str,
    repo: &str,
    action: Action,
) -> Result<RepoAccess, AppError> {
    match state.services.repo_access(subject, ns, repo, action).await {
        Err(ServiceError::Forbidden) => Err(AppError::not_found()),
        other => Ok(other?),
    }
}

async fn allow_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    a: &RepoAccess,
    // The finding, and the rejected push it came from.
    (fingerprint, rule, push): (String, String, Option<uuid::Uuid>),
    message: Option<&'static str>,
    errors: Vec<FieldError>,
) -> Result<Response, AppError> {
    // The push's other findings, if the viewer may allow them.
    let push = match push {
        Some(id) => state
            .services
            .push_findings(subject, a, id)
            .await
            .ok()
            .filter(|f| f.len() > 1)
            .map(|f| (id, f.len())),
        None => None,
    };
    let allowances = state
        .services
        .secret_allowances(subject, a)
        .await?
        .into_iter()
        .map(|x| AllowanceView {
            fingerprint: x.fingerprint,
            rule: x.rule,
            by: x.created_by.unwrap_or_else(|| "a deleted user".into()),
            reason: x.reason,
            expires: x.expires_at.into(),
        })
        .collect();
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&AllowPage {
        header: RepoHeader::load(state, a, subject, "push").await,
        fingerprint,
        rule,
        days: ALLOWANCE_DAYS.to_vec(),
        message,
        errors,
        allowances,
        push,
        ctx,
    });
    *res.status_mut() = status;
    Ok(res)
}

async fn show_allow(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo)): Path<(String, String)>,
    Query(q): Query<AllowQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let a = access(&state, &subject, &ns, &repo, Action::RepoPush).await?;
    let message = match q.status.as_deref() {
        Some("allowed") => {
            Some("Allowed. Push again; the finding passes until the allowance expires.")
        }
        Some("accepted") => Some("The first push's findings are allowed for a day. Push again."),
        Some("allowed-push") => Some(
            "The push's findings are allowed. Push again; they pass until the allowances expire.",
        ),
        _ => None,
    };
    let push = q.push.as_deref().and_then(|p| p.parse().ok());
    allow_page(
        &state,
        ctx,
        &subject,
        &a,
        (q.fingerprint, q.rule, push),
        message,
        Vec::new(),
    )
    .await
}

async fn save_allow(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<AllowForm>,
) -> Result<Response, AppError> {
    let a = access(&state, &s.subject, &ns, &repo, Action::RepoPush).await?;
    let days = f.days.parse().unwrap_or(0);
    match state
        .services
        .allow_secret(&s.subject, &a, &f.fingerprint, &f.rule, days, &f.reason)
        .await
    {
        Ok(()) => Ok(
            Redirect::to(&format!("/{ns}/{repo}/-/secret-scan/allow?status=allowed"))
                .into_response(),
        ),
        Err(ServiceError::Invalid(errors)) => {
            allow_page(
                &state,
                ctx,
                &s.subject,
                &a,
                (f.fingerprint, f.rule, None),
                None,
                errors,
            )
            .await
        }
        Err(e) => Err(e.into()),
    }
}

/// The page for a remembered rejection: a first push (administrators) or
/// a push with several findings (whoever may allow findings).
async fn accept_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    (ns, repo): (&str, &str),
    id: uuid::Uuid,
    errors: Vec<FieldError>,
) -> Result<Response, AppError> {
    let a = access(state, subject, ns, repo, Action::RepoPush).await?;
    let first_push = state
        .services
        .rejection_is_first_push(&a, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    let findings = if first_push {
        let a = access(state, subject, ns, repo, Action::RepoSettings).await?;
        state.services.first_push_findings(subject, &a, id).await?
    } else {
        // Who may not allow findings doesn't learn about them.
        match state.services.push_findings(subject, &a, id).await {
            Err(ServiceError::Forbidden) => return Err(AppError::not_found()),
            other => other?,
        }
    };
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&AcceptPage {
        header: RepoHeader::load(state, &a, subject, "push").await,
        id,
        findings,
        first_push,
        days: ALLOWANCE_DAYS.to_vec(),
        errors,
        ctx,
    });
    *res.status_mut() = status;
    Ok(res)
}

async fn show_accept(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo, id)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let id: uuid::Uuid = id.parse().map_err(|_| AppError::not_found())?;
    accept_page(&state, ctx, &subject, (&ns, &repo), id, Vec::new()).await
}

async fn save_accept(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(s): Sudo,
    Path((ns, repo, id)): Path<(String, String, String)>,
    Form(f): Form<AcceptForm>,
) -> Result<Response, AppError> {
    let id: uuid::Uuid = id.parse().map_err(|_| AppError::not_found())?;
    let a = access(&state, &s.subject, &ns, &repo, Action::RepoPush).await?;
    let first_push = state
        .services
        .rejection_is_first_push(&a, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    if first_push {
        let a = access(&state, &s.subject, &ns, &repo, Action::RepoSettings).await?;
        state.services.accept_first_push(&s.subject, &a, id).await?;
        return Ok(
            Redirect::to(&format!("/{ns}/{repo}/-/secret-scan/allow?status=accepted"))
                .into_response(),
        );
    }
    let days = f.days.parse().unwrap_or(0);
    match state
        .services
        .allow_push(&s.subject, &a, id, days, &f.reason)
        .await
    {
        Ok(_) => Ok(Redirect::to(&format!(
            "/{ns}/{repo}/-/secret-scan/allow?status=allowed-push"
        ))
        .into_response()),
        Err(ServiceError::Invalid(errors)) => {
            accept_page(&state, ctx, &s.subject, (&ns, &repo), id, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as(
            "/{ns}/{repo}/-/secret-scan/allow",
            Access::Resource,
            get(show_allow).post(save_allow),
        )
        .route_as(
            "/{ns}/{repo}/-/secret-scan/accept/{id}",
            Access::Resource,
            get(show_accept).post(save_accept),
        )
}
