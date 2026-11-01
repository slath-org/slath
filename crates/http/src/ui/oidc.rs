// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Signing in with OpenID Connect providers, first-sign-in registration,
//! and linking providers from the security settings.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Form, Router};
use serde::Deserialize;
use slath_service::ServiceError;
use slath_service::mfa::SignIn;
use slath_service::oidc::OidcResult;

use super::{PageCtx, cookie, render};
use crate::AppState;
use crate::auth::{
    ClientIp, CsrfSeed, MaybeSession, RequireSession, safe_next, seed_cookie_name,
    session_cookie_name, session_info, set_cookie,
};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

fn pending_cookie_name(state: &AppState) -> &'static str {
    if state.config.server.public_url.is_https() {
        "__Host-slath_oidc"
    } else {
        "slath_oidc"
    }
}

#[derive(Template)]
#[template(path = "oidc_message.html")]
struct MessagePage {
    ctx: PageCtx,
    heading: &'static str,
    message: String,
    error: bool,
}

fn message(
    ctx: PageCtx,
    status: StatusCode,
    heading: &'static str,
    message: String,
    error: bool,
) -> Response {
    let mut res = render(&MessagePage {
        ctx,
        heading,
        message,
        error,
    });
    *res.status_mut() = status;
    res
}

/// Turns a finished sign-in into cookies and a redirect.
fn signed_in(state: &AppState, sign_in: SignIn, next: &str) -> Response {
    match sign_in {
        SignIn::Session(cookie, _) => {
            let max_age = (cookie.expires_at - time::OffsetDateTime::now_utc()).whole_seconds();
            let mut res = Redirect::to(next).into_response();
            let h = res.headers_mut();
            h.append(
                header::SET_COOKIE,
                set_cookie(
                    state,
                    session_cookie_name(state),
                    cookie.value.expose(),
                    Some(max_age),
                ),
            );
            h.append(
                header::SET_COOKIE,
                set_cookie(state, seed_cookie_name(state), "", None),
            );
            h.append(
                header::SET_COOKIE,
                set_cookie(state, pending_cookie_name(state), "", None),
            );
            res
        }
        SignIn::SecondFactor(challenge) => {
            let max_age = (challenge.expires_at - time::OffsetDateTime::now_utc()).whole_seconds();
            let mut res = Redirect::to(&format!(
                "/-/login/two-factor?next={}",
                percent_encoding::utf8_percent_encode(next, percent_encoding::NON_ALPHANUMERIC)
            ))
            .into_response();
            res.headers_mut().append(
                header::SET_COOKIE,
                set_cookie(
                    state,
                    super::account::mfa_cookie_name(state),
                    challenge.token.expose(),
                    Some(max_age),
                ),
            );
            res
        }
    }
}

#[derive(Deserialize)]
struct StartQuery {
    next: Option<String>,
}

/// `GET /-/login/oidc/{key}`: off to the provider.
async fn start(
    State(state): State<AppState>,
    Extension(seed): Extension<CsrfSeed>,
    Path(key): Path<String>,
    Query(q): Query<StartQuery>,
) -> Result<Response, AppError> {
    let next = safe_next(q.next.as_deref());
    let url = state
        .services
        .oidc_begin(&key, &seed.value, None, Some(next))
        .await?;
    let mut res = Redirect::to(&url).into_response();
    // The flow is bound to this cookie (SameSite=Lax, so the provider's
    // redirect back brings it along).
    if seed.fresh {
        res.headers_mut().append(
            header::SET_COOKIE,
            set_cookie(
                &state,
                seed_cookie_name(&state),
                &seed.value,
                Some(7 * 86_400),
            ),
        );
    }
    Ok(res)
}

/// `POST /-/settings/oidc/{key}/link` (sudo mode).
async fn link(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Extension(seed): Extension<CsrfSeed>,
    Path(key): Path<String>,
) -> Result<Response, AppError> {
    match state
        .services
        .oidc_begin(
            &key,
            &seed.value,
            Some(&s),
            Some("/-/settings/security".to_owned()),
        )
        .await
    {
        Ok(url) => Ok(Redirect::to(&url).into_response()),
        Err(ServiceError::SudoRequired) => {
            Ok(Redirect::to("/-/sudo?next=/-/settings/security").into_response())
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct UnlinkForm {
    issuer: String,
    subject: String,
}

async fn unlink(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Form(f): Form<UnlinkForm>,
) -> Result<Response, AppError> {
    match state.services.oidc_unlink(&s, &f.issuer, &f.subject).await {
        Ok(()) => Ok(Redirect::to("/-/settings/security?status=oidc-unlinked").into_response()),
        Err(ServiceError::SudoRequired) => {
            Ok(Redirect::to("/-/sudo?next=/-/settings/security").into_response())
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
    error: Option<String>,
}

/// `GET /-/login/oidc/{key}/callback`.
#[allow(clippy::too_many_arguments)]
async fn callback(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Extension(seed): Extension<CsrfSeed>,
    ip: ClientIp,
    headers: HeaderMap,
    Path(key): Path<String>,
    Query(q): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    if q.error.is_some() || q.code.is_empty() || q.state.is_empty() {
        return Ok(message(
            ctx,
            StatusCode::BAD_REQUEST,
            "Sign-in canceled",
            "The provider didn't sign you in. Try again.".to_owned(),
            true,
        ));
    }
    let info = session_info(ip, &headers);
    let result = state
        .services
        .oidc_finish(
            &key,
            &q.state,
            &q.code,
            &seed.value,
            session.0.as_ref(),
            &info,
        )
        .await;
    let (outcome, next) = match result {
        Ok(r) => r,
        Err(ServiceError::Invalid(errors)) => {
            let text = errors
                .first()
                .map_or_else(|| "The sign-in failed.".to_owned(), |e| e.message.clone());
            return Ok(message(
                ctx,
                StatusCode::BAD_REQUEST,
                "Sign-in failed",
                text,
                true,
            ));
        }
        Err(e) => return Err(e.into()),
    };
    let next = safe_next(next.as_deref());
    Ok(match outcome {
        OidcResult::SignedIn(s) => signed_in(&state, s, &next),
        OidcResult::Linked => {
            Redirect::to("/-/settings/security?status=oidc-linked").into_response()
        }
        OidcResult::Register { token, .. } => {
            let mut res = Redirect::to("/-/login/oidc/register").into_response();
            res.headers_mut().append(
                header::SET_COOKIE,
                set_cookie(
                    &state,
                    pending_cookie_name(&state),
                    token.expose(),
                    Some(900),
                ),
            );
            res
        }
        OidcResult::NoAccount {
            provider,
            email_match,
        } => message(
            ctx,
            StatusCode::FORBIDDEN,
            "No account for this sign-in",
            if email_match {
                format!(
                    "An account here uses the same email address. Sign in to it with your password, then link {provider} under Settings → Security."
                )
            } else {
                format!(
                    "No account here is linked to this {provider} sign-in. If you have one, sign in with your password and link {provider} under Settings → Security."
                )
            },
            true,
        ),
        OidcResult::Disabled => message(
            ctx,
            StatusCode::FORBIDDEN,
            "Account disabled",
            "The account linked to this sign-in is disabled.".to_owned(),
            true,
        ),
    })
}

#[derive(Template)]
#[template(path = "oidc_register.html")]
struct RegisterPage {
    ctx: PageCtx,
    provider: String,
    username: String,
    errors: Vec<slath_service::FieldError>,
}

impl RegisterPage {
    fn error(&self, field: &str) -> Option<&str> {
        super::settings::error_for(&self.errors, field)
    }
}

async fn register_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    Extension(seed): Extension<CsrfSeed>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let token = cookie(&headers, pending_cookie_name(&state)).ok_or_else(AppError::not_found)?;
    let (provider, username) = state.services.oidc_pending(token, &seed.value).await?;
    Ok(render(&RegisterPage {
        ctx,
        provider,
        username,
        errors: Vec::new(),
    }))
}

#[derive(Deserialize)]
struct RegisterForm {
    #[serde(default)]
    username: String,
}

async fn register(
    State(state): State<AppState>,
    ctx: PageCtx,
    Extension(seed): Extension<CsrfSeed>,
    ip: ClientIp,
    headers: HeaderMap,
    Form(f): Form<RegisterForm>,
) -> Result<Response, AppError> {
    let token = cookie(&headers, pending_cookie_name(&state))
        .ok_or_else(AppError::not_found)?
        .to_owned();
    let info = session_info(ip, &headers);
    match state
        .services
        .oidc_register(&token, &seed.value, &f.username, &info)
        .await
    {
        Ok(s) => Ok(signed_in(&state, s, "/")),
        Err(ServiceError::Invalid(errors)) => {
            let (provider, _) = state.services.oidc_pending(&token, &seed.value).await?;
            let mut res = render(&RegisterPage {
                ctx,
                provider,
                username: f.username,
                errors,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        Err(e) => Err(e.into()),
    }
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as(
            "/-/login/oidc/register",
            Access::Public,
            get(register_page).post(register),
        )
        .route_as("/-/login/oidc/{key}", Access::Public, get(start))
        .route_as(
            "/-/login/oidc/{key}/callback",
            Access::Public,
            get(callback),
        )
        .route_as("/-/settings/oidc/{key}/link", Access::Account, post(link))
        .route_as("/-/settings/oidc/unlink", Access::Account, post(unlink))
}
