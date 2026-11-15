// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Email address settings: the account's addresses, the primary one
//! marked, and adding, confirming, removing, and choosing the primary one.

use askama::Template;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_db::user_emails::Address;
use slath_service::{FieldError, ServiceError};

use super::settings::{StatusQuery, error_for};
use super::{PageCtx, render};
use crate::AppState;
use crate::auth::{RequireSession, Sudo};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// Success messages after redirects (`?status=`), from a fixed set.
fn status_message(s: Option<&str>) -> Option<&'static str> {
    Some(match s? {
        "added" => "Address added. Open the link we sent to it to confirm it.",
        "added-unconfirmed" => {
            "Address added. Email is off here, so an administrator has to confirm it."
        }
        "sent" => "We sent the confirmation link again.",
        "primary" => "Primary address changed. Password resets and notifications go there now.",
        "removed" => "Address removed.",
        _ => return None,
    })
}

#[derive(Template)]
#[template(path = "settings/emails.html")]
struct EmailsPage {
    ctx: PageCtx,
    tab: &'static str,
    addresses: Vec<Address>,
    email_enabled: bool,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    /// What was entered into the add field.
    email: String,
}

impl EmailsPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn page(
    state: &AppState,
    ctx: PageCtx,
    session: &slath_service::sessions::Session,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    email: String,
) -> Result<Response, AppError> {
    let failed = !errors.is_empty();
    let mut res = render(&EmailsPage {
        ctx,
        tab: "emails",
        addresses: state.services.emails(session).await?,
        email_enabled: state.services.email_enabled(),
        message,
        errors,
        email,
    });
    if failed {
        *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
    }
    Ok(res)
}

async fn show(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    page(
        &state,
        ctx,
        &session,
        status_message(q.status.as_deref()),
        Vec::new(),
        String::new(),
    )
    .await
}

#[derive(Deserialize)]
struct EmailForm {
    #[serde(default)]
    email: String,
}

/// The page again with the service's field errors, or the error itself.
async fn failed(
    state: &AppState,
    ctx: PageCtx,
    session: &slath_service::sessions::Session,
    e: ServiceError,
    email: String,
) -> Result<Response, AppError> {
    match e {
        ServiceError::Invalid(errors) => page(state, ctx, session, None, errors, email).await,
        other => Err(other.into()),
    }
}

async fn add(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Form(f): Form<EmailForm>,
) -> Result<Response, AppError> {
    match state.services.add_email(&session, &f.email).await {
        Ok(()) if state.services.email_enabled() => {
            Ok(Redirect::to("/-/settings/emails?status=added").into_response())
        }
        Ok(()) => Ok(Redirect::to("/-/settings/emails?status=added-unconfirmed").into_response()),
        Err(e) => failed(&state, ctx, &session, e, f.email).await,
    }
}

async fn make_primary(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Form(f): Form<EmailForm>,
) -> Result<Response, AppError> {
    match state.services.make_email_primary(&session, &f.email).await {
        Ok(()) => Ok(Redirect::to("/-/settings/emails?status=primary").into_response()),
        Err(e) => failed(&state, ctx, &session, e, String::new()).await,
    }
}

async fn remove(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Form(f): Form<EmailForm>,
) -> Result<Response, AppError> {
    match state.services.remove_email(&session, &f.email).await {
        Ok(()) => Ok(Redirect::to("/-/settings/emails?status=removed").into_response()),
        Err(e) => failed(&state, ctx, &session, e, String::new()).await,
    }
}

async fn resend(
    State(state): State<AppState>,
    RequireSession(session): RequireSession,
    Form(f): Form<EmailForm>,
) -> Result<Response, AppError> {
    state
        .services
        .resend_email_confirmation(&session, &f.email)
        .await?;
    Ok(Redirect::to("/-/settings/emails?status=sent").into_response())
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as("/-/settings/emails", Access::Account, get(show).post(add))
        .route_as(
            "/-/settings/emails/primary",
            Access::Account,
            post(make_primary),
        )
        .route_as("/-/settings/emails/remove", Access::Account, post(remove))
        .route_as("/-/settings/emails/resend", Access::Account, post(resend))
}
