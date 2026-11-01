// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Password reset by email and email verification links.

use askama::Template;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::Secret;
use slath_service::recovery::{ResetInfo, Verified};
use slath_service::{FieldError, ServiceError};

use super::settings::error_for;
use super::{PageCtx, render};
use crate::AppState;
use crate::auth::ClientIp;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// Pages carrying a token: not cached, no referrer.
fn private(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    res
}

#[derive(Template)]
#[template(path = "password_reset.html")]
struct ResetPage<'a> {
    ctx: PageCtx,
    sent: bool,
    email: &'a str,
    errors: Vec<FieldError>,
}

impl ResetPage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn reset_page(State(state): State<AppState>, ctx: PageCtx) -> Result<Response, AppError> {
    if !state.services.email_enabled() {
        return Err(AppError::not_found());
    }
    Ok(render(&ResetPage {
        ctx,
        sent: false,
        email: "",
        errors: Vec::new(),
    }))
}

#[derive(Deserialize)]
struct ResetForm {
    /// Exactly one address; repeated fields are rejected by the parser.
    #[serde(default)]
    email: String,
}

async fn reset_request(
    State(state): State<AppState>,
    ctx: PageCtx,
    ip: ClientIp,
    Form(f): Form<ResetForm>,
) -> Result<Response, AppError> {
    let ip = ip.0.map(|i| i.to_string());
    match state
        .services
        .request_password_reset(&f.email, ip.as_deref())
        .await
    {
        Ok(()) => Ok(render(&ResetPage {
            ctx,
            sent: true,
            email: "",
            errors: Vec::new(),
        })),
        Err(ServiceError::Invalid(errors)) => {
            let mut res = render(&ResetPage {
                ctx,
                sent: false,
                email: &f.email,
                errors,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Template)]
#[template(path = "password_reset_confirm.html")]
struct ConfirmPage<'a> {
    ctx: PageCtx,
    token: &'a str,
    info: Option<ResetInfo>,
    errors: Vec<FieldError>,
}

impl ConfirmPage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

#[derive(Deserialize)]
struct TokenQuery {
    #[serde(default)]
    token: String,
}

async fn confirm_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    Query(q): Query<TokenQuery>,
) -> Result<Response, AppError> {
    let info = state.services.reset_info(&q.token).await?;
    let gone = info.is_none();
    let mut res = private(render(&ConfirmPage {
        ctx,
        token: &q.token,
        info,
        errors: Vec::new(),
    }));
    if gone {
        *res.status_mut() = StatusCode::GONE;
    }
    Ok(res)
}

#[derive(Deserialize)]
struct ConfirmForm {
    #[serde(default)]
    token: String,
    #[serde(default)]
    new_password: String,
    #[serde(default)]
    confirm_password: String,
    #[serde(default)]
    code: String,
}

async fn confirm_submit(
    State(state): State<AppState>,
    ctx: PageCtx,
    Form(f): Form<ConfirmForm>,
) -> Result<Response, AppError> {
    match state
        .services
        .reset_password(
            &f.token,
            Secret::new(f.new_password),
            Secret::new(f.confirm_password),
            &f.code,
        )
        .await
    {
        Ok(()) => Ok(Redirect::to("/-/login?status=password-reset").into_response()),
        Err(ServiceError::Invalid(errors)) => {
            let info = state.services.reset_info(&f.token).await?;
            let mut res = private(render(&ConfirmPage {
                ctx,
                token: &f.token,
                info,
                errors,
            }));
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Template)]
#[template(path = "verify_email.html")]
struct VerifyPage<'a> {
    ctx: PageCtx,
    token: &'a str,
    email: Option<String>,
    outcome: Option<&'static str>,
    ok: bool,
}

/// `GET /-/verify-email?token=`: asks for a click, so mail scanners that
/// follow links don't confirm anything.
async fn verify_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    Query(q): Query<TokenQuery>,
) -> Result<Response, AppError> {
    let email = state.services.verification_email(&q.token).await?;
    Ok(private(render(&VerifyPage {
        ctx,
        token: &q.token,
        email,
        outcome: None,
        ok: false,
    })))
}

async fn verify_submit(
    State(state): State<AppState>,
    ctx: PageCtx,
    Form(f): Form<TokenQuery>,
) -> Result<Response, AppError> {
    let (outcome, ok, status) = match state.services.verify_email(&f.token).await? {
        Verified::Done => ("Your email address is confirmed.", true, StatusCode::OK),
        Verified::Stale => (
            "This link has expired or was already used, or the address was changed since.",
            false,
            StatusCode::GONE,
        ),
        Verified::Taken => (
            "Another account already confirmed this address.",
            false,
            StatusCode::CONFLICT,
        ),
    };
    let mut res = private(render(&VerifyPage {
        ctx,
        token: "",
        email: None,
        outcome: Some(outcome),
        ok,
    }));
    *res.status_mut() = status;
    Ok(res)
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as(
            "/-/password/reset",
            Access::Public,
            get(reset_page).post(reset_request),
        )
        .route_as(
            "/-/password/reset/confirm",
            Access::Public,
            get(confirm_page).post(confirm_submit),
        )
        .route_as(
            "/-/verify-email",
            Access::Public,
            get(verify_page).post(verify_submit),
        )
}
