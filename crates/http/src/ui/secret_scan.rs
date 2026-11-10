// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Push protection pages behind the links in a rejected push: allowing one
//! finding (repository writers), and accepting all findings of a first push
//! into an empty repository (repository administrators).

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
}

#[derive(Deserialize, Default)]
struct AllowQuery {
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    rule: String,
    status: Option<String>,
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
    (fingerprint, rule): (String, String),
    message: Option<&'static str>,
    errors: Vec<FieldError>,
) -> Result<Response, AppError> {
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
        _ => None,
    };
    allow_page(
        &state,
        ctx,
        &subject,
        &a,
        (q.fingerprint, q.rule),
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
                (f.fingerprint, f.rule),
                None,
                errors,
            )
            .await
        }
        Err(e) => Err(e.into()),
    }
}

async fn show_accept(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo, id)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let id: uuid::Uuid = id.parse().map_err(|_| AppError::not_found())?;
    let a = access(&state, &subject, &ns, &repo, Action::RepoSettings).await?;
    let findings = state.services.first_push_findings(&subject, &a, id).await?;
    Ok(render(&AcceptPage {
        header: RepoHeader::load(&state, &a, &subject, "push").await,
        id,
        findings,
        ctx,
    }))
}

async fn save_accept(
    State(state): State<AppState>,
    Sudo(s): Sudo,
    Path((ns, repo, id)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let id: uuid::Uuid = id.parse().map_err(|_| AppError::not_found())?;
    let a = access(&state, &s.subject, &ns, &repo, Action::RepoSettings).await?;
    state.services.accept_first_push(&s.subject, &a, id).await?;
    Ok(Redirect::to(&format!("/{ns}/{repo}/-/secret-scan/allow?status=accepted")).into_response())
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
