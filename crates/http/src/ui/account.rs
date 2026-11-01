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

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage<'a> {
    ctx: PageCtx,
    next: &'a str,
    username: &'a str,
    error: Option<&'a str>,
    empty_instance: bool,
}

#[derive(Deserialize)]
pub struct NextQuery {
    next: Option<String>,
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
        error: None,
        empty_instance,
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
        Ok((cookie, _user)) => {
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
    };
    let mut res = render(&LoginPage {
        ctx,
        next: &next,
        username: &form.username,
        error: Some(message),
        empty_instance: false,
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
    let mut res = Redirect::to("/").into_response();
    res.headers_mut()
        .append(header::SET_COOKIE, set_cookie(&state, name, "", None));
    Ok(res)
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route("/-/login", get(login_page).post(login_submit))
        .route("/-/logout", post(logout))
}
