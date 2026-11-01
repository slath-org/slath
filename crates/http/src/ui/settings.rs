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

/// A token row for the template.
struct TokenView {
    id: String,
    name: String,
    prefix: String,
    scopes: String,
    created: Time,
    last_used: Option<Time>,
    expires: Option<Time>,
    state: &'static str,
}

#[derive(Template)]
#[template(path = "settings/tokens.html")]
struct TokensPage<'a> {
    ctx: PageCtx,
    tab: &'static str,
    tokens: Vec<TokenView>,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
    scopes: Vec<(&'static str, &'static str, bool)>,
    expiry: Vec<(String, String, bool)>,
    name: String,
    /// A token that was just created, shown once.
    created: Option<String>,
}

impl TokensPage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

fn scope_choices(is_admin: bool, chosen: &[String]) -> Vec<(&'static str, &'static str, bool)> {
    slath_core::authz::Scope::ALL
        .iter()
        .filter(|s| is_admin || **s != slath_core::authz::Scope::Admin)
        .map(|s| {
            (
                s.as_str(),
                s.description(),
                chosen.iter().any(|c| c == s.as_str()),
            )
        })
        .collect()
}

fn expiry_choices(chosen: Option<u32>) -> Vec<(String, String, bool)> {
    slath_service::tokens::EXPIRY_CHOICES
        .iter()
        .map(|c| match c {
            Some(d) => (d.to_string(), format!("{d} days"), *c == chosen),
            None => ("never".to_owned(), "No expiry".to_owned(), chosen.is_none()),
        })
        .collect()
}

async fn tokens_view(
    state: &AppState,
    session: &slath_service::sessions::Session,
) -> Result<Vec<TokenView>, AppError> {
    let list = state.services.list_tokens(&session.subject).await?;
    Ok(list
        .into_iter()
        .map(|t| TokenView {
            id: t.id.to_string(),
            state: if t.revoked {
                "Revoked"
            } else if t.expired {
                "Expired"
            } else {
                "Active"
            },
            name: t.name,
            prefix: t.prefix,
            scopes: t
                .scopes
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            created: t.created_at.into(),
            last_used: t.last_used_at.map(Into::into),
            expires: t.expires_at.map(Into::into),
        })
        .collect())
}

async fn tokens_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let tokens = tokens_view(&state, &session).await?;
    Ok(render(&TokensPage {
        ctx,
        tab: "tokens",
        tokens,
        message: status_message(q.status.as_deref()),
        errors: Vec::new(),
        scopes: scope_choices(session.row.is_admin, &["repo:read".to_owned()]),
        expiry: expiry_choices(Some(slath_service::tokens::DEFAULT_EXPIRY_DAYS)),
        name: String::new(),
        created: None,
    }))
}

async fn create_token(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    let pairs = crate::auth::form_pairs(&body);
    let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let name = get("name").unwrap_or_default();
    let scopes: Vec<String> = pairs
        .iter()
        .filter(|(n, _)| n == "scopes")
        .map(|(_, v)| v.clone())
        .collect();
    let expiry = match get("expires").as_deref() {
        Some("never") => None,
        Some(d) => Some(d.parse::<u32>().unwrap_or(0)),
        None => Some(slath_service::tokens::DEFAULT_EXPIRY_DAYS),
    };
    let result = state
        .services
        .create_token(
            &session,
            slath_service::tokens::NewToken {
                name: name.clone(),
                scopes: scopes.clone(),
                expires_in_days: expiry,
                repo_ids: None,
            },
        )
        .await;
    let (created, errors, status) = match result {
        Ok((secret, _)) => (Some(secret.into_inner()), Vec::new(), StatusCode::OK),
        Err(e) => (None, field_errors(e)?, StatusCode::UNPROCESSABLE_ENTITY),
    };
    let ok = created.is_some();
    let tokens = tokens_view(&state, &session).await?;
    let mut res = render(&TokensPage {
        ctx,
        tab: "tokens",
        tokens,
        message: None,
        errors,
        scopes: scope_choices(session.row.is_admin, if ok { &[] } else { &scopes }),
        expiry: expiry_choices(if ok {
            Some(slath_service::tokens::DEFAULT_EXPIRY_DAYS)
        } else {
            expiry
        }),
        name: if ok { String::new() } else { name },
        created,
    });
    *res.status_mut() = status;
    // The token appears in this response only: keep it out of every cache.
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

async fn revoke_token(
    State(state): State<AppState>,
    RequireSession(session): RequireSession,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state.services.revoke_token(&session.subject, id).await?;
    Ok(Redirect::to("/-/settings/tokens?status=token-revoked").into_response())
}

struct KeyView {
    id: String,
    title: String,
    key_type: String,
    fingerprint: String,
    created: Time,
    last_used: Option<Time>,
}

#[derive(Template)]
#[template(path = "settings/keys.html")]
struct KeysPage<'a> {
    ctx: PageCtx,
    tab: &'static str,
    keys: Vec<KeyView>,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
    title: String,
    key: String,
}

impl KeysPage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn keys_view(
    state: &AppState,
    session: &slath_service::sessions::Session,
) -> Result<Vec<KeyView>, AppError> {
    Ok(state
        .services
        .list_keys(&session.subject)
        .await?
        .into_iter()
        .map(|k| KeyView {
            id: k.id.to_string(),
            title: k.title,
            key_type: k.key_type,
            fingerprint: k.fingerprint,
            created: k.created_at.into(),
            last_used: k.last_used_at.map(Into::into),
        })
        .collect())
}

async fn keys_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let keys = keys_view(&state, &session).await?;
    Ok(render(&KeysPage {
        ctx,
        tab: "keys",
        keys,
        message: status_message(q.status.as_deref()),
        errors: Vec::new(),
        title: String::new(),
        key: String::new(),
    }))
}

#[derive(Deserialize)]
struct KeyForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    key: String,
}

async fn add_key(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Form(f): Form<KeyForm>,
) -> Result<Response, AppError> {
    match state
        .services
        .add_key(&session.subject, true, &f.title, &f.key)
        .await
    {
        Ok(_) => Ok(Redirect::to("/-/settings/keys?status=key-added").into_response()),
        Err(e) => {
            let errors = field_errors(e)?;
            let keys = keys_view(&state, &session).await?;
            let mut res = render(&KeysPage {
                ctx,
                tab: "keys",
                keys,
                message: None,
                errors,
                title: f.title,
                key: f.key,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
    }
}

async fn remove_key(
    State(state): State<AppState>,
    Sudo(session): Sudo,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state
        .services
        .remove_key(&session.subject, true, id)
        .await?;
    Ok(Redirect::to("/-/settings/keys?status=key-removed").into_response())
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
        .route("/-/settings/keys", get(keys_page).post(add_key))
        .route("/-/settings/keys/{id}/delete", post(remove_key))
        .route("/-/settings/tokens", get(tokens_page).post(create_token))
        .route("/-/settings/tokens/{id}/revoke", post(revoke_token))
        .route("/-/sudo", get(sudo_page).post(sudo_submit))
        .route("/-/avatars/{file}", get(avatar))
}
