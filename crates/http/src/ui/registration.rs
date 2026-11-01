// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Registration (`/-/register`) and the administrators' invitations.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::Secret;
use slath_core::config::Registration;
use slath_service::registration::{Gate, INVITE_DAYS, RegisterInput};
use slath_service::{FieldError, ServiceError};

use super::settings::error_for;
use super::{PageCtx, Time, render};
use crate::AppState;
use crate::auth::{
    ClientIp, MaybeSession, Sudo, seed_cookie_name, session_cookie_name, session_info, set_cookie,
};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

#[derive(Template)]
#[template(path = "register.html")]
struct RegisterPage<'a> {
    ctx: PageCtx,
    invite: Option<&'a str>,
    username: &'a str,
    email: &'a str,
    /// The address is fixed by the invitation.
    bound: bool,
    /// The address will need confirming.
    confirm: bool,
    errors: Vec<FieldError>,
}

impl RegisterPage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

#[derive(Deserialize)]
struct InviteQuery {
    invite: Option<String>,
}

async fn register_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Query(q): Query<InviteQuery>,
) -> Result<Response, AppError> {
    let gate = state
        .services
        .registration_gate(q.invite.as_deref())
        .await?;
    if session.0.is_some() {
        return Ok(Redirect::to("/").into_response());
    }
    let bound = match &gate {
        Gate::Invited(Some(e)) => Some(e.clone()),
        _ => None,
    };
    Ok(render(&RegisterPage {
        ctx,
        invite: q.invite.as_deref(),
        username: "",
        email: bound.as_deref().unwrap_or(""),
        bound: bound.is_some(),
        confirm: state.services.email_enabled() && bound.is_none(),
        errors: Vec::new(),
    }))
}

#[derive(Deserialize)]
struct RegisterForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    confirm_password: String,
    invite: Option<String>,
}

async fn register_submit(
    State(state): State<AppState>,
    ctx: PageCtx,
    ip: ClientIp,
    headers: HeaderMap,
    Form(f): Form<RegisterForm>,
) -> Result<Response, AppError> {
    let invite = f.invite.clone().filter(|i| !i.is_empty());
    let addr = ip.0.map(|i| i.to_string());
    let result = state
        .services
        .register(
            RegisterInput {
                username: f.username.clone(),
                email: f.email.clone(),
                password: Secret::new(f.password),
                confirm: Secret::new(f.confirm_password),
                invite: invite.clone(),
            },
            addr.as_deref(),
            &session_info(ip, &headers),
        )
        .await;
    match result {
        Ok((cookie, _)) => {
            let max_age = (cookie.expires_at - time::OffsetDateTime::now_utc()).whole_seconds();
            let mut res = Redirect::to("/").into_response();
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
            Ok(res)
        }
        Err(ServiceError::Invalid(errors)) => {
            let bound = matches!(
                state.services.registration_gate(invite.as_deref()).await,
                Ok(Gate::Invited(Some(_)))
            );
            let mut res = render(&RegisterPage {
                ctx,
                invite: invite.as_deref(),
                username: &f.username,
                email: &f.email,
                bound,
                confirm: state.services.email_enabled() && !bound,
                errors,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        Err(e) => Err(e.into()),
    }
}

struct InviteRow {
    id: String,
    created: Time,
    by: Option<String>,
    target: String,
    state: String,
    revocable: bool,
}

#[derive(Template)]
#[template(path = "admin/invites.html")]
struct InvitesPage {
    ctx: PageCtx,
    section: &'static str,
    invites: Vec<InviteRow>,
    link: Option<String>,
    days: i64,
    message: Option<&'static str>,
    invite_mode: bool,
    mode: &'static str,
    email_enabled: bool,
    errors: Vec<FieldError>,
}

async fn invites_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &slath_core::authz::Subject,
    link: Option<String>,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
) -> Result<Response, AppError> {
    let now = time::OffsetDateTime::now_utc();
    let invites = state
        .services
        .list_invitations(subject)
        .await?
        .into_iter()
        .map(|i| {
            let state = if i.revoked_at.is_some() {
                "revoked".to_owned()
            } else if let Some(u) = &i.used_by {
                format!("used by {u}")
            } else if i.used_at.is_some() {
                "used".to_owned()
            } else if i.expires_at <= now {
                "expired".to_owned()
            } else {
                "open".to_owned()
            };
            InviteRow {
                id: i.id.to_string(),
                created: i.created_at.into(),
                by: i.created_by,
                target: match (i.email, i.note) {
                    (Some(e), Some(n)) => format!("{e} ({n})"),
                    (Some(e), None) => e,
                    (None, Some(n)) => n,
                    (None, None) => "anyone with the link".to_owned(),
                },
                revocable: state == "open",
                state,
            }
        })
        .collect();
    let mode = state.config.auth.registration;
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&InvitesPage {
        ctx,
        section: "invites",
        invites,
        link,
        days: INVITE_DAYS,
        message,
        invite_mode: mode != Registration::Disabled,
        mode: match mode {
            Registration::Disabled => "disabled",
            Registration::Invite => "invite",
            Registration::Open => "open",
        },
        email_enabled: state.services.email_enabled(),
        errors,
    });
    *res.status_mut() = status;
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(res)
}

#[derive(Deserialize, Default)]
struct StatusQuery {
    status: Option<String>,
}

async fn invites(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let message = match q.status.as_deref() {
        Some("revoked") => Some("Invitation revoked."),
        _ => None,
    };
    invites_page(&state, ctx, &session.subject(), None, message, Vec::new()).await
}

#[derive(Deserialize)]
struct InviteForm {
    #[serde(default)]
    email: String,
    #[serde(default)]
    note: String,
}

async fn create_invite(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Form(f): Form<InviteForm>,
) -> Result<Response, AppError> {
    let subject = session.subject.clone();
    match state
        .services
        .create_invitation(&subject, Some(&f.email), Some(&f.note))
        .await
    {
        Ok(link) => {
            invites_page(
                &state,
                ctx,
                &subject,
                Some(link.into_inner()),
                None,
                Vec::new(),
            )
            .await
        }
        Err(ServiceError::Invalid(errors)) => {
            invites_page(&state, ctx, &subject, None, None, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

async fn revoke_invite(
    State(state): State<AppState>,
    Sudo(session): Sudo,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state
        .services
        .revoke_invitation(&session.subject, id)
        .await?;
    Ok(Redirect::to("/-/admin/invites?status=revoked").into_response())
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as(
            "/-/register",
            Access::Public,
            get(register_page).post(register_submit),
        )
        .route_as(
            "/-/admin/invites",
            Access::SiteAdmin,
            get(invites).post(create_invite),
        )
        .route_as(
            "/-/admin/invites/{id}/revoke",
            Access::SiteAdmin,
            post(revoke_invite),
        )
}
