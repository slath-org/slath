// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Passkey endpoints. `…/options` answer JSON for `navigator.credentials`
//! (POST, CSRF-checked); the browser's answer comes back as a normal form
//! post, which redirects like the other sign-in forms.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Json, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_service::ServiceError;

use super::account::mfa_cookie_name;
use crate::AppState;
use crate::auth::{
    ClientIp, RequireSession, Sudo, safe_next, seed_cookie_name, session_cookie_name, session_info,
    set_cookie,
};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

fn options(c: slath_service::passkeys::Ceremony) -> Response {
    let mut res = Json(serde_json::json!({
        "options": c.options,
        "ceremony": c.token.expose(),
    }))
    .into_response();
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    res
}

#[derive(Deserialize)]
struct CeremonyForm {
    #[serde(default)]
    ceremony: String,
    #[serde(default)]
    credential: String,
    #[serde(default)]
    name: String,
    next: Option<String>,
}

/// A form error for passkey pages: back to the page with a status.
fn back(page: &str, status: &str) -> Response {
    Redirect::to(&format!(
        "{page}{}status={status}",
        if page.contains('?') { "&" } else { "?" }
    ))
    .into_response()
}

fn sign_in(
    state: &AppState,
    cookie: &slath_service::sessions::SessionCookie,
    next: &str,
) -> Response {
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
        set_cookie(state, mfa_cookie_name(state), "", None),
    );
    h.append(
        header::SET_COOKIE,
        set_cookie(state, seed_cookie_name(state), "", None),
    );
    res
}

async fn register_options(
    State(state): State<AppState>,
    Sudo(session): Sudo,
) -> Result<Response, AppError> {
    Ok(options(
        state.services.passkey_register_start(&session).await?,
    ))
}

async fn register_finish(
    State(state): State<AppState>,
    Sudo(session): Sudo,
    Form(f): Form<CeremonyForm>,
) -> Result<Response, AppError> {
    match state
        .services
        .passkey_register_finish(&session, &f.ceremony, &f.name, &f.credential)
        .await
    {
        Ok(()) => Ok(back("/-/settings/security", "passkey-added")),
        Err(ServiceError::Invalid(e)) if e.iter().any(|f| f.field == "name") => {
            Ok(back("/-/settings/security", "passkey-name"))
        }
        Err(ServiceError::Invalid(_)) => Ok(back("/-/settings/security", "passkey-failed")),
        Err(e) => Err(e.into()),
    }
}

async fn delete(
    State(state): State<AppState>,
    Sudo(session): Sudo,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state.services.passkey_delete(&session, id).await?;
    Ok(back("/-/settings/security", "passkey-removed"))
}

async fn login_options(State(state): State<AppState>) -> Result<Response, AppError> {
    Ok(options(state.services.passkey_discover_start().await?))
}

async fn login_finish(
    State(state): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    Form(f): Form<CeremonyForm>,
) -> Result<Response, AppError> {
    let next = safe_next(f.next.as_deref());
    match state
        .services
        .passkey_login(&f.ceremony, &f.credential, &session_info(ip, &headers))
        .await
    {
        Ok((cookie, _)) => Ok(sign_in(&state, &cookie, &next)),
        Err(ServiceError::Invalid(_)) => Ok(back(
            &format!(
                "/-/login?next={}",
                percent_encoding::utf8_percent_encode(&next, percent_encoding::NON_ALPHANUMERIC)
            ),
            "passkey-failed",
        )),
        Err(e) => Err(e.into()),
    }
}

async fn second_factor_options(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let token = super::cookie(&headers, mfa_cookie_name(&state)).unwrap_or("");
    let c = state
        .services
        .challenge(token)
        .await?
        .ok_or_else(AppError::unauthenticated)?;
    Ok(options(state.services.passkey_auth_start(c.user_id).await?))
}

async fn second_factor_finish(
    State(state): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    Form(f): Form<CeremonyForm>,
) -> Result<Response, AppError> {
    let next = safe_next(f.next.as_deref());
    let token = super::cookie(&headers, mfa_cookie_name(&state)).unwrap_or("");
    match state
        .services
        .complete_login_with_passkey(
            token,
            &f.ceremony,
            &f.credential,
            &session_info(ip, &headers),
        )
        .await
    {
        Ok((cookie, _)) => Ok(sign_in(&state, &cookie, &next)),
        Err(ServiceError::Invalid(_)) => Ok(back(
            &format!(
                "/-/login/two-factor?next={}",
                percent_encoding::utf8_percent_encode(&next, percent_encoding::NON_ALPHANUMERIC)
            ),
            "passkey-failed",
        )),
        Err(ServiceError::Unauthenticated) => Ok(Redirect::to("/-/login").into_response()),
        Err(e) => Err(e.into()),
    }
}

async fn sudo_options(
    State(state): State<AppState>,
    RequireSession(session): RequireSession,
) -> Result<Response, AppError> {
    Ok(options(
        state.services.passkey_auth_start(session.user_id()).await?,
    ))
}

async fn sudo_finish(
    State(state): State<AppState>,
    RequireSession(session): RequireSession,
    Form(f): Form<CeremonyForm>,
) -> Result<Response, AppError> {
    let next = safe_next(f.next.as_deref());
    match state
        .services
        .sudo_with_passkey(&session, &f.ceremony, &f.credential)
        .await
    {
        Ok(()) => Ok(Redirect::to(&next).into_response()),
        Err(ServiceError::Invalid(_)) => Ok(back(
            &format!(
                "/-/sudo?next={}",
                percent_encoding::utf8_percent_encode(&next, percent_encoding::NON_ALPHANUMERIC)
            ),
            "passkey-failed",
        )),
        Err(e) => Err(e.into()),
    }
}

pub fn router() -> Router<AppState> {
    use axum::routing::post;
    Router::new()
        .route_as(
            "/-/settings/security/passkeys/options",
            Access::Account,
            post(register_options),
        )
        .route_as(
            "/-/settings/security/passkeys",
            Access::Account,
            post(register_finish),
        )
        .route_as(
            "/-/settings/security/passkeys/{id}/delete",
            Access::Account,
            post(delete),
        )
        .route_as(
            "/-/login/passkey/options",
            Access::Public,
            post(login_options),
        )
        .route_as("/-/login/passkey", Access::Public, post(login_finish))
        .route_as(
            "/-/login/two-factor/passkey/options",
            Access::Public,
            post(second_factor_options),
        )
        .route_as(
            "/-/login/two-factor/passkey",
            Access::Public,
            post(second_factor_finish),
        )
        .route_as(
            "/-/sudo/passkey/options",
            Access::Account,
            post(sudo_options),
        )
        .route_as("/-/sudo/passkey", Access::Account, post(sudo_finish))
}
