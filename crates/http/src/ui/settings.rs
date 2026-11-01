// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Account settings: profile, password, sessions, and sudo confirmation.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::Secret;
use slath_service::accounts::LoginError;
use slath_service::settings::SessionListItem;
use slath_service::{FieldError, ServiceError};

use super::{PageCtx, Time, render};
use crate::AppState;
use crate::auth::{RequireSession, Sudo, safe_next, session_cookie_name, set_cookie};
use crate::error::AppError;

#[derive(Deserialize, Default)]
pub struct StatusQuery {
    status: Option<String>,
}

/// Success messages after redirects (`?status=`), from a fixed set.
fn status_message(s: Option<&str>) -> Option<&'static str> {
    Some(match s? {
        "profile-saved" => "Profile saved.",
        "password-changed" => "Password changed. Your other sessions were signed out.",
        "session-revoked" => "Session signed out.",
        "key-added" => "SSH key added.",
        "key-removed" => "SSH key removed.",
        "token-revoked" => "Token revoked.",
        _ => return None,
    })
}

fn field_errors(e: ServiceError) -> Result<Vec<FieldError>, AppError> {
    match e {
        ServiceError::Invalid(errors) => Ok(errors),
        other => Err(other.into()),
    }
}

/// The first error message for `field`, for templates.
pub fn error_for<'a>(errors: &'a [FieldError], field: &str) -> Option<&'a str> {
    errors
        .iter()
        .find(|e| e.field == field)
        .map(|e| e.message.as_str())
}

#[derive(Template)]
#[template(path = "settings/profile.html")]
struct ProfilePage<'a> {
    ctx: PageCtx,
    /// Active navigation entry (`profile`, `security`, …).
    tab: &'static str,
    name: &'a str,
    display_name: &'a str,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
}

impl ProfilePage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn profile(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let user = slath_db::users::get(&state.pool, session.user_id())
        .await
        .map_err(|e| AppError::internal(&e))?
        .ok_or_else(AppError::not_found)?;
    Ok(render(&ProfilePage {
        ctx,
        tab: "profile",
        name: &user.name,
        display_name: user.display_name.as_deref().unwrap_or(""),
        message: status_message(q.status.as_deref()),
        errors: Vec::new(),
    }))
}

#[derive(Deserialize)]
struct ProfileForm {
    #[serde(default)]
    display_name: String,
}

async fn profile_save(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Form(f): Form<ProfileForm>,
) -> Result<Response, AppError> {
    match state
        .services
        .update_profile(&session, &f.display_name)
        .await
    {
        Ok(()) => Ok(Redirect::to("/-/settings/profile?status=profile-saved").into_response()),
        Err(e) => {
            let errors = field_errors(e)?;
            let mut res = render(&ProfilePage {
                ctx,
                tab: "profile",
                name: &session.row.user_name,
                display_name: &f.display_name,
                message: None,
                errors,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
    }
}

#[derive(Template)]
#[template(path = "settings/security.html")]
struct SecurityPage<'a> {
    ctx: PageCtx,
    /// Active navigation entry (`profile`, `security`, …).
    tab: &'static str,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
}

impl SecurityPage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn security(ctx: PageCtx, _s: RequireSession, Query(q): Query<StatusQuery>) -> Response {
    render(&SecurityPage {
        ctx,
        tab: "security",
        message: status_message(q.status.as_deref()),
        errors: Vec::new(),
    })
}

#[derive(Deserialize)]
struct PasswordForm {
    #[serde(default)]
    new_password: String,
    #[serde(default)]
    confirm_password: String,
}

async fn change_password(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Form(f): Form<PasswordForm>,
) -> Result<Response, AppError> {
    match state
        .services
        .change_password(
            &session,
            Secret::new(f.new_password),
            Secret::new(f.confirm_password),
        )
        .await
    {
        Ok(_) => Ok(Redirect::to("/-/settings/security?status=password-changed").into_response()),
        Err(e) => {
            let mut res = render(&SecurityPage {
                ctx,
                tab: "security",
                message: None,
                errors: field_errors(e)?,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
    }
}

/// A session row for the template.
struct SessionView {
    id: String,
    current: bool,
    created: Time,
    last_seen: Time,
    ip: String,
    agent: String,
}

impl From<SessionListItem> for SessionView {
    fn from(s: SessionListItem) -> Self {
        Self {
            id: s.id,
            current: s.current,
            created: s.created_at.into(),
            last_seen: s.last_seen_at.into(),
            ip: s.ip.unwrap_or_else(|| "unknown".to_owned()),
            agent: s.user_agent.map_or_else(
                || "unknown browser".to_owned(),
                |a| a.chars().take(120).collect(),
            ),
        }
    }
}

#[derive(Template)]
#[template(path = "settings/sessions.html")]
struct SessionsPage<'a> {
    ctx: PageCtx,
    /// Active navigation entry (`profile`, `security`, …).
    tab: &'static str,
    sessions: Vec<SessionView>,
    message: Option<&'a str>,
}

async fn sessions(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let list = state.services.list_sessions(&session).await?;
    Ok(render(&SessionsPage {
        ctx,
        tab: "sessions",
        sessions: list.into_iter().map(Into::into).collect(),
        message: status_message(q.status.as_deref()),
    }))
}

async fn revoke_session(
    State(state): State<AppState>,
    RequireSession(session): RequireSession,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let own = state
        .services
        .list_sessions(&session)
        .await?
        .into_iter()
        .any(|s| s.id == id && s.current);
    state.services.revoke_session(&session, &id).await?;
    if own {
        let mut res = Redirect::to("/").into_response();
        res.headers_mut().append(
            header::SET_COOKIE,
            set_cookie(&state, session_cookie_name(&state), "", None),
        );
        return Ok(res);
    }
    Ok(Redirect::to("/-/settings/sessions?status=session-revoked").into_response())
}

async fn revoke_all(
    State(state): State<AppState>,
    RequireSession(session): RequireSession,
) -> Result<Response, AppError> {
    state.services.sign_out_everywhere(&session).await?;
    let mut res = Redirect::to("/-/login").into_response();
    res.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(&state, session_cookie_name(&state), "", None),
    );
    Ok(res)
}

#[derive(Template)]
#[template(path = "sudo.html")]
struct SudoPage<'a> {
    ctx: PageCtx,
    next: &'a str,
    error: Option<&'a str>,
}

#[derive(Deserialize)]
struct NextQuery {
    next: Option<String>,
}

async fn sudo_page(ctx: PageCtx, _s: RequireSession, Query(q): Query<NextQuery>) -> Response {
    let next = safe_next(q.next.as_deref());
    render(&SudoPage {
        ctx,
        next: &next,
        error: None,
    })
}

#[derive(Deserialize)]
struct SudoForm {
    #[serde(default)]
    password: String,
    next: Option<String>,
}

async fn sudo_submit(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Form(f): Form<SudoForm>,
) -> Result<Response, AppError> {
    let next = safe_next(f.next.as_deref());
    let (status, msg) = match state
        .services
        .reauthenticate(&session, Secret::new(f.password))
        .await
    {
        Ok(()) => return Ok(Redirect::to(&next).into_response()),
        Err(LoginError::Invalid) => (StatusCode::UNAUTHORIZED, "Incorrect password."),
        Err(LoginError::Throttled) => (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed attempts. Wait 15 minutes and try again.",
        ),
        Err(LoginError::Internal(e)) => return Err(AppError::internal(&e)),
    };
    let mut res = render(&SudoPage {
        ctx,
        next: &next,
        error: Some(msg),
    });
    *res.status_mut() = status;
    Ok(res)
}

/// `GET /-/avatars/{name}.svg`: a generated identicon. It depends only on the
/// name, so it reveals nothing about whether the account exists.
async fn avatar(Path(file): Path<String>) -> Result<Response, AppError> {
    let name = file.strip_suffix(".svg").ok_or_else(AppError::not_found)?;
    if name.is_empty() || name.len() > 64 {
        return Err(AppError::not_found());
    }
    let svg = super::identicon::svg(&name.to_ascii_lowercase());
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("image/svg+xml"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
            ),
        ],
        svg,
    )
        .into_response())
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route(
            "/-/settings",
            get(|| async { Redirect::to("/-/settings/profile") }),
        )
        .route("/-/settings/profile", get(profile).post(profile_save))
        .route("/-/settings/security", get(security))
        .route("/-/settings/security/password", post(change_password))
        .route("/-/settings/sessions", get(sessions))
        .route("/-/settings/sessions/revoke-all", post(revoke_all))
        .route("/-/settings/sessions/{id}/revoke", post(revoke_session))
        .route("/-/sudo", get(sudo_page).post(sudo_submit))
        .route("/-/avatars/{file}", get(avatar))
}
