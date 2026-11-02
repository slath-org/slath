// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sign-in and sign-out.

use askama::Template;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::Secret;
use slath_service::accounts::LoginError;

use super::{PageCtx, render};
use crate::AppState;
use crate::auth::{
    ClientIp, MaybeSession, safe_next, seed_cookie_name, session_cookie_name, session_info,
    set_cookie,
};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage<'a> {
    ctx: PageCtx,
    next: &'a str,
    username: &'a str,
    error: Option<&'a str>,
    empty_instance: bool,
    /// Email is on: offer the password reset.
    can_reset: bool,
    /// Registration is open: offer it.
    can_register: bool,
    /// Passkeys work here: offer passwordless sign-in.
    passkeys: bool,
    notice: Option<&'static str>,
    /// OpenID providers as `(key, name)`.
    providers: Vec<(String, String)>,
}

#[derive(Deserialize)]
pub struct NextQuery {
    next: Option<String>,
    status: Option<String>,
}

/// `GET /-/login`.
pub async fn login_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Query(q): Query<NextQuery>,
) -> Result<Response, AppError> {
    let next = safe_next(q.next.as_deref());
    if session.0.is_some() {
        return Ok(Redirect::to(&next).into_response());
    }
    let empty_instance = state
        .services
        .is_empty_instance()
        .await
        .map_err(|e| AppError::internal(&e))?;
    Ok(render(&LoginPage {
        ctx,
        next: &next,
        username: "",
        error: (q.status.as_deref() == Some("passkey-failed")).then_some(
            "The passkey couldn't be verified. Try again, or sign in with your password.",
        ),
        empty_instance,
        can_reset: state.services.email_enabled(),
        can_register: state.config.auth.registration == slath_core::config::Registration::Open,
        passkeys: state.services.passkeys_available(),
        providers: state.services.oidc_providers(),
        notice: match q.status.as_deref() {
            Some("password-reset") => Some("Your password was changed. Sign in with it now."),
            Some("signed-out") => {
                Some("You're signed out. Text you hadn't sent yet was removed from this browser.")
            }
            _ => None,
        },
    }))
}

#[derive(Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    next: Option<String>,
}

/// `POST /-/login`.
pub async fn login_submit(
    State(state): State<AppState>,
    ctx: PageCtx,
    ip: ClientIp,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let next = safe_next(form.next.as_deref());
    let result = state
        .services
        .login(
            &form.username,
            Secret::new(form.password),
            session_info(ip, &headers),
        )
        .await;
    let (status, message) = match result {
        Ok(slath_service::mfa::SignIn::SecondFactor(challenge)) => {
            let max_age = (challenge.expires_at - time::OffsetDateTime::now_utc()).whole_seconds();
            let mut res = Redirect::to(&format!(
                "/-/login/two-factor?next={}",
                percent_encoding::utf8_percent_encode(&next, percent_encoding::NON_ALPHANUMERIC)
            ))
            .into_response();
            res.headers_mut().append(
                header::SET_COOKIE,
                set_cookie(
                    &state,
                    mfa_cookie_name(&state),
                    challenge.token.expose(),
                    Some(max_age),
                ),
            );
            return Ok(res);
        }
        Ok(slath_service::mfa::SignIn::Session(cookie, _user)) => {
            let max_age = (cookie.expires_at - time::OffsetDateTime::now_utc()).whole_seconds();
            let mut res = Redirect::to(&next).into_response();
            let h = res.headers_mut();
            h.append(
                header::SET_COOKIE,
                set_cookie(
                    &state,
                    session_cookie_name(&state),
                    cookie.value.expose(),
                    Some(max_age),
                ),
            );
            h.append(
                header::SET_COOKIE,
                set_cookie(&state, seed_cookie_name(&state), "", None),
            );
            return Ok(res);
        }
        Err(LoginError::Invalid) => (StatusCode::UNAUTHORIZED, "Incorrect username or password."),
        Err(LoginError::Throttled) => (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed sign-in attempts. Wait 15 minutes and try again.",
        ),
        Err(LoginError::Internal(e)) => return Err(AppError::internal(&e)),
        Err(LoginError::Expired | LoginError::CodeRequired) => {
            (StatusCode::UNAUTHORIZED, "Incorrect username or password.")
        }
    };
    let mut res = render(&LoginPage {
        ctx,
        next: &next,
        username: &form.username,
        error: Some(message),
        empty_instance: false,
        can_reset: state.services.email_enabled(),
        can_register: state.config.auth.registration == slath_core::config::Registration::Open,
        passkeys: state.services.passkeys_available(),
        providers: state.services.oidc_providers(),
        notice: None,
    });
    *res.status_mut() = status;
    Ok(res)
}

/// Name of the cookie holding a pending second-factor step.
#[must_use]
pub fn mfa_cookie_name(state: &AppState) -> &'static str {
    if state.config.server.public_url.is_https() {
        "__Host-slath_mfa"
    } else {
        "slath_mfa"
    }
}

#[derive(Template)]
#[template(path = "login_mfa.html")]
struct MfaPage<'a> {
    ctx: PageCtx,
    next: &'a str,
    error: Option<&'a str>,
    /// The account has passkeys.
    passkey: bool,
}

/// `GET /-/login/two-factor`: the code step of a sign-in.
pub async fn mfa_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    headers: HeaderMap,
    Query(q): Query<NextQuery>,
) -> Result<Response, AppError> {
    let next = safe_next(q.next.as_deref());
    let challenge = match super::cookie(&headers, mfa_cookie_name(&state)) {
        Some(c) => state.services.challenge(c).await?,
        None => None,
    };
    let Some(challenge) = challenge else {
        return Ok(Redirect::to(&format!(
            "/-/login?next={}",
            percent_encoding::utf8_percent_encode(&next, percent_encoding::NON_ALPHANUMERIC)
        ))
        .into_response());
    };
    let passkey = state.services.passkeys_available()
        && !state
            .services
            .passkey_list(challenge.user_id)
            .await?
            .is_empty();
    Ok(render(&MfaPage {
        ctx,
        next: &next,
        error: (q.status.as_deref() == Some("passkey-failed"))
            .then_some("The passkey couldn't be verified. Try again, or enter a code."),
        passkey,
    }))
}

#[derive(Deserialize)]
pub struct MfaForm {
    #[serde(default)]
    code: String,
    next: Option<String>,
}

/// `POST /-/login/two-factor`.
pub async fn mfa_submit(
    State(state): State<AppState>,
    ctx: PageCtx,
    ip: ClientIp,
    headers: HeaderMap,
    Form(form): Form<MfaForm>,
) -> Result<Response, AppError> {
    let next = safe_next(form.next.as_deref());
    let token = super::cookie(&headers, mfa_cookie_name(&state)).unwrap_or("");
    let (status, message) = match state
        .services
        .complete_login(token, &form.code, &session_info(ip, &headers))
        .await
    {
        Ok((cookie, _)) => {
            let max_age = (cookie.expires_at - time::OffsetDateTime::now_utc()).whole_seconds();
            let mut res = Redirect::to(&next).into_response();
            let h = res.headers_mut();
            h.append(
                header::SET_COOKIE,
                set_cookie(
                    &state,
                    session_cookie_name(&state),
                    cookie.value.expose(),
                    Some(max_age),
                ),
            );
            h.append(
                header::SET_COOKIE,
                set_cookie(&state, mfa_cookie_name(&state), "", None),
            );
            h.append(
                header::SET_COOKIE,
                set_cookie(&state, seed_cookie_name(&state), "", None),
            );
            return Ok(res);
        }
        Err(LoginError::Expired) => {
            let mut res = Redirect::to(&format!(
                "/-/login?next={}",
                percent_encoding::utf8_percent_encode(&next, percent_encoding::NON_ALPHANUMERIC)
            ))
            .into_response();
            res.headers_mut().append(
                header::SET_COOKIE,
                set_cookie(&state, mfa_cookie_name(&state), "", None),
            );
            return Ok(res);
        }
        Err(LoginError::Invalid | LoginError::CodeRequired) => (
            StatusCode::UNAUTHORIZED,
            "That code isn't right. Enter the current code from your app, or a recovery code.",
        ),
        Err(LoginError::Throttled) => (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed attempts. Wait 15 minutes and try again.",
        ),
        Err(LoginError::Internal(e)) => return Err(AppError::internal(&e)),
    };
    let passkey = match super::cookie(&headers, mfa_cookie_name(&state)) {
        Some(c) => match state.services.challenge(c).await? {
            Some(ch) => {
                state.services.passkeys_available()
                    && !state.services.passkey_list(ch.user_id).await?.is_empty()
            }
            None => false,
        },
        None => false,
    };
    let mut res = render(&MfaPage {
        ctx,
        next: &next,
        error: Some(message),
        passkey,
    });
    *res.status_mut() = status;
    Ok(res)
}

/// `POST /-/logout`: deletes the session and its cookie.
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let name = session_cookie_name(&state);
    if let Some(c) = super::cookie(&headers, name) {
        state
            .services
            .logout(c)
            .await
            .map_err(|e| AppError::internal(&e))?;
    }
    // The sign-in page says that unsent editor drafts were removed (the
    // page's script removes them when the sign-out form is sent).
    let mut res = Redirect::to("/-/login?status=signed-out").into_response();
    res.headers_mut()
        .append(header::SET_COOKIE, set_cookie(&state, name, "", None));
    Ok(res)
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as(
            "/-/login",
            Access::Public,
            get(login_page).post(login_submit),
        )
        .route_as(
            "/-/login/two-factor",
            Access::Public,
            get(mfa_page).post(mfa_submit),
        )
        .route_as("/-/logout", Access::Public, post(logout))
}
